# tmux-agents

A Rust local prototype for managing workspaces in a dedicated tmux server from your existing tmux session. Your local server keeps its configuration; managed workspaces use a separate configuration and `C-a` prefix.

Requires Unix, **Bash 3.2 or newer**, and **tmux 3.8 or newer**, installed separately. macOS binaries target Intel 10.12+ and Apple Silicon 11.0+. Tested locally on macOS with tmux 3.8 and for remote installation/startup on Linux x86_64 with tmux 3.6. Release builds have passed on GitHub; older macOS runtime behavior remains unverified.

## Get started

Clone the public repository:

```sh
git clone https://github.com/adonis0147/tmux-agents.git ~/.tmux/plugins/tmux-agents
```

Add this to your local `~/.tmux.conf`:

```tmux
run-shell '~/.tmux/plugins/tmux-agents/tmux-agents.tmux'
```

Alternatively, with [TPM](https://github.com/tmux-plugins/tpm), add `set -g @plugin 'adonis0147/tmux-agents'` before TPM's initialization and install plugins normally. The standard `tmux-agents.tmux` entry point is supported.

For an already running local server, load it with:

```sh
tmux run-shell "${HOME}/.tmux/plugins/tmux-agents/tmux-agents.tmux"
```

On loading, the plugin automatically installs the exact release tag in the root `RELEASE_TAG` file (currently `pre-0.1`) for macOS or Linux on x86_64 or aarch64. Downloads require `curl` and either `sha256sum` or `shasum`; an existing installed executable works offline. No Rust, sudo, or PATH installation is needed. Loading the plugin does not start the supervisor or install tmux.

**The exact `pre-0.1` GitHub release must be published before automatic installation works.** A checkout whose `RELEASE_TAG` is not yet published may fail to install; use a checkout pointing to a published release or the [source/offline override](docs/releases.md#source-builds-and-offline-overrides). See [installation and releases](docs/releases.md) for supported targets, diagnostics, upgrades, and publishing.

`TMUX_AGENTS_HOME` selects an absolute, private state directory; its default is `$HOME/.local/state/tmux-agents`.

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

### Explicit remote setup

`tmux machine add HOST_OR_USER@HOST` checks the remote supervisor over SSH, asks for installation approval only if it is missing, and otherwise continues directly with progress displayed; `tmux machine list` lists registrations, not live status. **Build the new local client and update `TMUX_AGENTS_BIN` in the tmux server before loading the plugin**: published `pre-0.1` lacks the required registry CLI. See [remote setup](docs/remote-setup.md) for commands, prerequisites, security, and failure behavior.

Remote setup is not remote workspace UI: all workspace commands, `start`, `status`, and the picker remain local. There is no remote routing, attachment, reconnect, agent execution, uploads, or reboot layout restoration. Restarting the supervisor leaves managed tmux running, but does not promise to recreate local attachments. Destructive workspace commands are intentionally absent.

Run the checks from a source checkout with Rust/Cargo and Python 3 installed:

```sh
cargo test
cargo build --release
python3 tests/remote.py -v
python3 tests/installer.py -v
python3 tests/integration.py -v
```

Integration tests use isolated state directories, tmux servers, and a pseudoterminal; Python 3 is only needed for these tests.

See [prototype architecture and usage](docs/prototype.md) for lifecycle, transport, and persistence details.
