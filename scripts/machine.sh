#!/usr/bin/env bash
set -euo pipefail

main() {
    binary=$(tmux show-option -gqv @tmux-agents-bin)
    TMUX_AGENTS_HOME=$(tmux show-option -gqv @tmux-agents-home)
    export TMUX_AGENTS_HOME

    [[ -f "${binary}" && -x "${binary}" ]] ||
        fail "executable missing; reload tmux-agents.tmux to install it (${binary})"

    socket=$(tmux display-message -p '#{socket_path}')
    command=${1:-}
    shift

    # Alias templates supply empty strings for omitted parameters, including the guard.
    [[ -z "${5:-}" ]] || usage

    case "${command}" in
        add)
            [[ -n "${1:-}" && -z "${2:-}${3:-}${4:-}" ]] || usage

            root=$(tmux show-option -gqv @tmux-agents-root)
            tmux display-popup -EE -e "TMUX_AGENTS_BIN=${binary}" -e "TMUX_AGENTS_HOME=${TMUX_AGENTS_HOME}" \
                bash "${root}/scripts/machine-add.sh" "${1}"
            ;;
        list)
            [[ -z "${1:-}${2:-}${3:-}${4:-}" ]] || usage

            ensure_client
            "${binary}" client machines "${socket}"
            ;;
        start)
            [[ -z "${1:-}${2:-}${3:-}${4:-}" ]] || usage

            "${binary}" ensure
            ensure_client
            printf 'tmux-agents supervisor and client are ready\n'
            ;;
        status)
            [[ -z "${2:-}${3:-}${4:-}" ]] || usage

            ensure_client
            if [[ -n "${1:-}" ]]; then
                "${binary}" client status "${socket}" "${1}"
            else
                "${binary}" client status "${socket}"
            fi
            ;;
        connect)
            [[ -n "${1:-}" && -z "${2:-}${3:-}${4:-}" ]] || usage

            ensure_client
            tmux display-popup -EE -e "TMUX_AGENTS_BIN=${binary}" -e "TMUX_AGENTS_HOME=${TMUX_AGENTS_HOME}" \
                "${binary}" client connect "${socket}" "${1}"
            ;;
        workspace)
            workspace "${@}"
            ;;
        picker)
            [[ -z "${1:-}${2:-}${3:-}${4:-}" ]] || usage

            root=$(tmux show-option -gqv @tmux-agents-root)
            tmux display-popup -E bash "${root}/scripts/picker.sh"
            ;;
        *) usage ;;
    esac
}

ensure_client() {
    # Startup is silent even when an installer-test binary writes a marker to stdout.
    "${binary}" client ensure "${socket}" >/dev/null
}

workspace() {
    local host method reference directory

    host=local
    case "${1:-}" in
        list|create|open|snapshot|restore) ;;
        *)
            host=${1:-}
            [[ -n "${host}" ]] || usage
            shift
            ;;
    esac

    method=${1:-}
    reference=${2:-}
    directory=${3:-}
    [[ -z "${4:-}" ]] || usage

    case "${method}" in
        list)
            [[ -z "${reference}${directory}" ]] || usage

            ensure_client
            "${binary}" client request "${socket}" "${host}" workspace.list
            ;;
        create)
            [[ -n "${reference}" ]] || usage
            if [[ -z "${directory}" && "${host}" == local ]]; then
                directory=$(tmux display-message -p '#{pane_current_path}')
            fi

            ensure_client
            if [[ -n "${directory}" ]]; then
                "${binary}" client request "${socket}" "${host}" workspace.create "${reference}" "${directory}"
            else
                "${binary}" client request "${socket}" "${host}" workspace.create "${reference}"
            fi
            ;;
        open|snapshot|restore)
            [[ -n "${reference}" && -z "${directory}" ]] || usage

            ensure_client
            if [[ "${method}" == open ]]; then
                "${binary}" client open "${socket}" "${host}" "${reference}"
            else
                "${binary}" client request "${socket}" "${host}" "workspace.${method}" "${reference}"
            fi
            ;;
        *) usage ;;
    esac
}

fail() {
    printf 'tmux-agents: %s\n' "${*}" >&2
    exit 1
}

usage() {
    local message='usage: tmux machine add HOST|list|start|status [HOST]|connect HOST|picker|'
    message+='workspace [HOST] list|create NAME [DIR]|open REF|snapshot REF|restore REF'

    fail "${message}"
}

[[ "${#}" -gt 0 ]] || usage
main "${@}"
