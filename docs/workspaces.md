# Local workspace behavior

`tmux-agents` uses socket label `tmux-agents`. Its first session is named `main`;
subsequent launches attach to the last session attached to or selected by a client.
Native tmux commands can create and rename additional sessions. With multiple
clients, the most recent attach or session switch determines the remembered session.

The dedicated socket keeps application sessions separate from ordinary tmux
sessions. Reserve this socket label for the application. Tmux still reads your
normal configuration when its server starts, so your preferred native bindings
and appearance apply. Configuration changes here do not affect other running
tmux servers.

New workspaces start in the launcher's current directory. Reattachment does not
change existing panes' working directories, active selection, or layout.
Detaching leaves processes alive. There is no application daemon or terminal
rendering layer; the foreground launcher waits for tmux and saves once more on
normal detach.

Launch requires an interactive terminal. Invocation from inside tmux is rejected
to avoid nested sessions with competing bindings and status bars. Detach and
launch from your outer terminal instead.

No agent launch commands are provided. Start programs in the shell and interact
with them normally. There are no automatic restarts or agent-status integrations.

## Saved state

The versioned snapshot lives at:

```text
${XDG_STATE_HOME}/tmux-agents/workspace.json
```

If `XDG_STATE_HOME` is unset or empty, use
`~/.local/state/tmux-agents/workspace.json`. These base directories must be absolute.
New application directories have mode `0700`, and snapshot and lock files have
mode `0600`. State contains workspace names and paths, but no command lines,
credentials, or environment variables.

A file lock serializes startup and saves. Snapshots are written to a private
temporary file, synced, and atomically renamed; a failed capture does not replace
the previous snapshot. Malformed or unsupported snapshots stop reconstruction
with an error and remain unchanged. Failed reconstruction removes only sessions
created by that attempt. A running server is authoritative over disk state.

To recover from a damaged snapshot or start over, stop the application server
and move `workspace.json` to a backup location before launching again.

## Capturing native changes

Indexed tmux hooks capture layout, pane/window selection, session changes,
renames, window linking/unlinking, and attachment changes. The application reserves
hook array index `9876`, leaving other hook entries intact. Global option
`@ta_last_session` records session selection. Do not overwrite these entries.

Hooks invoke a private, short-lived launcher helper. Burst saves are coalesced
with a 200 ms delay; there is no polling service. Hooks use the launcher's absolute
executable path, explicit state directory, and actual server socket path. Reopen
`tmux-agents` after moving or replacing the executable to refresh the hooks.

Tmux has no working-directory-change hook. The next snapshot captures each pane's
current directory; normal detach also saves synchronously. A crash can lose recent
changes, especially a `cd` not followed by another tmux event. Dead panes use their
startup directory when their current directory is unavailable.

## Reconstruction after server loss

When a live server exists, reconnect without rebuilding anything. Otherwise restore:

- Session names and last-selected session.
- Window names, indices, and active selection.
- Pane arrangement, working directories, active pane, and zoom state.

Saved layouts are applied to new pane IDs and adapt to the current terminal size.
Missing directories fall back to the launcher's current directory with a warning.
New panes run the launcher's `SHELL` as a login shell, falling back to `/bin/sh`
when unavailable. Saved commands are never replayed, and tmux's `default-command`
is bypassed. Shell startup files and the user's tmux configuration can still
execute their own commands.

Not restored: running processes, agent conversations, scrollback, shell variables,
arbitrary tmux options, session groups, or linked-window relationships. Linked
windows reconstruct as independent copies. When the final session disappears,
the last nonempty snapshot is retained, so the next launch can rebuild it.

## Checks

```sh
cargo fmt --check
cargo clippy -- -D warnings
python3 tests/test_startup.py
```

The Python standard-library tests build the executable, exercise error handling,
and launch it in a pseudo-terminal. Live-session tests use temporary state and
`TMUX_TMPDIR` directories, keeping test servers isolated from real workspaces.
They cover reconnection, process continuity, server isolation, native change
snapshots, last-session selection, reconstruction, zoom, missing directories,
literal paths and names, dead panes, linked-window indices, default-command
suppression, and malformed snapshots.
Live-session tests are skipped if tmux is unavailable.
