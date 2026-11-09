# Local prototype

## Scope and components

This Rust prototype manages local workspaces through an isolated tmux server. It requires Unix and tmux 3.8 or newer and is tested on macOS with tmux 3.8. See the [README](../README.md) for build and plugin installation.

- **Local plugin (`tmux-agents.tmux`)** exposes `tmux machine` commands and attachment bindings without changing the local tmux prefix.
- **Short-lived CLI** sends requests to the supervisor over a private Unix socket.
- **Supervisor** owns workspace operations and persistent metadata.
- **Managed tmux server** uses a private socket and a dedicated configuration that ignores the user's tmux configuration. Its prefix is `C-a`.

The plugin resolves the executable as `target/release/tmux-agents` under the repository, unless `TMUX_AGENTS_BIN` overrides it. Build it with `cargo build --release`.

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

The supervisor and managed tmux server have separate lifetimes. Restarting the supervisor does not kill managed tmux. Persistent IDs and metadata support continued workspace lookup; they are not a promise to recreate every local attachment.

The prototype is limited to 128 workspaces and serves one request at a time, with two-second socket I/O timeouts. Workspace definitions are saved before tmux starts; a launch failure retains a stopped definition. Stopped definitions are listed but cannot yet be restarted or deleted through the plugin. Arbitrary native renaming of managed tmux sessions is not supported.

This prototype does not provide SSH, agent integrations, uploads, reboot layout restoration, or automatic recreation of local attachments. Managed configuration currently only sets the prefix; user configuration inclusion is deferred. It intentionally exposes no destructive workspace commands: the initial interface focuses on starting, discovering, creating, and opening workspaces without offering deletion or teardown operations.
