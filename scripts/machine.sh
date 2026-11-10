#!/usr/bin/env bash
set -euo pipefail

main() {
    binary=$(tmux show-option -gqv @tmux-agents-bin)
    TMUX_AGENTS_HOME=$(tmux show-option -gqv @tmux-agents-home)
    export TMUX_AGENTS_HOME

    [[ -f "${binary}" && -x "${binary}" ]] ||
        fail "executable missing; reload tmux-agents.tmux to install it (${binary})"

    # Alias templates supply empty strings for omitted parameters.
    command=${1:-}
    subcommand=${2:-}
    name=${3:-}
    directory=${4:-}

    [[ -z "${5:-}" ]] || usage

    case "${command}" in
        add)
            [[ -n "${subcommand}" && -z "${name}${directory}" ]] || usage

            root=$(tmux show-option -gqv @tmux-agents-root)
            tmux display-popup -EE -e "TMUX_AGENTS_BIN=${binary}" -e "TMUX_AGENTS_HOME=${TMUX_AGENTS_HOME}" \
                bash "${root}/scripts/machine-add.sh" "${subcommand}"
            ;;
        list)
            [[ -z "${subcommand}${name}${directory}" ]] || usage

            "${binary}" machine list
            ;;
        start)
            [[ -z "${subcommand}${name}${directory}" ]] || usage

            "${binary}" ensure
            printf 'tmux-agents supervisor is ready\n'
            ;;
        status)
            [[ -z "${subcommand}${name}${directory}" ]] || usage

            "${binary}" request status
            ;;
        workspace)
            case "${subcommand}" in
                create)
                    [[ -n "${name}" ]] || usage
                    [[ -n "${directory}" ]] || directory=$(tmux display-message -p '#{pane_current_path}')

                    "${binary}" ensure
                    "${binary}" request workspace.create "${name}" "${directory}"
                    ;;
                list)
                    [[ -z "${name}${directory}" ]] || usage

                    "${binary}" ensure
                    "${binary}" request workspace.list
                    ;;
                open)
                    [[ -n "${name}" && -z "${directory}" ]] || usage

                    "${binary}" ensure
                    open_workspace "${name}"
                    ;;
                *) usage ;;
            esac
            ;;
        picker)
            [[ -z "${subcommand}${name}${directory}" ]] || usage

            # The popup is another invocation of this plugin client, not a daemon.
            root=$(tmux show-option -gqv @tmux-agents-root)
            tmux display-popup -E bash "${root}/scripts/picker.sh"
            ;;
        *) usage ;;
    esac
}

fail() {
    printf 'tmux-agents: %s\n' "${*}" >&2
    exit 1
}

usage() {
    local message='usage: tmux machine add HOST_OR_USER@HOST|list|start|status|picker|'
    message+='workspace create NAME [ABSOLUTE_DIR]|workspace list|workspace open NAME_OR_ID'

    fail "${message}"
}

open_workspace() {
    local target socket session id window

    target=$("${binary}" request workspace.resolve "${1}") || exit 1
    socket=$(printf '%s\n' "${target}" | awk 'NR == 1')
    session=$(printf '%s\n' "${target}" | awk 'NR == 2')
    id=$(printf '%s\n' "${target}" | awk 'NR == 3')

    window=$(tmux list-windows -F '#{window_id} #{@tmux-agents-workspace}' |
        awk -v id="${id}" '$2 == id && !found { print $1; found = 1 }')

    if [[ -n "${window}" ]]; then
        tmux select-window -t "${window}"
        return
    fi

    # Multiple new-window arguments execute directly, without a shell string.
    window=$(tmux new-window -P -F '#{window_id}' -n "workspace:${1}" \
        env -u TMUX tmux -S "${socket}" attach-session -t "=${session}")
    tmux set-option -w -t "${window}" @tmux-agents-workspace "${id}"
}

[[ "${#}" -gt 0 ]] || usage
main "${@}"
