# Remote machine registration

```sh
tmux-agents machine add user@host
```

This verifies a remote machine and saves its SSH destination locally. It does not
create remote sessions, install software, or copy keys. To open its workspace,
run `tmux-agents` and select the machine using the [native workspace menu](switching.md).

Registration can run inside or outside tmux and does not require a terminal when
SSH authentication is already available. Run it on the originating local machine
to update that launcher's menu; running it in a remote shell updates the remote
machine's own registry instead.

## SSH configuration

Use the normal OpenSSH configuration for keys, usernames, ports, aliases, jump
hosts, and connection sharing:

```sshconfig
Host buildbox
    HostName build.example.com
    User developer
    Port 2222
    IdentityFile ~/.ssh/id_ed25519
    ProxyJump gateway
```

Then register it with:

```sh
tmux-agents machine add buildbox
```

Supported destinations are `[user@]host`, including unbracketed IPv6 addresses such
as `developer@2001:db8::1`. Host aliases and usernames accept ASCII letters,
numbers, dots, underscores, and hyphens, and cannot start with a hyphen. IPv6 zone
identifiers such as `%eth0` are accepted. For other username forms, configure
`User` under an SSH alias. Inline ports, SSH URLs, whitespace, and shell syntax
are rejected; configure ports in SSH configuration instead.

The SSH destination is passed as a literal argument after `--`, never inserted
into a remote shell command. The dependency probe is a fixed POSIX shell script.
SSH runs without a remote PTY, with a 10-second connection timeout and one
connection attempt. Password, host-key, and key-passphrase prompts still use
OpenSSH's normal terminal handling; authentication prompts can wait for input.
Host-key checking is not disabled. SSH errors remain visible.

## Remote requirements

The remote noninteractive SSH command environment must provide:

- A POSIX `sh`.
- Stable tmux 3.6 or newer, including patch releases such as `3.6a`.
- `tmux-agents` in `PATH`, with the same version as the local executable.

Workspace switching also requires the current build on both machines. Registration
checks release versions, not build hashes; reinstall both sides when testing
unreleased changes that share a version number.

The tmux minimum conservatively matches the version tested by this project.
Check your local application version using `tmux-agents --version`. On the remote,
the probe runs `tmux -V` and `tmux-agents --version`. Version checks do not launch
or alter a tmux server. Remote shell initialization must not print extra text to
stdout; unexpected probe output is rejected rather than silently accepted.

A failed connection, missing dependency, incompatible version, or invalid registry
does not add the machine or overwrite the registry. Existing registrations are
checked again when re-added, but exact duplicates are not written twice. Different
SSH aliases for the same host remain separate entries.

## Registry storage

The versioned registry is stored at:

```text
${XDG_CONFIG_HOME}/tmux-agents/machines.json
```

If `XDG_CONFIG_HOME` is unset or empty, use `~/.config/tmux-agents/machines.json`.
The base directory must be absolute. The file contains only destinations:

```json
{
  "version": 1,
  "machines": ["developer@build.example.com", "buildbox"]
}
```

New application directories have mode `0700`; registry and lock files have mode
`0600`. A lock serializes concurrent registrations. Writes use a synced temporary
file and atomic rename. Malformed, duplicate, or unsupported registry contents
are rejected and left unchanged. Credentials and SSH settings stay in SSH's own
configuration and agent, not this file.

## Checks

```sh
python3 tests/test_machines.py
python3 tests/test_startup.py
python3 tests/test_switching.py
```

Registration tests use an SSH stub, not real network connections. They verify
argument boundaries, destination validation, dependency checks, version rejection,
SSH errors, registry permissions, duplicate handling, malformed registry
preservation, concurrent writes, and configuration paths. The dependency script
is also executed through a local POSIX shell with controlled remote executables.
The switching suite additionally tests actual SSH against an isolated loopback
server when `sshd` is available. Your own host is still needed to validate its
authentication and deployment.
