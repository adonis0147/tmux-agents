use std::{ffi::OsStr, process::ExitStatus};
use tokio::io;

#[derive(Clone)]
pub struct Tmux {
    pub command: String,
    pub socket_name: String,
    pub session_name: String,
}

impl Default for Tmux {
    fn default() -> Self {
        Self::new("tmux", env!("CARGO_PKG_NAME"), "local")
    }
}

impl Tmux {
    pub fn new(command: &str, socket_name: &str, session_name: &str) -> Self {
        Tmux {
            command: String::from(command),
            socket_name: String::from(socket_name),
            session_name: String::from(session_name),
        }
    }

    pub async fn execute<I, S>(&self, arguments: I) -> io::Result<ExitStatus>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        tokio::process::Command::new(&self.command)
            .args(arguments)
            .spawn()?
            .wait()
            .await
    }

    pub async fn clean_up(&self) -> io::Result<()> {
        let _ = self.execute(["kill-server"]).await;
        Ok(())
    }
}
