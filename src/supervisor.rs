use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Result, atomic_write,
    protocol::{self, Request, Response},
    random_id,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    workspaces: Vec<Workspace>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Workspace {
    id: String,
    name: String,
    directory: String,
}

pub fn run(home: &Path) -> Result<()> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(home.join("supervisor.lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("a supervisor already owns this state directory".into());
    }
    // Do not leak the supervisor lock into the independently surviving tmux server.
    let mut state = load_state(home)?;
    atomic_write(
        &home.join("tmux.conf"),
        b"set -g prefix C-a\nunbind C-b\nbind a send-prefix\n",
    )?;
    let socket_path = home.join("control.sock");
    match fs::symlink_metadata(&socket_path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(&socket_path)?,
        Ok(_) => return Err("control.sock exists but is not a socket".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
    let instance = random_id()?;
    for socket in listener.incoming() {
        let mut socket = socket?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        socket.set_write_timeout(Some(Duration::from_secs(2)))?;
        // shortcut: local prototype serves one bounded request at a time; add concurrency before remote clients.
        let result = (|| -> Result<()> {
            let request: Request = protocol::read(&mut socket)?;
            let result = dispatch(home, &mut state, &instance, &request);
            let response = match result {
                Ok(result) => Response {
                    version: protocol::VERSION,
                    id: request.id,
                    result,
                    error: None,
                },
                Err(error) => Response {
                    version: protocol::VERSION,
                    id: request.id,
                    result: Value::Null,
                    error: Some(error.to_string()),
                },
            };
            protocol::write(&mut socket, &response)
        })();
        if let Err(error) = result {
            eprintln!("request failed: {error}");
        }
    }
    Ok(())
}

fn load_state(home: &Path) -> Result<State> {
    let state: State = match fs::read(home.join("metadata.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => State {
            version: 1,
            workspaces: vec![],
        },
        Err(error) => return Err(error.into()),
    };
    if state.version != 1 {
        return Err("incompatible metadata version; saved state was not modified".into());
    }
    if state.workspaces.len() > 128 {
        return Err(
            "saved state exceeds prototype limit of 128 workspaces; saved state was not modified"
                .into(),
        );
    }
    for (index, workspace) in state.workspaces.iter().enumerate() {
        validate_name(&workspace.name)?;
        if workspace.id.len() != 32
            || !workspace.id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !Path::new(&workspace.directory).is_absolute()
        {
            return Err("invalid saved workspace; saved state was not modified".into());
        }
        if state.workspaces[..index].iter().any(|other| {
            other.id == workspace.id
                || other.name == workspace.name
                || other.id == workspace.name
                || other.name == workspace.id
        }) {
            return Err("duplicate saved workspace; saved state was not modified".into());
        }
    }
    Ok(state)
}

fn dispatch(home: &Path, state: &mut State, instance: &str, request: &Request) -> Result<Value> {
    if request.version != protocol::VERSION {
        return Err("unsupported protocol version (expected 1)".into());
    }
    if request.id.is_empty() || request.id.len() > 64 {
        return Err("request id must contain 1..64 bytes".into());
    }
    match (request.method.as_str(), request.args.as_slice()) {
        ("status", []) => Ok(
            json!({"protocol": protocol::VERSION, "instance": instance, "pid": std::process::id(), "capabilities": ["workspace.list", "workspace.create", "workspace.resolve"]}),
        ),
        ("workspace.list", []) => {
            let mut workspaces = vec![];
            for workspace in &state.workspaces {
                workspaces.push(json!({"id": workspace.id, "name": workspace.name, "running": is_running(home, &workspace.id)?}));
            }
            Ok(json!(workspaces))
        }
        ("workspace.create", [name, directory]) => create(home, state, name, directory),
        ("workspace.resolve", [reference]) => {
            let workspace = state
                .workspaces
                .iter()
                .find(|workspace| &workspace.id == reference || &workspace.name == reference)
                .ok_or("workspace not found")?;
            if !is_running(home, &workspace.id)? {
                return Err("workspace is stopped; layout restoration is not implemented".into());
            }
            Ok(
                json!({"socket": home.join("tmux.sock"), "session": session_name(&workspace.id), "id": workspace.id}),
            )
        }
        _ => Err("unknown method or invalid arguments".into()),
    }
}

fn create(home: &Path, state: &mut State, name: &str, directory: &str) -> Result<Value> {
    validate_name(name)?;
    if state.workspaces.len() >= 128 {
        return Err("prototype limit of 128 workspaces reached".into());
    }
    if state
        .workspaces
        .iter()
        .any(|workspace| workspace.name == name || workspace.id == name)
    {
        return Err("workspace name conflicts with an existing name or ID".into());
    }
    if !Path::new(directory).is_absolute() || !Path::new(directory).is_dir() {
        return Err("working directory must be an existing absolute directory".into());
    }
    let workspace = Workspace {
        id: random_id()?,
        name: name.into(),
        directory: fs::canonicalize(directory)?
            .to_str()
            .ok_or("directory must be UTF-8")?
            .into(),
    };
    // Persist intent before launching a process, so a crash cannot orphan a managed session.
    let mut workspaces = state.workspaces.clone();
    workspaces.push(workspace.clone());
    let saved = State {
        version: 1,
        workspaces,
    };
    atomic_write(
        &home.join("metadata.json"),
        &serde_json::to_vec_pretty(&saved)?,
    )?;
    *state = saved;
    let output = tmux(
        home,
        &[
            "new-session",
            "-d",
            "-s",
            &session_name(&workspace.id),
            "-c",
            &workspace.directory,
        ],
    )?;
    if !output.status.success() {
        return Err(format!(
            "workspace definition saved, but tmux could not start it: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(json!(workspace))
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        return Err("workspace name must contain 1..128 bytes with no control characters or surrounding whitespace".into());
    }
    Ok(())
}

fn is_running(home: &Path, id: &str) -> Result<bool> {
    let output = tmux(
        home,
        &["has-session", "-t", &format!("={}", session_name(id))],
    )?;
    if output.status.success() {
        return Ok(true);
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("can't find session")
        || error.contains("no server running")
        || error.contains("error connecting") && !home.join("tmux.sock").exists()
    {
        return Ok(false);
    }
    Err(format!("tmux session check failed: {}", error.trim()).into())
}

fn session_name(id: &str) -> String {
    format!("workspace-{id}")
}

fn tmux(home: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new("tmux")
        .env_remove("TMUX")
        .arg("-S")
        .arg(home.join("tmux.sock"))
        .arg("-f")
        .arg(home.join("tmux.conf"))
        .args(args)
        .output()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_workspace_names() {
        for name in ["", " leading", "trailing ", "line\nbreak", "tab\tname"] {
            assert!(validate_name(name).is_err());
        }
        for name in ["demo", "with spaces", "a'b;$(touch nope)", "日本語"] {
            assert!(validate_name(name).is_ok());
        }
        assert!(validate_name(&"a".repeat(129)).is_err());
    }
}
