#!/bin/sh
set -eu

fail() { printf 'tmux-agents: %s\n' "${*}" >&2; exit 1; }
usage() { fail 'usage: tmux machine start|status|picker|workspace create NAME [ABSOLUTE_DIR]|workspace list|workspace open NAME_OR_ID'; }

binary=$(tmux show-option -gqv @tmux-agents-bin)
TMUX_AGENTS_HOME=$(tmux show-option -gqv @tmux-agents-home)
export TMUX_AGENTS_HOME
[ -x "${binary}" ] || fail "build the executable first: cargo build --release (${binary})"
# Alias templates supply empty strings for omitted parameters.
command=${1:-}
subcommand=${2:-}
name=${3:-}
directory=${4:-}
[ -z "${5:-}" ] || usage

open_workspace() {
    target=$("${binary}" request workspace.resolve "${1}") || exit 1
    socket=$(printf '%s\n' "${target}" | awk 'NR == 1')
    session=$(printf '%s\n' "${target}" | awk 'NR == 2')
    id=$(printf '%s\n' "${target}" | awk 'NR == 3')
    window=$(tmux list-windows -F '#{window_id} #{@tmux-agents-workspace}' | awk -v id="${id}" '$2 == id { print $1; exit }')
    if [ -n "${window}" ]; then
        tmux select-window -t "${window}"
        return
    fi
    # Multiple new-window arguments execute directly, without a shell string.
    window=$(tmux new-window -P -F '#{window_id}' -n "workspace:${1}" env -u TMUX tmux -S "${socket}" attach-session -t "=${session}")
    tmux set-option -w -t "${window}" @tmux-agents-workspace "${id}"
}

case "${command}" in
    start)
        [ -z "${subcommand}${name}${directory}" ] || usage
        "${binary}" ensure
        printf 'tmux-agents supervisor is ready\n'
        ;;
    status)
        [ -z "${subcommand}${name}${directory}" ] || usage
        "${binary}" request status
        ;;
    workspace)
        case "${subcommand}" in
            create)
                [ -n "${name}" ] || usage
                [ -n "${directory}" ] || directory=$(tmux display-message -p '#{pane_current_path}')
                "${binary}" ensure
                "${binary}" request workspace.create "${name}" "${directory}"
                ;;
            list)
                [ -z "${name}${directory}" ] || usage
                "${binary}" ensure
                "${binary}" request workspace.list
                ;;
            open)
                [ -n "${name}" ] && [ -z "${directory}" ] || usage
                "${binary}" ensure
                open_workspace "${name}"
                ;;
            *) usage ;;
        esac
        ;;
    picker)
        [ -z "${subcommand}${name}${directory}" ] || usage
        # The popup is another invocation of this plugin client, not a daemon.
        root=$(tmux show-option -gqv @tmux-agents-root)
        tmux display-popup -E sh "${root}/scripts/picker.sh"
        ;;
    *) usage ;;
esac
