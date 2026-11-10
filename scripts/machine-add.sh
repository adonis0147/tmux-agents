#!/usr/bin/env bash
set -euo pipefail

main() {
    destination=${1}

    case "${destination}" in
        ''|-*|*[!a-zA-Z0-9_.@:%+-]*) fail 'invalid SSH destination' ;;
    esac

    [[ "${#destination}" -le 255 ]] || fail 'SSH destination is too long'

    root=$(CDPATH='' cd -- "$(dirname -- "${0}")/.." && pwd)
    tag=$(awk 'NR == 1 { tag = $0 } END { if (NR == 1) print tag; else exit 1 }' \
        "${root}/RELEASE_TAG") || fail 'invalid RELEASE_TAG'

    [[ "${tag}" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || fail 'invalid RELEASE_TAG'
    [[ "${#tag}" -le 128 ]] || fail 'release tag is too long'

    binary=${TMUX_AGENTS_BIN:?TMUX_AGENTS_BIN must point to the local executable}
    [[ -f "${binary}" && -x "${binary}" ]] || fail 'local executable is missing'

    command -v ssh >/dev/null 2>&1 || fail 'OpenSSH is required'
    "${binary}" machine list >/dev/null ||
        fail 'cannot read machine registry; a rebuilt client is required for machine add'

    printf 'Remote machine: %s\nRelease: %s\n\n' "${destination}" "${tag}" >&2

    umask 077
    temporary=$(mktemp -d "${TMPDIR:-/tmp}/tmux-agents-add.XXXXXX")

    trap 'rm -rf -- "${temporary}"' EXIT
    trap 'exit 1' HUP INT TERM

    install_allowed=0
    write_bootstrap probe > "${temporary}/probe.sh"

    printf '[1/5] Connecting to %s over SSH...\n' "${destination}" >&2
    run_ssh probe || fail 'could not check the remote supervisor; no installation was attempted'

    availability=$(awk 'NR == 1 { value = $0 } END { if (NR == 1) print value; else exit 1 }' \
        "${temporary}/probe.out") || fail 'invalid remote installation status'

    case "${availability}" in
        missing)
            printf '\nSupervisor %s is not installed on %s.\nInstall it now? [y/N] ' "${tag}" "${destination}" >&2

            if ! IFS= read -r answer; then
                answer=
            fi

            case "${answer}" in
                [yY]|[yY][eE][sS]) install_allowed=1 ;;
                *)
                    printf 'Setup cancelled.\n' >&2
                    return
                    ;;
            esac
            ;;
        installed)
            printf 'Supervisor %s is already installed; continuing.\n' "${tag}" >&2
            ;;
        *) fail 'invalid remote installation status' ;;
    esac

    write_bootstrap setup > "${temporary}/setup.sh"
    run_ssh setup || fail 'remote setup failed; no machine was registered (remote installation may have completed)'

    printf '[5/5] Verifying status and registering machine locally...\n' >&2
    "${binary}" machine register "${destination}" "${tag}" < "${temporary}/setup.out"

    if [[ -t 0 ]]; then
        printf '\nSetup complete. Press Enter to close. ' >&2
        read -r _ignored || true
    fi
}

