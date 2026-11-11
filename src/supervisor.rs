use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::{
    Result, atomic_write,
    protocol::{self, Request, Response},
    random_id,
    recovery::{self, State, Workspace},
};

const MAX_CLIENTS: usize = 32;
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);

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

    // Validate before touching configuration, sockets, or tmux. The lock is close-on-exec.
    let mut state = recovery::load(home)?;
    atomic_write(
        &home.join("tmux.conf"),
        b"set -g prefix C-a\nunbind C-b\nbind a send-prefix\nset -g remain-on-exit on\n\
          set -g default-shell /bin/bash\nset -g default-command ''\n",
    )?;
    if let Err(error) = recovery::reconcile(home, &mut state) {
        eprintln!("recovery deferred: {error}");
    }

    let socket_path = home.join("control.sock");
    match fs::symlink_metadata(&socket_path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(&socket_path)?,
        Ok(_) => return Err("control.sock exists but is not a socket".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }

    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
    let instance = Arc::new(random_id()?);
    let home = Arc::new(home.to_path_buf());
    let state = Arc::new(Mutex::new(state));
    let clients = Arc::new(AtomicUsize::new(0));

    {
        let home = Arc::clone(&home);
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let mut last_error = String::new();
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let mut state = state.lock().unwrap();
                match recovery::reconcile(&home, &mut state) {
                    Ok(()) => last_error.clear(),
                    Err(error) => {
                        let message = error.to_string();
                        if message != last_error {
                            eprintln!("recovery deferred: {message}");
                            last_error = message;
                        }
                    }
                }
            }
        });
    }

    for socket in listener.incoming() {
        let socket = socket?;
        if clients.fetch_add(1, Ordering::SeqCst) >= MAX_CLIENTS {
            clients.fetch_sub(1, Ordering::SeqCst);
            drop(socket);
            continue;
        }
        let home = Arc::clone(&home);
        let state = Arc::clone(&state);
        let instance = Arc::clone(&instance);
        let clients = Arc::clone(&clients);
        std::thread::spawn(move || {
            // Always release the slot, including after malformed input, timeout, or disconnect.
            struct ClientSlot(Arc<AtomicUsize>);
            impl Drop for ClientSlot {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _slot = ClientSlot(clients);
            let _ = serve(socket, &home, &state, &instance);
        });
    }
    Ok(())
}

fn serve(mut socket: UnixStream, home: &Path, state: &Mutex<State>, instance: &str) -> Result<()> {
    socket.set_read_timeout(Some(IDLE_TIMEOUT))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    loop {
        let request: Request = protocol::read(&mut socket)?;
        let result = {
            let mut state = state.lock().map_err(|_| "supervisor state lock poisoned")?;
            dispatch(home, &mut state, instance, &request)
        };
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
        // Network backpressure never holds the state lock.
        protocol::write(&mut socket, &response)?;
    }
}

fn dispatch(home: &Path, state: &mut State, instance: &str, request: &Request) -> Result<Value> {
    if request.version != protocol::VERSION {
        return Err("unsupported protocol version (expected 1)".into());
    }
    if request.id.is_empty() || request.id.len() > 64 {
        return Err("request id must contain 1..64 bytes".into());
    }

    match (request.method.as_str(), request.args.as_slice()) {
        ("status", []) => Ok(json!({
            "protocol": protocol::VERSION,
            "instance": instance,
            "pid": std::process::id(),
            "capabilities": ["workspace.list", "workspace.create", "workspace.resolve",
                "state.snapshot", "workspace.snapshot", "workspace.restore"]
        })),
        ("state.snapshot", []) => {
            let mut workspaces = vec![];
            for workspace in &state.workspaces {
                workspaces.push(json!({
                    "id": workspace.id,
                    "name": workspace.name,
                    "running": recovery::is_running(home, &workspace.id)?,
                    "windows": workspace.windows.len(),
                    "panes": workspace.windows.iter().map(|window| window.panes.len()).sum::<usize>()
                }));
            }
            Ok(json!({"instance": instance, "revision": state.revision, "workspaces": workspaces}))
        }
        ("workspace.list", []) => {
            let mut workspaces = vec![];
            for workspace in &state.workspaces {
                workspaces.push(json!({
                    "id": workspace.id, "name": workspace.name,
                    "running": recovery::is_running(home, &workspace.id)?
                }));
            }
            Ok(json!(workspaces))
        }
        ("workspace.create", [name, directory]) => create(home, state, name, directory),
        ("workspace.create", [name]) => {
            let directory = std::env::var("HOME").map_err(|_| "remote HOME is unset")?;
            create(home, state, name, &directory)
        }
        ("workspace.snapshot", [reference]) => {
            let index = workspace_index(state, reference)?;
            let mut updated = state.clone();
            // Missing sessions must not erase the last useful snapshot.
            if recovery::is_running(home, &state.workspaces[index].id)? {
                updated.workspaces[index].windows =
                    recovery::capture(home, &state.workspaces[index])?;
                if updated != *state {
                    recovery::save(home, state, &mut updated)?;
                    *state = updated;
                }
            }
            let result = serde_json::to_value(&state.workspaces[index])?;
            if serde_json::to_vec(&result)?.len() > protocol::MAX_MESSAGE - 256 {
                return Err(
                    "workspace snapshot exceeds RPC frame limit; recovery remains saved on disk"
                        .into(),
                );
            }
            Ok(result)
        }
        ("workspace.resolve" | "workspace.restore", [reference]) => {
            let index = workspace_index(state, reference)?;
            let workspace = &state.workspaces[index];
            recovery::ensure(home, workspace)?;
            Ok(json!({"socket": home.join("tmux.sock"),
                "session": recovery::session_name(&workspace.id), "id": workspace.id}))
        }
        _ => Err("unknown method or invalid arguments".into()),
    }
}

fn workspace_index(state: &State, reference: &str) -> Result<usize> {
    state
        .workspaces
        .iter()
        .position(|workspace| workspace.id == reference || workspace.name == reference)
        .ok_or_else(|| "workspace not found".into())
}

fn create(home: &Path, state: &mut State, name: &str, directory: &str) -> Result<Value> {
    recovery::validate_name(name)?;
    if state.workspaces.len() >= 128 {
        return Err("limit of 128 workspaces reached".into());
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
        windows: vec![],
    };
    let mut updated = state.clone();
    updated.workspaces.push(workspace.clone());
    // Save intent first, so a crash cannot orphan a newly managed tmux session.
    recovery::save(home, state, &mut updated)?;
    *state = updated;
    recovery::ensure(home, &workspace)?;
    recovery::reconcile(home, state)?;
    Ok(json!(workspace))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_workspace_names() {
        for name in ["", " leading", "trailing ", "line\nbreak", "tab\tname"] {
            assert!(recovery::validate_name(name).is_err());
        }
        for name in ["demo", "with spaces", "a'b;$(touch nope)", "日本語"] {
            assert!(recovery::validate_name(name).is_ok());
        }
        assert!(recovery::validate_name(&"a".repeat(129)).is_err());
    }
}
