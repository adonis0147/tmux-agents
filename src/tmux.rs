use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Tmux {
    socket: Option<PathBuf>,
}

impl Tmux {
    pub fn named() -> Self {
        Self { socket: None }
    }

    pub fn at(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        match &self.socket {
            Some(socket) => command.arg("-S").arg(socket),
            None => command.args(["-L", "tmux-agents"]),
        };
        command
    }

    pub fn output(&self, arguments: &[&str]) -> Result<String, String> {
        let output = self.command().args(arguments).output().map_err(|error| {
            format!(
                "cannot execute tmux: {error}; make sure tmux is installed and available in PATH"
            )
        })?;
        if !output.status.success() {
            return Err(format!(
                "tmux {}: {}",
                arguments[0],
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let text =
            String::from_utf8(output.stdout).map_err(|_| "tmux returned non-UTF-8 metadata")?;
        Ok(text.strip_suffix('\n').unwrap_or(&text).to_owned())
    }

    pub fn has_sessions(&self) -> Result<bool, String> {
        let output = self.command().arg("has-session").output().map_err(|error| {
            format!("cannot execute tmux: {error}; make sure tmux is installed and available in PATH")
        })?;
        Ok(output.status.success())
    }

    pub fn display(&self, target: &str, format: &str) -> Result<String, String> {
        self.output(&["display-message", "-p", "-t", target, format])
    }
}

pub fn login_shell() -> Result<String, String> {
    let shell = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));
    shell
        .into_os_string()
        .into_string()
        .map_err(|_| "shell path must be valid UTF-8".into())
}

pub fn shell_quote(value: &OsStr) -> Result<String, String> {
    let value = value.to_str().ok_or("hook paths must be valid UTF-8")?;
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

pub fn directory_argument(directory: &Path) -> Result<String, String> {
    let directory = directory
        .to_str()
        .ok_or("working directories must be valid UTF-8")?;
    Ok(literal_format(directory))
}

pub fn literal_format(value: &str) -> String {
    // tmux expands formats in names, directories, and run-shell commands independently of shell quoting.
    value.replace('#', "##")
}
