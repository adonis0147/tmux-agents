#!/usr/bin/env bash
set -euo pipefail

main() {
    if [[ -n "${TMUX_AGENTS_BIN:-}" ]]; then
        [[ "${TMUX_AGENTS_BIN}" = /* ]] || fail 'TMUX_AGENTS_BIN must be an absolute path'
        [[ -f "${TMUX_AGENTS_BIN}" && -x "${TMUX_AGENTS_BIN}" ]] ||
            fail "TMUX_AGENTS_BIN is not an executable file: ${TMUX_AGENTS_BIN}"

        printf '%s\n' "${TMUX_AGENTS_BIN}"
        return
    fi

    root=$(CDPATH='' cd -- "$(dirname -- "${0}")/.." && pwd)
    tag=$(awk 'NR == 1 { tag = $0 } END { if (NR == 1) print tag; else exit 1 }' \
        "${root}/RELEASE_TAG") || fail 'RELEASE_TAG must contain exactly one line'

    [[ "${tag}" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || fail 'invalid release tag in RELEASE_TAG'

    os=$(uname -s)
    arch=$(uname -m)

    case "${os}:${arch}" in
        Darwin:x86_64) target=x86_64-apple-darwin ;;
        Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin ;;
        Linux:x86_64) target=x86_64-unknown-linux-musl ;;
        Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-musl ;;
        *) fail "unsupported platform: ${os} ${arch}; build from source and set TMUX_AGENTS_BIN" ;;
    esac

    directory="${root}/bin/${tag}/${target}"
    binary="${directory}/tmux-agents"

    if [[ -f "${binary}" && -x "${binary}" && ! -L "${binary}" ]]; then
        progress "Using installed supervisor ${tag} (${target}); no download needed."
        printf '%s\n' "${binary}"
        return
    fi

    [[ ! -d "${binary}" ]] || fail "installation destination is a directory: ${binary}"
    command -v curl >/dev/null 2>&1 || fail 'curl is required for automatic installation'

    if command -v sha256sum >/dev/null 2>&1; then
        checksum=sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        checksum=shasum
    else
        fail 'sha256sum or shasum is required for automatic installation'
    fi

    umask 077
    mkdir -p "${directory}"
    temporary=$(mktemp -d "${directory}/.install.XXXXXX")

    trap 'rm -rf -- "${temporary}"' EXIT
    trap 'exit 1' HUP INT TERM

    asset="tmux-agents-${tag}-${target}"
    url="https://github.com/adonis0147/tmux-agents/releases/download/${tag}"
    printf 'tmux-agents: installing %s for %s\n' "${tag}" "${target}" >&2

    progress 'Downloading checksum manifest...'
    download "${url}/SHA256SUMS" "${temporary}/checksum" 16384

    expected=$(awk -v asset="${asset}" '
        $2 == asset {
            if (NF != 2 || length($1) != 64 || $1 ~ /[^0-9a-f]/) invalid = 1
            hash = $1
            count++
        }
        END { if (count == 1 && !invalid) print hash; else exit 1 }
    ' "${temporary}/checksum") || fail 'invalid or missing entry in release checksum manifest'

    progress 'Downloading supervisor binary...'
    download "${url}/${asset}" "${temporary}/tmux-agents" 52428800

    progress 'Verifying SHA-256 checksum...'

    if [[ "${checksum}" = sha256sum ]]; then
        actual=$(sha256sum "${temporary}/tmux-agents" | awk '{ print $1 }')
    else
        actual=$(shasum -a 256 "${temporary}/tmux-agents" | awk '{ print $1 }')
    fi

    [[ "${actual}" = "${expected}" ]] || fail 'release checksum mismatch; binary was not installed'
    chmod 700 "${temporary}/tmux-agents"

    # Concurrent installers publish identical verified bytes; staging stays on the same filesystem.
    mv -f "${temporary}/tmux-agents" "${binary}"

    progress 'Installation complete.'
    printf '%s\n' "${binary}"
}

fail() {
    printf 'tmux-agents: %s\n' "${*}" >&2
    exit 1
}

progress() {
    if [[ "${TMUX_AGENTS_INSTALL_PROGRESS:-0}" = 1 ]]; then
        printf 'tmux-agents: %s\n' "${*}" >&2
    fi
}

download() {
    local progress_option=--silent

    if [[ "${TMUX_AGENTS_INSTALL_PROGRESS:-0}" = 1 ]]; then
        progress_option=--progress-bar
    fi

    curl --fail --location "${progress_option}" --show-error --proto '=https' --proto-redir '=https' --tlsv1.2 \
        --connect-timeout 10 --max-time 120 --retry 2 --max-filesize "${3}" --output "${2}" "${1}" ||
        fail "download failed: ${1}; check that the matching GitHub release has been published"
}

main
