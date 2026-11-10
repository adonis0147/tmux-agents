use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, atomic_write, protocol};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    version: u32,
    machines: Vec<Machine>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Machine {
    destination: String,
    release_tag: String,
}

pub fn run(home: &Path, args: &[String]) -> Result<()> {
    match args {
        [method, destination, tag] if method == "register" => register(home, destination, tag),
        [method] if method == "list" => list(home),
        _ => Err("usage: tmux-agents machine list|register DESTINATION RELEASE_TAG".into()),
    }
}

fn register(home: &Path, destination: &str, tag: &str) -> Result<()> {
    validate_destination(destination)?;
    validate_tag(tag)?;
    let mut bytes = Vec::new();
    io::stdin()
        .take(protocol::MAX_MESSAGE as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > protocol::MAX_MESSAGE {
        return Err("remote status exceeds 64 KiB".into());
    }
    validate_status(&serde_json::from_slice(&bytes)?)?;

    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(home.join("machines.lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut registry = load(home)?;
    if let Some(machine) = registry
        .machines
        .iter_mut()
        .find(|machine| machine.destination == destination)
    {
        machine.release_tag = tag.into();
    } else {
        if registry.machines.len() >= 128 {
            return Err("prototype limit of 128 machines reached".into());
        }
        registry.machines.push(Machine {
            destination: destination.into(),
            release_tag: tag.into(),
        });
    }
    atomic_write(
        &home.join("machines.json"),
        &serde_json::to_vec_pretty(&registry)?,
    )?;
    println!("Registered {destination} ({tag}); remote supervisor is ready.");
    Ok(())
}

fn validate_destination(destination: &str) -> Result<()> {
    if destination.is_empty()
        || destination.len() > 255
        || destination.starts_with('-')
        || !destination
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.@:%+-".contains(&byte))
    {
        return Err("destination must be an SSH host or user@host, without options, whitespace, or shell characters".into());
    }
    Ok(())
}

fn validate_tag(tag: &str) -> Result<()> {
    if tag.is_empty()
        || tag.len() > 128
        || !tag.as_bytes()[0].is_ascii_alphanumeric()
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err("invalid release tag".into());
    }
    Ok(())
}

fn validate_status(status: &Value) -> Result<()> {
    if status["protocol"].as_u64() != Some(protocol::VERSION as u64) {
        return Err("remote supervisor protocol is incompatible".into());
    }
    let instance = status["instance"]
        .as_str()
        .ok_or("remote supervisor is missing its instance identity")?;
    if instance.len() != 32
        || !instance.bytes().all(|byte| byte.is_ascii_hexdigit())
        || status["pid"].as_u64().unwrap_or(0) == 0
    {
        return Err("invalid remote supervisor identity".into());
    }
    let capabilities = status["capabilities"]
        .as_array()
        .ok_or("remote supervisor is missing capabilities")?;
    for required in ["workspace.list", "workspace.create", "workspace.resolve"] {
        if !capabilities
            .iter()
            .any(|capability| capability.as_str() == Some(required))
        {
            return Err(format!("remote supervisor is missing capability {required}").into());
        }
    }
    Ok(())
}

fn load(home: &Path) -> Result<Registry> {
    let registry: Registry = match fs::read(home.join("machines.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Registry {
            version: 1,
            machines: vec![],
        },
        Err(error) => return Err(error.into()),
    };
    if registry.version != 1 || registry.machines.len() > 128 {
        return Err("incompatible machine registry; saved state was not modified".into());
    }
    for (index, machine) in registry.machines.iter().enumerate() {
        validate_destination(&machine.destination)?;
        validate_tag(&machine.release_tag)?;
        if registry.machines[..index]
            .iter()
            .any(|other| other.destination == machine.destination)
        {
            return Err("duplicate saved machine; saved state was not modified".into());
        }
    }
    Ok(registry)
}

fn list(home: &Path) -> Result<()> {
    for machine in load(home)?.machines {
        println!("{}\t{}", machine.destination, machine.release_tag);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_ssh_destinations_and_release_tags() {
        for destination in ["host", "user@192.0.2.55", "my-host", "user@::1"] {
            assert!(validate_destination(destination).is_ok());
        }
        for destination in [
            "",
            "-oProxyCommand=evil",
            "host\nother",
            "host;touch",
            "$(id)",
            "host name",
        ] {
            assert!(validate_destination(destination).is_err());
        }
        for tag in ["pre-0.1", "v0.1.0"] {
            assert!(validate_tag(tag).is_ok());
        }
        for tag in ["", "../outside", "two lines\n", "-bad"] {
            assert!(validate_tag(tag).is_err());
        }
    }

    #[test]
    fn validates_remote_handshake() {
        let valid = json!({"protocol":1,"instance":"0123456789abcdef0123456789abcdef","pid":42,"capabilities":["workspace.list","workspace.create","workspace.resolve"]});
        assert!(validate_status(&valid).is_ok());
        for (field, value) in [
            ("protocol", json!(2)),
            ("instance", json!("bad")),
            ("pid", json!(0)),
            ("capabilities", json!([])),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(validate_status(&invalid).is_err());
        }
    }
}
