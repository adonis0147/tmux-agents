use crate::snapshot::Snapshot;
use crate::tmux::{Tmux, directory_argument, literal_format, login_shell, shell_quote};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

pub fn prepare(tmux: &Tmux) -> Result<(PathBuf, String), String> {
    let directory = state_directory()?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .map_err(|error| {
            format!(
                "cannot create state directory {}: {error}",
                directory.display()
            )
        })?;
    let _lock = lock(&directory)?;
    if !tmux.has_sessions()? {
        let current = env::current_dir()
            .map_err(|error| format!("cannot read working directory: {error}"))?;
        let selected = match read_snapshot(&directory)? {
            Some(snapshot) => snapshot.restore(tmux, &current)?,
            None => tmux.output(&[
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{session_id}",
                "-s",
                "main",
                "-c",
                &directory_argument(&current)?,
                &login_shell()?,
                "-l",
            ])?,
        };
        tmux.output(&["set-option", "-g", "@ta_last_session", &selected])?;
    }
    let selected = selected_session(tmux)?;
    install_hooks(tmux, &directory, &selected)?;
    save(tmux, &directory)?;
    Ok((directory, selected))
}

fn state_directory() -> Result<PathBuf, String> {
    if let Some(base) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        let base = PathBuf::from(base);
        if !base.is_absolute() {
            return Err("XDG_STATE_HOME must be an absolute path".into());
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
    Ok(home.join(".local/state/tmux-agents"))
}

fn lock(directory: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join("snapshot.lock"))
        .map_err(|error| format!("cannot open snapshot lock: {error}"))?;
    file.lock()
        .map_err(|error| format!("cannot lock workspace state: {error}"))?;
    Ok(file)
}

fn read_snapshot(directory: &Path) -> Result<Option<Snapshot>, String> {
    let path = directory.join("workspace.json");
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let snapshot: Snapshot = serde_json::from_slice(&data).map_err(|error| {
        format!(
            "invalid workspace snapshot {}: {error}; file left unchanged",
            path.display()
        )
    })?;
    snapshot
        .validate()
        .map_err(|error| format!("{error} in {}; file left unchanged", path.display()))?;
    Ok(Some(snapshot))
}

fn selected_session(tmux: &Tmux) -> Result<String, String> {
    let sessions = tmux.output(&[
        "list-sessions",
        "-F",
        "#{session_id} #{session_last_attached}",
    ])?;
    let remembered = tmux
        .output(&["show-options", "-gv", "@ta_last_session"])
        .unwrap_or_default();
    let mut latest = None;
    for line in sessions.lines() {
        let (id, timestamp) = line.split_once(' ').ok_or("invalid session metadata")?;
        if id == remembered {
            return Ok(id.to_owned());
        }
        let timestamp = timestamp.parse::<u64>().unwrap_or(0);
        if latest.as_ref().is_none_or(|(_, time)| timestamp >= *time) {
            latest = Some((id.to_owned(), timestamp));
        }
    }
    latest
        .map(|(id, _)| id)
        .ok_or_else(|| "no workspace sessions available".into())
}

fn install_hooks(tmux: &Tmux, directory: &Path, selected: &str) -> Result<(), String> {
    let executable =
        env::current_exe().map_err(|error| format!("cannot find launcher executable: {error}"))?;
    let socket = tmux.display(selected, "#{socket_path}")?;
    let callback = format!(
        "{} --internal-snapshot {} {}",
        shell_quote(executable.as_os_str())?,
        shell_quote(directory.as_os_str())?,
        shell_quote(Path::new(&socket).as_os_str())?,
    );
    let callback = literal_format(&callback);
    let command = format!("run-shell -b {}", shell_quote(callback.as_ref())?);
    for hook in [
        "window-layout-changed",
        "window-pane-changed",
        "session-window-changed",
        "session-created",
        "session-closed",
        "session-renamed",
        "window-linked",
        "window-unlinked",
        "window-renamed",
        "client-detached",
    ] {
        tmux.output(&["set-hook", "-g", &format!("{hook}[9876]"), &command])?;
    }
    for hook in ["client-attached", "client-session-changed"] {
        tmux.output(&[
            "set-hook",
            "-g",
            &format!("{hook}[9876]"),
            &format!("set-option -gF @ta_last_session '#{{session_id}}' ; {command}"),
        ])?;
    }
    Ok(())
}

fn save(tmux: &Tmux, directory: &Path) -> Result<(), String> {
    let selected = selected_session(tmux)?;
    tmux.output(&["set-option", "-g", "@ta_last_session", &selected])?;
    let snapshot = Snapshot::capture(tmux)?;
    let data = serde_json::to_vec_pretty(&snapshot)
        .map_err(|error| format!("cannot serialize workspace: {error}"))?;
    let temporary = directory.join(format!("workspace.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&temporary, directory.join("workspace.json"))?;
        File::open(directory)?.sync_all()
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot save workspace snapshot: {error}"));
    }
    Ok(())
}

pub fn save_after_detach(tmux: &Tmux, directory: &Path) -> Result<(), String> {
    let _lock = lock(directory)?;
    if tmux.has_sessions()? {
        save(tmux, directory)?;
    }
    Ok(())
}

pub fn save_debounced(tmux: &Tmux, directory: &Path) -> Result<(), String> {
    let pending = directory.join("snapshot.pending");
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&pending)
        .map_err(|error| format!("cannot schedule workspace snapshot: {error}"))?;
    let _lock = lock(directory)?;
    if !pending.exists() {
        return Ok(());
    }
    thread::sleep(Duration::from_millis(200));
    fs::remove_file(&pending).map_err(|error| format!("cannot clear pending snapshot: {error}"))?;
    if tmux.has_sessions()? {
        save(tmux, directory)?;
    }
    Ok(())
}
