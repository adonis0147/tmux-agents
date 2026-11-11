# Switching machines and workspaces

Start normally:

```sh
tmux-agents
```

No menu appears at startup. Press your tmux prefix followed by uppercase `W`
(default: `Ctrl-b`, release, then `Shift-w`) when you want to switch.

The native tmux menu contains:

- **Sessions on this machine** (`s`): open tmux's session tree, including its
  ordinary window/pane navigation.
- **local**: return to the originating machine when attached remotely.
- Registered SSH destinations other than the currently selected machine.

Use arrow keys and Enter to select a machine, or Escape to cancel. Selecting a
machine attaches to its last session. If it has no live server, its launcher
reconstructs the saved workspace or creates an initial shell. Open the session
tree there to select another remote session.

This is intentionally lazy discovery: other machines are not queried merely to
display the menu. Their sessions are browsed after connecting. Offline or
unselected registrations do not delay local startup.

The local menu refreshes the registry on opening, so machines added while the
local workspace is open appear immediately. Remote menus use the originating
client's registry snapshot, refreshed on the next attachment. Return locally
to see newly registered machines while already connected remotely.

## Bindings

Only the application-owned tmux server receives this binding. Existing `W`
bindings are left intact, with a warning instead of being overwritten.
To choose a free key, use a native tmux option in your configuration:

```tmux
set -g @ta_workspace_key F12
```

Reconnect after changing the option. This changes the key after the prefix;
it does not change tmux's prefix or replace native split/navigation bindings.
Manage attachments through `tmux-agents` to enable the workspace menu; ordinary
external `tmux attach` clients do not have the launcher's routing context.

## Attachment handoff

The foreground launcher detaches only its own current tmux client before starting
the next attachment. Local agents and remote agents continue in their respective
tmux servers. Other attached clients are not detached.

SSH attaches with a remote PTY and runs the remote `tmux-agents` helper. The
remote helper owns its tmux client, not a second terminal UI. All outgoing SSH
connections are initiated by the original local launcher, including switches
from one remote machine to another. No credentials or SSH agent are forwarded
by the application.

Remote menus receive only destination metadata and a connection token. The
originating client's machine registry remains local. Each tmux client has its
own routing context, so simultaneous launcher instances cannot consume each
other's requests.

A short-lived, private handoff file carries the selected destination. On leaving
a remote attachment for another machine, one noninteractive SSH command reads
and removes that request before the next attachment. This can require another
authentication prompt; existing SSH connection-sharing configuration is honored.
No application daemon or listening network service is added.

Registration still only verifies and saves a machine. It never creates a remote
tmux session; selecting that machine is what opens or restores one.

## Resume and connection loss

Normal detach returns to the outer terminal. The next bare launch resumes the
last selected destination. Automatic remote resume uses `BatchMode=yes`, one
connection attempt, and a 3-second SSH connection timeout so it cannot stop on
an authentication prompt. If it fails, the local workspace opens instead.

Explicit remote selections use normal SSH authentication and a 10-second
connection timeout. While attached, SSH keepalives help detect a lost transport.
A failed attachment, lost connection, or failed handoff retrieval returns to the
local workspace with an error message. Try selecting the remote again after
fixing connectivity; its tmux server and agents remain independent of SSH.

Use the same current application build on both ends. The remote noninteractive
SSH command environment must find `tmux-agents` in `PATH`. For example:

```sh
ssh user@host 'command -v tmux-agents'
```

When testing against `127.0.0.1` as the same user, local and remote attachments
can share the same application tmux server and saved workspace. They still
handoff without nesting; separate machines naturally use separate servers.

## State and limitations

`selection.json` beside the local `workspace.json` records the last destination.
It is private, versioned, synced, and atomically replaced. An invalid selection
or a removed registration falls back to local. Malformed machine registries are
left unchanged and disable remote selection rather than blocking local startup.

Transient `@ta_route_<client-pid>` options associate a client with its routing
context. These are removed after normal detach. Requests use private
`handoff.<connection-token>.json` files in the selected machine's state directory.
An interrupted request retrieval can leave a small stale handoff file; there is
no automatic expiry yet. No saved request is replayed on a later attachment,
because every attachment uses a new token.

Shell environments, terminal scrollback, and running processes still cannot
survive tmux-server loss; the normal restoration limitations apply independently
on every machine.

## Tests

```sh
python3 tests/test_switching.py
```

The suite uses PTYs and isolated tmux servers. If OpenSSH server tools are
available, it starts an unprivileged loopback `sshd` with temporary keys, strict
host-key checking, and isolated remote directories. No existing SSH configuration,
authorized keys, or known-host files are changed.

Checks cover native menu/session selection, custom bindings, literal paths,
client isolation, live registration, local/remote and remote/remote handoffs,
last-session selection, resume, offline startup, disconnection, and process
continuity. SSH cases are skipped if the test server cannot be started.
