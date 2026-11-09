# Local prototype

## Scope and components

This Rust prototype manages local workspaces through an isolated tmux server. It requires Unix and separately installed tmux 3.8 or newer and is tested locally on macOS with tmux 3.8. Release builds have passed on GitHub; Linux runtime behavior and the minimum macOS versions (Intel 10.12, Apple Silicon 11.0) remain unverified. See the [README](../README.md) for getting started and [installation and releases](releases.md) for installer behavior and publishing.

- **Local plugin (`tmux-agents.tmux`)** exposes `tmux machine` commands and attachment bindings without changing the local tmux prefix.
- **Short-lived CLI** sends requests to the supervisor over a private Unix socket.
- **Supervisor** owns workspace operations and persistent metadata.
- **Managed tmux server** uses a private socket and a dedicated configuration that ignores the user's tmux configuration. Its prefix is `C-a`.

At startup, the plugin runs `sh scripts/install.sh`. An executable-file `TMUX_AGENTS_BIN` override takes precedence; otherwise the installer reads the exact release tag from the root `RELEASE_TAG` file (currently `pre-0.1`) and platform from `uname`, reuses an existing executable, or downloads and verifies the matching GitHub release into `bin/<tag>/<target>/tmux-agents`. `RELEASE_TAG` must contain exactly one line matching `[A-Za-z0-9][A-Za-z0-9._-]*`; the Cargo package version is independent and does not identify downloads. Installation failure stops plugin loading before registering bindings or the `tmux machine` alias; reload to retry. Loading the plugin does not start the supervisor.

For source development, run `cargo build --release` and set `TMUX_AGENTS_BIN=/absolute/repo/target/release/tmux-agents` in the tmux server environment before loading the plugin. See [source builds and offline overrides](releases.md#source-builds-and-offline-overrides) for commands. The direct source-build commands below remain valid.

`TMUX_AGENTS_HOME` must be an absolute path to a private state directory. It defaults to `$HOME/.local/state/tmux-agents`. Keep this directory private: its sockets provide access to the supervisor and managed tmux server. Persistent workspace metadata lives in `metadata.json`; workspace IDs remain stable across supervisor restarts. The canonical state-directory path must be UTF-8, contain no control characters, and fit within 85 bytes for portable Unix socket paths. Existing directories must be owned by the current user, have mode `0700`, and not be symlinks.

## Interactive usage

After loading the plugin with `run-shell '/absolute/repo/tmux-agents.tmux'`:

| Command | Purpose |
| --- | --- |
| `tmux machine start` | Ensure the supervisor is running. |
| `tmux machine status` | Show JSON status, including PID, instance identity, version, and capabilities. |
| `tmux machine workspace create NAME [ABSOLUTE_DIR]` | Create a named workspace, optionally specifying its directory. |
| `tmux machine workspace list` | List workspaces. |
| `tmux machine workspace open NAME_OR_ID` | Resolve and attach to a workspace. |
| `tmux machine picker` | Pick a workspace in a shell popup. |

The local prefix followed by `M` invokes the same picker. It lists workspaces and reads a numeric selection; it is not a tmux choose-list UI.

The managed server uses `C-a`, while the local server retains its existing prefix. The local prefix followed by `a` sends `C-a` to the managed server only in an attachment pane. The binding checks the window option `@tmux-agents-workspace`, rather than forwarding keys unconditionally in ordinary windows. The plugin installs prefix bindings `M` and `a`, replacing any existing bindings on those keys. Outside managed windows, `a` sends the local prefix. Keep attachment windows dedicated to the managed connection; pane mirroring and mixed attachment/shell splits are not supported.

## Supervisor and request interface

For foreground operation, run:

```sh
./target/release/tmux-agents supervisor
```

For idempotent detached startup, run:

```sh
./target/release/tmux-agents ensure
```

The short-lived transport command is:

```text
tmux-agents request METHOD [ARGS...]
```

| Method | Arguments | Output |
| --- | --- | --- |
| `status` | None | JSON status. |
| `workspace.list` | None | Tabular workspace data. |
| `workspace.create` | `NAME ABSOLUTE_DIR` | Tab-separated ID and name. The plugin supplies the current pane directory when omitted. |
| `workspace.resolve` | `NAME_OR_ID` | Three lines: managed tmux socket, session, then stable workspace ID. |

`open` is a plugin operation, not a separate request method: it resolves a workspace and uses the returned socket and session to attach.

The private Unix-socket protocol uses length-prefixed JSON, with a maximum message size of 64 KiB. Every request and response carries a protocol version and correlation ID. This internal framing is distinct from the CLI's JSON or tabular output. A `status` request provides the version/capability handshake:

```json
{"version":1,"id":"example","method":"status","args":[]}
```

Responses contain `version`, matching `id`, `result`, and `error` (null on success). There are no event subscriptions in this prototype. A lost creation response is reported as outcome unknown; refresh the list before retrying. No mutation is automatically replayed.

## Lifecycle and limits

The supervisor and managed tmux server have separate lifetimes. Restarting the supervisor does not kill managed tmux. Installation never restarts a running supervisor or automatically replaces the service. Protocol upgrades and any required migration remain manual; installing a new CLI does not upgrade an already running supervisor. Persistent IDs and metadata support continued workspace lookup; they are not a promise to recreate every local attachment.

The prototype is limited to 128 workspaces and serves one request at a time, with two-second socket I/O timeouts. Workspace definitions are saved before tmux starts; a launch failure retains a stopped definition. Stopped definitions are listed but cannot yet be restarted or deleted through the plugin. Arbitrary native renaming of managed tmux sessions is not supported.

This prototype does not provide SSH, agent integrations, uploads, reboot layout restoration, or automatic recreation of local attachments. Managed configuration currently only sets the prefix; user configuration inclusion is deferred. It intentionally exposes no destructive workspace commands: the initial interface focuses on starting, discovering, creating, and opening workspaces without offering deletion or teardown operations.
