#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "${0}")" && pwd)
version=$(tmux -V | awk '{print $2}')
if ! awk -v version="${version}" 'BEGIN { split(version, parts, "."); exit !(parts[1] > 3 || (parts[1] == 3 && parts[2] + 0 >= 8)) }'; then
    tmux display-message 'tmux-agents requires tmux 3.8 or newer'
    exit 1
fi

tmux set-option -g @tmux-agents-root "${root}"
tmux set-option -g @tmux-agents-home "${TMUX_AGENTS_HOME:-${HOME}/.local/state/tmux-agents}"
# A separate array slot avoids overwriting any existing machine alias.
existing=$(tmux show-options -s | grep -E '[" ]machine=' || true)
if [ -n "${existing}" ] && ! printf '%s' "${existing}" | grep -q 'tmux-agents-root'; then
    tmux display-message 'tmux-agents: machine alias already exists; refusing to replace it'
    exit 1
fi
# tmux 3.8 run-shell parameters are formats, not shell positional arguments.
# Quote each parameter before it crosses the shell boundary.
slot=$(tmux show-option -sqv 'command-alias[tmux-agents-machine]')
if [ -n "${slot}" ] && ! printf '%s' "${slot}" | grep -q 'tmux-agents-root'; then
    tmux display-message 'tmux-agents: alias slot is occupied; refusing to replace it'
    exit 1
fi
if ! binary=$(sh "${root}/scripts/install.sh"); then
    tmux display-message 'tmux-agents installation failed; run scripts/install.sh in the plugin directory for details'
    exit 1
fi
tmux set-option -g @tmux-agents-bin "${binary}"
tmux set-option -s 'command-alias[tmux-agents-machine]' 'machine=run-shell -E "sh #{q:@tmux-agents-root}/scripts/machine.sh #{q:1} #{q:2} #{q:3} #{q:4} #{q:5}"'
tmux bind-key M run-shell -E 'sh #{q:@tmux-agents-root}/scripts/machine.sh picker'
tmux bind-key a if-shell -F '#{!=:#{@tmux-agents-workspace},}' 'send-keys C-a' 'send-prefix'