write_bootstrap() {
    printf "mode='%s'\ntag='%s'\ninstall_allowed='%s'\n" "${1}" "${tag}" "${install_allowed}"

    cat <<'BOOTSTRAP'
set -euo pipefail
umask 077

if [[ "${mode}" = probe ]]; then
    printf '[2/5] Checking remote prerequisites and installed supervisor...\n' >&2
fi

if ! command -v tmux >/dev/null 2>&1; then
    printf 'tmux-agents: install tmux 3.2 or newer on the remote host first\n' >&2
    exit 1
fi

version=$(tmux -V | awk '{ print $2 }')

if ! awk -v version="${version}" '
    BEGIN {
        split(version, parts, ".")
        exit !(parts[1] > 3 || (parts[1] == 3 && parts[2] + 0 >= 2))
    }
'; then
    printf 'tmux-agents: remote tmux 3.2 or newer is required\n' >&2
    exit 1
fi

root="${HOME}/.local/share/tmux-agents"

[[ ! -L "${root}" && ! -L "${root}/scripts" ]] || {
    printf 'tmux-agents: refusing a symlinked remote installation directory\n' >&2
    exit 1
}

if [[ -e "${root}" ]]; then
    identity=$(LC_ALL=C ls -ldn "${root}" | awk '{ print substr($1, 1, 10) ":" $3 }')

    [[ "${identity}" = "drwx------:$(id -u)" ]] || {
        printf 'tmux-agents: remote installation directory must be owned by you with permissions 0700\n' >&2
        exit 1
    }
fi

case "$(uname -s):$(uname -m)" in
    Darwin:x86_64) target=x86_64-apple-darwin ;;
    Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin ;;
    Linux:x86_64) target=x86_64-unknown-linux-musl ;;
    Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-musl ;;
    *)
        printf 'tmux-agents: unsupported remote platform\n' >&2
        exit 1
        ;;
esac

binary="${root}/bin/${tag}/${target}/tmux-agents"
installed=0

if [[ -f "${binary}" && -x "${binary}" && ! -L "${binary}" ]]; then
    installed=1
fi

if [[ "${mode}" = probe ]]; then
    if [[ "${installed}" = 1 ]]; then
        printf 'installed\n'
    else
        printf 'missing\n'
    fi

    exit 0
fi

if [[ "${installed}" = 1 ]]; then
    printf '[3/5] Using installed supervisor; no download needed.\n' >&2
else
    [[ "${install_allowed}" = 1 ]] || {
        printf 'tmux-agents: supervisor is now missing; run machine add again to approve installation\n' >&2
        exit 1
    }

    mkdir -p "${root}/scripts"
    chmod 700 "${root}" "${root}/scripts"
    staging=$(mktemp -d "${root}/.bootstrap.XXXXXX")

    trap 'rm -rf -- "${staging}"' EXIT
    trap 'exit 1' HUP INT TERM

    cat > "${staging}/install.sh" <<'TMUX_AGENTS_INSTALL_SCRIPT'
BOOTSTRAP
    cat "${root}/scripts/install.sh"
    cat <<'BOOTSTRAP'
TMUX_AGENTS_INSTALL_SCRIPT
    mv -f "${staging}/install.sh" "${root}/scripts/install.sh"

    printf '%s\n' "${tag}" > "${staging}/RELEASE_TAG"
    mv -f "${staging}/RELEASE_TAG" "${root}/RELEASE_TAG"

    unset TMUX_AGENTS_BIN

    printf '[3/5] Installing the supervisor...\n' >&2
    binary=$(TMUX_AGENTS_INSTALL_PROGRESS=1 bash "${root}/scripts/install.sh")
fi

printf '[4/5] Starting or reusing the supervisor and checking status...\n' >&2
"${binary}" ensure
"${binary}" request status
BOOTSTRAP
}

run_ssh() {
    rm -f "${temporary}/ssh-status" || return 1

    # Keep replies bounded and preserve SSH's exit status across the pipeline.
    (
        if ssh -T -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
            -- "${destination}" 'bash -s'; then
            printf '0\n' > "${temporary}/ssh-status"
        else
            printf '1\n' > "${temporary}/ssh-status"
        fi
    ) < "${temporary}/${1}.sh" | head -c 65537 > "${temporary}/${1}.out" || return 1

    IFS= read -r ssh_status < "${temporary}/ssh-status" || return 1
    [[ "${ssh_status}" = 0 ]]
}

fail() {
    printf 'tmux-agents: %s\n' "${*}" >&2
    exit 1
}

[[ "${#}" -eq 1 ]] || fail 'usage: machine-add.sh HOST_OR_USER@HOST'
main "${1}"
