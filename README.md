# tmux-agents

A Rust local prototype for managing workspaces in a dedicated tmux server from your existing tmux session. Your local server keeps its configuration; managed workspaces use a separate configuration and `C-a` prefix.

Requires Unix, Rust/Cargo, and **tmux 3.8 or newer**. Tested on macOS with tmux 3.8.

## Get started

Build from the repository root:

```sh
cargo build --release
```

Load the plugin in your local tmux configuration (replace the absolute path):

```tmux
run-shell '/absolute/repo/tmux-agents.tmux'
```

For an already running local server, load it with:

```sh
tmux run-shell '/absolute/repo/tmux-agents.tmux'
```

The plugin uses `target/release/tmux-agents` relative to the repository, unless `TMUX_AGENTS_BIN` overrides it. `TMUX_AGENTS_HOME` selects an absolute, private state directory; its default is `$HOME/.local/state/tmux-agents`.

From a shell in your local tmux session:

```sh
tmux machine start
tmux machine status
tmux machine workspace create demo /absolute/project/directory
tmux machine workspace list
tmux machine workspace open demo
```

The directory is optional when creating a workspace. Open accepts a workspace name or stable ID. Use your local **prefix, then `M`** (or `tmux machine picker`) to open a shell popup that lists workspaces and reads a numeric selection.

Managed tmux uses `C-a`. While attached to a managed workspace, your local **prefix, then `a`** forwards `C-a` to it; your local prefix itself is unchanged. The plugin replaces existing prefix bindings on `M` and `a`.

This is local-only: no SSH, agent execution, uploads, or reboot layout restoration. Restarting the supervisor leaves managed tmux running, but does not promise to recreate local attachments. Destructive workspace commands are intentionally absent.

Run the checks with:

```sh
cargo test
cargo build --release
python3 tests/integration.py -v
```

Integration tests use isolated state directories, tmux servers, and a pseudoterminal; Python 3 is only needed for these tests.

See [prototype architecture and usage](docs/prototype.md) for lifecycle, transport, and persistence details.
