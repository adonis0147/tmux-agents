# Remote setup and SSH authorization

`tmux machine add HOST_OR_USER@HOST` explicitly checks/installs a remote supervisor and registers that SSH
destination. Afterwards, the local tmux helper manages remote workspace routing, attachments, health, and reconnect.
See [commands](prototype.md) and [recovery](recovery.md).

## Prerequisites and usage

Local requirements are Bash **3.2+** and tmux **3.8+**. Remote hosts need Bash **3.2+**, tmux **3.2+**, and, for
installation, `curl`, `sha256sum` or `shasum`, and access to GitHub assets. Setup never installs system tmux or
uses sudo. Supported release targets are listed in [installation](releases.md#automatic-installation).

**New client AND server binaries are required.** Published `pre-0.1` lacks bridge/recovery support. Build locally
and provision a matching remote server before using this checkout's remote UI. The automatic installer still
pins `RELEASE_TAG`; it cannot turn an old release into a compatible server. Do not treat a successful old-server
setup check as proof of bridge compatibility. No release is published by this change.

```sh
cargo build --release
tmux set-environment -g TMUX_AGENTS_BIN "${PWD}/target/release/tmux-agents"
tmux run-shell "${PWD}/tmux-agents.tmux"
tmux machine add user@server
tmux machine connect user@server
tmux machine list
tmux machine workspace user@server list
tmux machine workspace user@server create demo /remote/project
tmux machine workspace user@server open demo
```

The override must be in the tmux server environment before loading, not only exported in a pane.
Remote directories are remote paths; a local state override does not choose remote state.

## Explicit installation approval

The plugin opens `scripts/machine-add.sh` with `display-popup -EE`, keeping SSH authentication and host-key
prompts visible. Setup uses native SSH, honors SSH configuration, and runs `bash -s` without a remote PTY (`-T`).
It uses `ConnectTimeout=10`, `ServerAliveInterval=15`, and `ServerAliveCountMax=3`.
It does not store credentials or bypass host-key verification.

The first SSH check is read-only: it checks prerequisites and the exact managed release/platform binary.
If present, setup continues without installation approval. If missing, `Install it now? [y/N]` requires explicit
`y`/`yes` (case-insensitive). Enter, EOF, or any other response cancels without installation, startup, or registration.
A second SSH connection performs approved setup, so authentication may prompt again. If the binary disappears
between check and setup, setup fails instead of installing without approval.

The popup reports connecting, checking prerequisites/installation, installing/reusing, starting/checking, and
validating/registering. Downloads show real curl progress and checksum messages on stderr; JSON status remains
separate. After registration, the popup establishes the helper's management channel; native SSH may ask again
when authorizing its private connection master. On interactive success, press Enter to close.

## Installation and state

Approved setup installs the bootstrap and exact `RELEASE_TAG` under `~/.local/share/tmux-agents`. This remote root
must be user-owned, mode `0700`, and not a symlink. The shared installer verifies `SHA256SUMS`, then atomically
renames the binary into `<root>/bin/<tag>/<target>/tmux-agents`. Existing executables are reused without downloads
or bootstrap rewriting. See the [verification trust boundary](releases.md#downloads-and-verification).

Remote state defaults to `~/.local/state/tmux-agents`; `TMUX_AGENTS_HOME` in the remote command environment can
select another private absolute directory. The [state constraints](prototype.md#scope-and-components) apply.
Setup starts/reuses a compatible detached supervisor. It never replaces a running service or adopts unrelated
tmux servers. Incompatibility requires manual attention. Background reconnect is more restricted: it may start
an already-installed supervisor, but never installs or upgrades software.

## Registration and connection UI

Only a successful SSH exit and bounded JSON status that passes protocol/capability/identity checks allow internal
`tmux-agents machine register DEST TAG` to store the profile in local `machines.json`, under a lock with atomic
replacement. Registration stores no credentials. Repeating the same destination is idempotent; distinct aliases
remain separate profiles even if they reach the same host. Registration notifies/starts the client helper.

The plugin's `tmux machine list` now uses `client machines SOCKET`, including built-in `local`, connection state,
and detail. Root native `machine list` and `machine register` remain internal registry/setup helpers.
`tmux machine status [HOST]` shows JSON status. The picker displays machines even with no workspaces, and suggests
`machine connect HOST` for unavailable connections. Stale workspace rows cannot be opened.

`machine connect HOST` opens native interactive SSH in a popup, using the current binary and state environment,
then waits for a healthy bridge. Automatic reconnect uses strict noninteractive authentication and private SSH
multiplexing; see [recovery](recovery.md#connection-health) for options, backoff, and key/agent requirements.

## Failures and validation

Failed SSH, installation, or status validation does not register a profile. Local registration failure after remote
setup can leave the supervisor installed/running; setup has no remote rollback. Fix the local error and retry.
Do not retry a workspace mutation blindly after a lost response; inspect refreshed state first.

Run the offline and isolated checks listed in the [README](../README.md#development-checks). Bridge/recovery
behavior is not claimed to have been live-SSH-validated here. Cross-platform release builds are not equivalent
to remote runtime validation.
