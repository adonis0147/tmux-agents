# tmux-agents

A persistent workspace for terminal coding agents, using tmux's own interface.

## Getting started

Requires Rust 1.89+, tmux, and a Unix-like system. Tested with tmux 3.6.

```sh
cargo install --path .
tmux-agents
```

The first launch opens a shell in your current directory, with no startup menu.
Run `claude`, `codex`, `pi`, or any other program directly in a pane. Use your
normal tmux bindings to create windows, split panes, resize, and navigate.

Detach with tmux's detach binding (normally `Ctrl-b d`). Run `tmux-agents` again
to reconnect to the same live workspace, including its layout and running programs.
Launch from outside any existing tmux session.

After a reboot or tmux server loss, saved sessions, windows, layouts, and working
directories reopen with fresh shells. The last-used session and active panes are
restored; previous programs are **not** restarted.

Remote machines are planned, not yet available.

See [workspace behavior](docs/workspaces.md) for isolation, limitations, and tests.
