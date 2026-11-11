# Connection and workspace recovery

## Connection health

Each local tmux server has its own Rust helper and persistent SSH bridges. Health is checked every two seconds.
A host moves through `connecting`, `connected`, `reconnecting`, or `needs-attention`; the machine list and picker
show these states even if no workspaces are cached. Retry uses bounded exponential backoff with jitter, capped
at 30 seconds. Backoff resets only after 15 healthy seconds, so briefly successful connections do not cause
rapid retry loops.

On connection, the helper refreshes an atomic `state.snapshot` before advertising `connected`. During an outage,
cached remote rows remain visible as `stale`. Operations against stale state are disabled. Open always resolves
fresh; it does not attach using a cached socket/session. Lost mutation responses are outcome-unknown and are
never automatically replayed. Refresh the list and inspect the result before manually retrying a create or restore.

Background SSH uses `BatchMode=yes` and `StrictHostKeyChecking=yes`. It cannot answer passwords, passphrases,
or host-key prompts. Use:

```sh
tmux machine connect user@server
```

This opens a `-EE` popup for native interactive SSH authentication, wakes reconnect, and waits for a healthy
connection. No credentials are stored and host-key verification is not bypassed. Correct changed host keys
through your normal trusted SSH workflow, not by disabling checks. Background reconnect can start a missing
**already-installed** compatible supervisor; it never installs or upgrades binaries.

SSH uses a private, controlled-path `ControlMaster=auto` scoped to this client, with `ControlPersist=60`.
An authenticated private master lets background bridges reuse the connection. To disable multiplexing, set
`TMUX_AGENTS_SSH_MULTIPLEX=0` in the client environment before starting it. Without multiplexing, background
connections require key/agent authentication; an interactive password login alone does not authorize them.
Private SSH control paths are limited to 85 bytes, including their 18-byte filename/path separator, to leave
space for OpenSSH's temporary suffix on macOS. Use a shorter state directory or disable multiplexing if necessary.
See [remote setup](remote-setup.md) for initial authorization and matching binary requirements.

## Attachments

The native SSH attachment runner transports tmux terminal bytes, not JSON. Exit status zero is an intentional
detach and does not trigger reattachment. Once SSH exits and the runner waits for reconnection, typed keys
are discarded, never queued for replay. Until SSH detects a broken link, its normal terminal buffering applies.

The client remembers stable host/workspace attachment preferences per local tmux server and reloads them after
that server restarts. Closing or detaching an attachment window forgets the preference; use the runner's `q`
option during an outage to forget it as well. This is not a request to restart remote commands. The Bash plugin
only invokes `client open`; attachment focus/reuse, tags, and preference management stay in the Rust layer.

## Managed layout persistence

The supervisor worker periodically captures layouts and working directories (approximately every 500 ms),
using atomic persistence. Workspace IDs remain stable. Existing v1 state is migrated safely with a backup.
Managed panes use `remain-on-exit on` so exited processes do not immediately erase the recorded layout.

Startup and managed-tmux loss trigger restoration from saved state. Explicit `workspace snapshot REF` saves a
layout; `workspace restore REF` restores only where safe. Restoration never overwrites a live workspace layout,
touches unrelated tmux servers, or replays shell commands. Window/pane structure and working directories can be
recreated, but processes, editor buffers, shell history, and in-flight terminal state cannot. Abrupt loss may lose
the most recent changes since the last snapshot.

Directories must be absolute UTF-8 paths without control characters or a trailing semicolon.
Window titles with control characters or trailing semicolons cannot be saved; the previous snapshot is preserved.
The state directory and attachment socket/executable paths must not end with semicolons either.

Keep state backups and coordinate compatible client/server upgrades. Published `pre-0.1` does not support this
bridge/recovery protocol: build and provision new binaries on **both** ends. This document describes the intended
implementation behavior, not a claim of live SSH or reboot validation. No release is published by this change.
