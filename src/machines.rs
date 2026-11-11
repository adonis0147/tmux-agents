use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::Ipv6Addr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REMOTE_PROBE: &str = r#"sh -c '
command -v tmux >/dev/null 2>&1 || { echo "remote tmux is not installed or not in PATH" >&2; exit 1; }
command -v tmux-agents >/dev/null 2>&1 || { echo "remote tmux-agents is not installed or not in PATH" >&2; exit 1; }
tmux -V && tmux-agents --version
'"#;

#[derive(Deserialize, Serialize)]
struct Registry {
    version: u32,
    machines: Vec<String>,
}

pub fn add(destination: &str) -> Result<(), String> {
    validate_destination(destination)?;
    let directory = config_directory()?;
    probe(destination)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .map_err(|error| {
            format!(
                "cannot create configuration directory {}: {error}",
                directory.display()
            )
        })?;
    let _lock = lock(&directory)?;
    let mut registry = read_registry(&directory)?;
    if registry
        .machines
        .iter()
        .any(|machine| machine == destination)
    {
        println!("Machine {destination} is already registered (SSH checks passed).");
        return Ok(());
    }
    registry.machines.push(destination.to_owned());
    write_registry(&directory, &registry)?;
    println!("Added machine {destination}.");
    Ok(())
}

pub fn registered() -> Result<Vec<String>, String> {
    registered_in(&config_directory()?)
}

pub fn registered_in(directory: &Path) -> Result<Vec<String>, String> {
    Ok(read_registry(directory)?.machines)
}

pub fn validate_destination(destination: &str) -> Result<(), String> {
    let host = if let Some((user, host)) = destination.split_once('@') {
        if !valid_name(user) {
            return Err("invalid SSH user; use [user@]host or an SSH alias".into());
        }
        host
    } else {
        destination
    };
    let valid_host = if host.contains(':') {
        let (address, zone) = host
            .split_once('%')
            .map_or((host, None), |(address, zone)| (address, Some(zone)));
        address.parse::<Ipv6Addr>().is_ok() && zone.is_none_or(valid_name)
    } else {
        valid_name(host)
    };
    if !valid_host {
        return Err(
            "invalid SSH host; use [user@]host or an SSH alias (configure ports in ~/.ssh/config)"
                .into(),
        );
    }
    Ok(())
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub fn config_directory() -> Result<PathBuf, String> {
    if let Some(base) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        let base = PathBuf::from(base);
        if !base.is_absolute() {
            return Err("XDG_CONFIG_HOME must be an absolute path".into());
        }
        return Ok(base.join("tmux-agents"));
    }
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or("HOME is not set")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err("HOME must be an absolute path".into());
    }
    Ok(home.join(".config/tmux-agents"))
}

fn probe(destination: &str) -> Result<(), String> {
    // Only the destination varies; no user input is interpolated into the remote shell command.
    let output = ssh_command(destination, SshMode::Probe)?
        .arg(REMOTE_PROBE)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| {
            format!("cannot execute ssh: {error}; make sure OpenSSH is available in PATH")
        })?;
    if !output.status.success() {
        return Err(format!(
            "SSH checks failed for {destination} ({}); machine was not added",
            output.status
        ));
    }
    let text =
        String::from_utf8(output.stdout).map_err(|_| "remote probe output is not valid UTF-8")?;
    let lines: Vec<_> = text.lines().collect();
    if lines.len() != 2 {
        return Err(
            "unexpected remote probe output; expected tmux and tmux-agents versions".into(),
        );
    }
    validate_tmux_version(lines[0])?;
    let expected = format!("tmux-agents {}", env!("CARGO_PKG_VERSION"));
    if lines[1] != expected {
        return Err(format!(
            "incompatible remote tmux-agents: expected {expected}, received {:?}",
            lines[1]
        ));
    }
    Ok(())
}

pub enum SshMode {
    Probe,
    Resume,
    Interactive,
}

pub fn ssh_command(destination: &str, mode: SshMode) -> Result<Command, String> {
    validate_destination(destination)?;
    let mut command = Command::new("ssh");
    let automatic = matches!(mode, SshMode::Resume);
    command.args([
        if matches!(mode, SshMode::Probe) {
            "-T"
        } else {
            "-tt"
        },
        "-o",
        if automatic {
            "ConnectTimeout=3"
        } else {
            "ConnectTimeout=10"
        },
        "-o",
        "ConnectionAttempts=1",
    ]);
    if automatic {
        command.args(["-o", "BatchMode=yes"]);
    }
    if !matches!(mode, SshMode::Probe) {
        command.args(["-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2"]);
    }
    command.args(["--", destination]);
    Ok(command)
}

fn validate_tmux_version(version: &str) -> Result<(), String> {
    let parsed = version.strip_prefix("tmux ").and_then(|version| {
        let (major, minor) = version.split_once('.')?;
        let minor = minor.trim_end_matches(|character: char| character.is_ascii_lowercase());
        Some((major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?))
    });
    if parsed.is_none_or(|version| version < (3, 6)) {
        return Err(format!(
            "remote tmux 3.6 or newer is required; received {version:?}"
        ));
    }
    Ok(())
}

fn lock(directory: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join("machines.lock"))
        .map_err(|error| format!("cannot open machine registry lock: {error}"))?;
    file.lock()
        .map_err(|error| format!("cannot lock machine registry: {error}"))?;
    Ok(file)
}

fn read_registry(directory: &Path) -> Result<Registry, String> {
    let path = directory.join("machines.json");
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Registry {
                version: 1,
                machines: Vec::new(),
            });
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let registry: Registry = serde_json::from_slice(&data).map_err(|error| {
        format!(
            "invalid machine registry {}: {error}; file left unchanged",
            path.display()
        )
    })?;
    if registry.version != 1 {
        return Err(format!(
            "unsupported machine registry version {}; file left unchanged",
            registry.version
        ));
    }
    let mut destinations = HashSet::new();
    for destination in &registry.machines {
        validate_destination(destination).map_err(|error| {
            format!(
                "invalid machine registry {}: {error}; file left unchanged",
                path.display()
            )
        })?;
        if !destinations.insert(destination) {
            return Err("duplicate machine in registry; file left unchanged".into());
        }
    }
    Ok(registry)
}

fn write_registry(directory: &Path, registry: &Registry) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(registry)
        .map_err(|error| format!("cannot serialize machine registry: {error}"))?;
    let temporary = directory.join(format!("machines.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join("machines.json"))?;
        File::open(directory)?.sync_all()
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot save machine registry: {error}"));
    }
    Ok(())
}
