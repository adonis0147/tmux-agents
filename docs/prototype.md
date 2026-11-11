# Architecture and commands

See the [README](../README.md) for setup, [remote setup](remote-setup.md) for SSH authorization,
and [recovery](recovery.md) for persistence and failure semantics.

## Scope and components

- The Bash 3.2 plugin provides tmux aliases and popups; it does not implement the transport or replace key bindings.
- A detached Rust client helper belongs to each local tmux socket. It exits when that local server disappears.
- The helper maintains persistent SSH bridges, per-host health/retry state, cached snapshots, and attachments.
- Each machine's supervisor owns workspace operations and persistence in its dedicated managed tmux server.
- Managed tmux ignores unrelated user configuration and uses `C-a`. Unrelated tmux servers are not adopted.

Loading invokes the installer, then `client ensure SOCKET` using `tmux display-message -p '#{socket_path}'`.
The plugin stores `@tmux-agents-socket` and suppresses helper startup stdout. It does not create a system service.
The installer honors an executable `TMUX_AGENTS_BIN` override before consulting `RELEASE_TAG`.
Both client and server must be new source builds; published `pre-0.1` lacks bridge/recovery support.
See [installation](releases.md#source-builds-and-offline-overrides).

`TMUX_AGENTS_HOME` defaults to `$HOME/.local/state/tmux-agents`. It must be absolute, private, user-owned,
mode `0700`, and not a symlink. Canonical state paths must be UTF-8, contain no control characters, and fit
within 85 bytes for portable Unix sockets, without a trailing semicolon. Keep the directory private:
sockets grant access to managed sessions. SSH multiplexing needs additional path headroom; see [recovery](recovery.md).

## Interactive commands

| Command | Purpose |
| --- | --- |
| `tmux machine add HOST` | Explicitly check/install and register an SSH destination. |
| `tmux machine list` | Show machines, release tags, connection states, and details (including built-in `local`). |
| `tmux machine start` | Ensure the local supervisor and per-local-tmux helper. |
| `tmux machine status [HOST]` | JSON connection/supervisor status. |
| `tmux machine connect HOST` | Native SSH authentication popup; wake reconnect and wait for health. |
| `tmux machine workspace [HOST] list` | List workspaces on a machine. |
| `tmux machine workspace [HOST] create NAME [DIR]` | Create a workspace. |
| `tmux machine workspace [HOST] open REF` | Resolve fresh and focus/create an attachment window. |
| `tmux machine workspace [HOST] snapshot REF` | Persist the current workspace layout. |
| `tmux machine workspace [HOST] restore REF` | Restore a saved workspace where safe, not over a live layout. |
| `tmux machine picker` | Numeric popup with connection states and window/pane counts. |

Omit `HOST` for local commands, preserving the original syntax. `REF` is a name or stable ID.
Local create without `DIR` uses the current pane directory; remote paths are interpreted on the remote host.
SSH aliases matching `list`, `create`, `open`, `snapshot`, or `restore` are ambiguous in the host position;
use `user@alias` to disambiguate them. Quote names and paths as ordinary tmux command arguments.

`tmux machine picker` opens the picker. Machines appear even when no workspace rows exist; disconnected machines suggest
`machine connect HOST`. Cached remote workspaces are marked `stale` and cannot be opened. Reopen the picker
after connecting. Selection sends the stable ID to `client open`, which re-resolves instead of trusting cached data.

Attachment window creation, tagging, reuse, and stable preference persistence belong to the Rust client,
not duplicated Bash logic. Keep these windows dedicated to attachments rather than mixed shell splits.

## Optional key bindings

The plugin installs no key bindings. To opt into picker and remote-prefix shortcuts, add these to `.tmux.conf`:

```tmux
bind-key M run-shell -E 'bash #{q:@tmux-agents-root}/scripts/machine.sh picker'
bind-key a if-shell -F '#{!=:#{@tmux-agents-workspace},}' 'send-keys C-a' 'send-prefix'
```

The second binding forwards `C-a` in tagged attachments and the local prefix elsewhere.
Choose unused keys rather than replacing your own shortcuts.

## Internal CLI contract

The plugin obtains the actual local tmux socket for each invocation. The Rust CLI is responsible for transport:

```text
tmux-agents client ensure SOCKET
tmux-agents client machines SOCKET
tmux-agents client workspaces SOCKET [HOST]
tmux-agents client status SOCKET [HOST]
tmux-agents client request SOCKET HOST workspace.METHOD [ARGS...]
tmux-agents client open SOCKET HOST REF
tmux-agents client connect SOCKET HOST
tmux-agents client reconnect SOCKET HOST
```

`machines` emits tab rows `HOST TAG STATE DETAIL`; states are `connecting`, `connected`, `reconnecting`, and
`needs-attention`. `workspaces` emits `HOST ID NAME STATE WINDOWS PANES`; disconnected remote rows are `stale`.
`request` routes list/create/snapshot/restore to the chosen host. `open` handles both local and remote attachment
resolution and window focus. `reconnect` resets the retry schedule; interactive authentication uses `connect`.

Root CLI `machine list`/`machine register DEST TAG` remain internal registry/setup helpers, not the plugin's
live machine listing. Root `ensure`, `supervisor`, and `request` remain useful for local diagnostics.
The supervisor's private protocol uses bounded length-prefixed JSON with protocol versions and correlation IDs.
Attachment SSH streams carry native tmux bytes, not JSON. A lost mutation response has an unknown outcome:
refresh state before deciding whether to retry; the helper never replays the mutation automatically.

## Lifecycle and limits

The supervisor and managed tmux have independent lifetimes; restarting the supervisor does not kill tmux.
The per-local-server helper is neither the supervisor nor an OS service. Background connections may start a
missing, already-installed compatible supervisor, but never install or upgrade software. Installation does not
replace running supervisors; protocol upgrades require manual coordination.

Layout recovery preserves workspace IDs, window/pane structure, and working directories, not running processes
or shell history. Recovery never replays commands or destructively restores over a live layout. See
[recovery](recovery.md) for outage behavior and attachment preferences.

RPC frames are limited to 64 KiB. Large aggregate lists fail explicitly rather than returning partial state;
use host-specific workspace/status queries. The aggregate picker is unavailable when the combined cache exceeds
this bound. Polling/large registries have not been load-tested.

No agent integration, uploads, permanent sidebar, or exact Herdr parity is provided. Destructive workspace
teardown is intentionally not exposed by this plugin.
