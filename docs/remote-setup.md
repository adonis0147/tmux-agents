# Automatic remote supervisor setup

`tmux machine add HOST_OR_USER@HOST` explicitly requests installation/checking of a remote supervisor and local registration of that SSH destination. It does not open a login shell or provide remote workspace UI, routing, attachment, or reconnect. `machine start`, `machine status`, `machine picker`, and all `machine workspace …` commands remain local.

## Prerequisites and usage

The local client requires Bash **3.2 or newer** and tmux **3.8 or newer**. Build the new local client: the published `pre-0.1` binary lacks the machine registry CLI required by this flow. From your source checkout, update the override in the tmux server **before loading the plugin**:

```sh
cargo build --release
export TMUX_AGENTS_BIN="${PWD}/target/release/tmux-agents"
tmux set-environment -g TMUX_AGENTS_BIN "${TMUX_AGENTS_BIN}"
tmux run-shell "${PWD}/tmux-agents.tmux"
tmux machine add user@server
tmux machine list
```

The remote host needs Bash **3.2 or newer**, tmux **3.2 or newer**, `curl`, `sha256sum` or `shasum`, and access to GitHub release assets. Supported remote releases are the same four [OS/architecture targets](releases.md#automatic-installation) as local installation. tmux must already be installed; setup never uses sudo or upgrades system tmux.

End-to-end setup was validated against a Linux x86_64 server with tmux 3.6: a fresh verified installation, detached supervisor startup, successful registration, and repeated setup retaining the same supervisor PID and instance. The actual `tmux machine add` popup also completed successfully before the confirmation/progress enhancement. The new confirmation, cancellation, progress, and completion pause are covered by offline pseudoterminal tests; the validation server was left uninstalled for manual testing.

## SSH and authorization

The plugin invokes `scripts/machine-add.sh` in a tmux popup with `-EE`, keeping SSH authentication and host-key prompts visible. It uses native SSH and respects SSH configuration, with `bash -s` as the non-PTY remote command (`-T`), `ConnectTimeout=10`, `ServerAliveInterval=15`, and `ServerAliveCountMax=3`. It neither stores credentials nor bypasses host-key verification.

Each `machine add` first shows the destination and release and performs a read-only SSH check of prerequisites and the exact managed release/platform binary. If it is already installed, setup continues without an installation prompt. If it is missing, the popup says so and asks `Install it now? [y/N]`. Enter, EOF, or any response other than `y`/`yes` (case-insensitive) cancels without installation, supervisor startup, or machine registration.

Normal SSH authentication and host-key confirmation apply to the initial check. Setup uses a second SSH connection after checking/approval; password authentication may therefore prompt again unless your SSH configuration reuses the connection. Credentials are never stored. Merely loading the plugin or listing registered machines does not initiate remote setup.

The popup shows five stages: connecting, checking remote prerequisites/installation, installing or reusing the supervisor, starting/checking it, and validating/registering the machine locally. The installation prompt, when needed, appears after the first two read-only stages. Fresh downloads show curl's real progress bars and checksum-verification messages; cached binaries report that no download is needed. Progress stays on stderr, separate from the JSON status response. On interactive success, press Enter to close the popup and return to tmux.

## Installation and state

For an approved missing installation, the bootstrap stages and installs the existing `scripts/install.sh` and the checkout's `RELEASE_TAG` under `~/.local/share/tmux-agents`. That remote install root must be owned by the remote user, have mode `0700`, and not be a symlink. The shared installer downloads the exact published tag, verifies the binary against `SHA256SUMS`, and atomically renames it into `<root>/bin/<tag>/<target>/tmux-agents`. An existing executable is reused directly, without downloads or rewriting bootstrap files. If that executable disappears between the initial check and setup, setup fails rather than installing without approval; retry to confirm installation. See [download verification and its trust boundary](releases.md#downloads-and-verification).

Remote state defaults to `~/.local/state/tmux-agents`, respecting `TMUX_AGENTS_HOME` when configured in the remote command environment. The Rust state-directory checks require an absolute, user-owned, private `0700` directory, not a symlink; the [other state-path constraints](prototype.md#scope-and-components) also apply. A local state override does not select the remote state directory.

Setup starts a detached supervisor or reuses an existing compatible one. It does not replace a running service, restart sessions, or adopt unrelated tmux servers. An incompatible running service requires manual attention, not an automatic upgrade.

## Registration and failures

Only after SSH exits successfully (status 0) and the returned bounded JSON status passes protocol, capability, and identity validation does the short-lived local Rust helper run:

```text
tmux-agents machine register DEST TAG
```

It stores the destination and release tag in `machines.json` under the local `TMUX_AGENTS_HOME` (or its default), using a lock and atomic replacement, without credentials. Retrying the same destination is idempotent; different SSH aliases are separate destinations even if they reach the same host.

`tmux machine list` reads these registered destinations and release tags; it does not query or display live remote status. The Rust `machine list` and `machine register` modes are internal helpers for plugin orchestration; there is no canonical Rust `machine add` command.

Failed SSH, installation, or status validation does not persist a registration. If local registration fails after remote setup succeeds, the remote supervisor may already be installed and running. Setup is not a transaction with remote rollback; fix the local problem and retry.

## Validation

Run the offline remote-flow tests alongside the existing checks:

```sh
cargo test
cargo build --release
python3 tests/remote.py -v
python3 tests/installer.py -v
python3 tests/integration.py -v
```

The offline tests cover failures and concurrency separately from the live Linux validation above. Other remote OS/architecture combinations have not been runtime-tested. This work does not publish or modify any GitHub release; the local source-build requirement above remains until a suitable new release is explicitly published.
