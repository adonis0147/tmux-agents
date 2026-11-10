#!/usr/bin/env bash
set -euo pipefail

binary=$(tmux show-option -gqv @tmux-agents-bin)
TMUX_AGENTS_HOME=$(tmux show-option -gqv @tmux-agents-home)
export TMUX_AGENTS_HOME

"${binary}" ensure
list=$("${binary}" request workspace.list)

if [[ -z "${list}" ]]; then
    printf 'No workspaces. Create one with: tmux machine workspace create NAME\n'
    printf 'Press Enter to close. '
    read -r _ignored
    exit 0
fi

printf '%s\n' "${list}" | awk -F '\t' '{ printf "%d. %s (%s)\n", NR, $2, $3 }'
printf 'Workspace number (empty to cancel): '
read -r selection

[[ -n "${selection}" ]] || exit 0

if [[ ! "${selection}" =~ ^[0-9]+$ ]]; then
    printf 'Invalid selection\n'
    exit 1
fi

id=$(printf '%s\n' "${list}" | awk -F '\t' -v selection="${selection}" 'NR == selection { print $1 }')

if [[ -z "${id}" ]]; then
    printf 'Invalid selection\n'
    exit 1
fi

root=$(tmux show-option -gqv @tmux-agents-root)
bash "${root}/scripts/machine.sh" workspace open "${id}"
