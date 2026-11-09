#!/bin/sh
set -eu

main() {
    if [ -n "${TMUX_AGENTS_BIN:-}" ]; then
        case "${TMUX_AGENTS_BIN}" in /*) ;; *) fail 'TMUX_AGENTS_BIN must be an absolute path' ;; esac
        [ -f "${TMUX_AGENTS_BIN}" ] && [ -x "${TMUX_AGENTS_BIN}" ] || fail "TMUX_AGENTS_BIN is not an executable file: ${TMUX_AGENTS_BIN}"
        printf '%s\n' "${TMUX_AGENTS_BIN}"
        return
    fi

    root=$(CDPATH='' cd -- "$(dirname -- "${0}")/.." && pwd)
    version=$(awk -F '"' '/^version = / { print $2; exit }' "${root}/Cargo.toml")
    printf '%s\n' "${version}" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$' || fail 'invalid release version in Cargo.toml'
    os=$(uname -s)
    arch=$(uname -m)
    case "${os}:${arch}" in
        Darwin:x86_64) target=x86_64-apple-darwin ;;
        Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin ;;
        Linux:x86_64) target=x86_64-unknown-linux-musl ;;
        Linux:aarch64|Linux:arm64) target=aarch64-unknown-linux-musl ;;
        *) fail "unsupported platform: ${os} ${arch}; build from source and set TMUX_AGENTS_BIN" ;;
    esac
    directory="${root}/bin/${version}/${target}"
    binary="${directory}/tmux-agents"
    if [ -f "${binary}" ] && [ -x "${binary}" ] && [ ! -L "${binary}" ]; then
        printf '%s\n' "${binary}"
        return
    fi
    [ ! -d "${binary}" ] || fail "installation destination is a directory: ${binary}"
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
    asset="tmux-agents-v${version}-${target}"
    url="https://github.com/adonis0147/tmux-agents/releases/download/v${version}/${asset}"
    printf 'tmux-agents: installing v%s for %s\n' "${version}" "${target}" >&2
    download "${url}.sha256" "${temporary}/checksum" 1024
    expected=$(awk -v asset="${asset}" 'NR == 1 && NF == 2 && $2 == asset && length($1) == 64 && $1 !~ /[^0-9a-f]/ { hash = $1 } END { if (NR == 1 && hash != "") print hash; else exit 1 }' "${temporary}/checksum") || fail 'invalid release checksum file'
    download "${url}" "${temporary}/tmux-agents" 52428800
    if [ "${checksum}" = sha256sum ]; then
        actual=$(sha256sum "${temporary}/tmux-agents" | awk '{ print $1 }')
    else
        actual=$(shasum -a 256 "${temporary}/tmux-agents" | awk '{ print $1 }')
    fi
    [ "${actual}" = "${expected}" ] || fail 'release checksum mismatch; binary was not installed'
    chmod 700 "${temporary}/tmux-agents"
    # Concurrent installers publish identical verified bytes; staging stays on the same filesystem.
    mv -f "${temporary}/tmux-agents" "${binary}"
    printf '%s\n' "${binary}"
}

fail() {
    printf 'tmux-agents: %s\n' "${*}" >&2
    exit 1
}

download() {
    curl --fail --location --silent --show-error --proto '=https' --proto-redir '=https' --tlsv1.2 \
        --connect-timeout 10 --max-time 120 --retry 2 --max-filesize "${3}" --output "${2}" "${1}" || fail "download failed: ${1}; check that the matching GitHub release has been published"
}

main
