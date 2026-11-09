#!/bin/sh
set -eu
binary=$(tmux show-option -gqv @tmux-agents-bin)
TMUX_AGENTS_HOME=$(tmux show-option -gqv @tmux-agents-home)
export TMUX_AGENTS_HOME
"${binary}" ensure
list=$("${binary}" request workspace.list)
if [ -z "${list}" ]; then
    printf 'No workspaces. Create one with: tmux machine workspace create NAME\n'
    printf 'Press Enter to close. '
    read -r ignored
    exit 0
fi
printf '%s\n' "${list}" | awk -F '\t' '{ printf "%d. %s (%s)\n", NR, $2, $3 }'
printf 'Workspace number (empty to cancel): '
read -r selection
[ -n "${selection}" ] || exit 0
case "${selection}" in *[!0-9]*) printf 'Invalid selection\n'; exit 1 ;; esac
id=$(printf '%s\n' "${list}" | awk -F '\t' -v selection="${selection}" 'NR == selection { print $1 }')
[ -n "${id}" ] || { printf 'Invalid selection\n'; exit 1; }
root=$(tmux show-option -gqv @tmux-agents-root)
sh "${root}/scripts/machine.sh" workspace open "${id}"
