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

## Register a remote machine

Install tmux 3.6+ and the current build of `tmux-agents` on both machines, then:

```sh
tmux-agents machine add user@host
# Or use an existing SSH alias:
tmux-agents machine add buildbox
```

Registration checks SSH connectivity and remote dependencies using your existing
SSH configuration. It does not install software or open a remote workspace.

## Switch workspaces

Run `tmux-agents`, then press your tmux prefix followed by **uppercase `W`**
(normally `Ctrl-b`, release, then `Shift-w`). This opens a native tmux menu:

- Choose a registered machine to create, restore, or attach to its workspace over SSH.
- Choose **Sessions on this machine** to open tmux's native session tree.
- Choose **local** from a remote workspace to return without nesting tmux.

Running programs survive switching. Detaching exits to your terminal; the next
launch resumes the last selected machine, falling back locally if unavailable.
There is still no startup menu.

See [switching behavior](docs/switching.md), [remote registration](docs/machines.md),
and [local restoration](docs/workspaces.md) for details and tests.
