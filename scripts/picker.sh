#!/usr/bin/env bash
set -euo pipefail

main() {
    binary=$(tmux show-option -gqv @tmux-agents-bin)
    TMUX_AGENTS_HOME=$(tmux show-option -gqv @tmux-agents-home)
    export TMUX_AGENTS_HOME

    socket=$(tmux display-message -p '#{socket_path}')
    "${binary}" client ensure "${socket}" >/dev/null
    workspaces=$("${binary}" client workspaces "${socket}")
    machines=$("${binary}" client machines "${socket}")

    printf 'Machines\n'
    printf '%s\n' "${machines}" | awk -F '\t' '
        NF >= 3 {
            printf "  %s [%s] %s\n", $1, $3, $4
            if ($3 != "connected") printf "    Try: tmux machine connect %s\n", $1
        }
    '
    printf '\nWorkspaces\n'

    if [[ -z "${workspaces}" ]]; then
        printf 'No workspaces. Create one with: tmux machine workspace [HOST] create NAME\n'
        pause
        return
    fi

    printf '%s\n' "${workspaces}" | awk -F '\t' '
        { printf "%d. %s / %s [%s] %s windows, %s panes\n", NR, $1, $3, $4, $5, $6 }
    '
    printf 'Workspace number (empty to cancel): '
    read -r selection || return 0

    [[ -n "${selection}" ]] || return 0
    if [[ ! "${selection}" =~ ^[0-9]+$ ]]; then
        printf 'Invalid selection\n'
        return 1
    fi

    row=$(printf '%s\n' "${workspaces}" | awk -F '\t' -v selection="${selection}" 'NR == selection')
    if [[ -z "${row}" ]]; then
        printf 'Invalid selection\n'
        return 1
    fi

    IFS=$'\t' read -r host id _name state _windows _panes <<< "${row}"
    connection=$(printf '%s\n' "${machines}" | awk -F '\t' -v host="${host}" '$1 == host { print $3 }')
    if [[ "${state}" == stale || "${connection}" != connected ]]; then
        printf 'Workspace unavailable (%s, %s). Try: tmux machine connect %s\n' "${state}" "${connection}" "${host}"
        pause
        return
    fi

    # Resolve again in the client; names are display-only and may have changed.
    "${binary}" client open "${socket}" "${host}" "${id}"
}

pause() {
    printf 'Press Enter to close. '
    read -r _ignored || true
}

main
