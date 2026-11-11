# tmux-agents

Manage local and SSH-hosted workspaces from your existing tmux session. Managed workspaces use a separate tmux
server and `C-a` prefix; your local server keeps its configuration. A small popup picker shows machines,
connection health, and workspace window/pane counts. There is no permanent panel.

Requires Unix, Bash **3.2+**, and local tmux **3.8+** (remote tmux **3.2+**).

## Get started

Clone the repository:

```sh
git clone https://github.com/adonis0147/tmux-agents.git ~/.tmux/plugins/tmux-agents
```

Add to `~/.tmux.conf`:

```tmux
run-shell '~/.tmux/plugins/tmux-agents/tmux-agents.tmux'
```

TPM users can instead add `set -g @plugin 'adonis0147/tmux-agents'` before TPM initialization.

**This checkout requires newly built client AND server binaries.** Published `pre-0.1` does not implement
its bridge/recovery protocol. The pinned installer cannot supply these features yet. Build from source and
set the override in the tmux server **before loading**:

```sh
cargo build --release
tmux set-environment -g TMUX_AGENTS_BIN "${PWD}/target/release/tmux-agents"
tmux run-shell "${PWD}/tmux-agents.tmux"
```

Provision the matching server binary on remote hosts as well; do not use the old published server for these
features. See [installation and releases](docs/releases.md) and [remote setup](docs/remote-setup.md).
Loading starts/reuses a detached Rust helper scoped to your local tmux server, not a standalone system service.
`TMUX_AGENTS_HOME` selects private state (default `$HOME/.local/state/tmux-agents`).

```sh
tmux machine start
tmux machine workspace create demo /absolute/project/directory
tmux machine workspace open demo
tmux machine list
tmux machine status
```

Local commands retain their implicit-local syntax. Remote commands put the host before the operation:

```sh
tmux machine add user@server
tmux machine connect user@server
tmux machine workspace user@server list
tmux machine workspace user@server create demo /remote/project
tmux machine workspace user@server open demo
tmux machine status user@server
```

Use `tmux machine picker` for the numeric picker. The plugin does not replace any key bindings;
optional shortcuts are shown in the [command guide](docs/prototype.md#optional-key-bindings).
Open accepts names or stable IDs; the picker opens by stable ID.

Connections reconnect automatically with cached, visibly stale workspace rows during outages. Stale operations
are disabled; mutations with lost responses are never replayed. `machine connect HOST` exposes native SSH
authentication in a popup. See [commands and architecture](docs/prototype.md),
[SSH setup/security](docs/remote-setup.md), and [recovery and limitations](docs/recovery.md).

This is a workspace prototype, not an agent integration, upload tool, sidebar, or exact Herdr replica.

## Development checks

```sh
cargo test
cargo build --release
python3 tests/remote.py -v
python3 tests/installer.py -v
python3 tests/integration.py -v
python3 tests/transport.py -v
python3 tests/recovery.py -v
python3 tests/control.py -v
```

Tests use isolated state/tmux servers and pseudoterminals. These commands are not a claim of live SSH validation
for the bridge/recovery implementation. No release is published by this change.
