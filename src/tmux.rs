use std::process::ExitStatus;

use tokio::io;

#[derive(Clone)]
pub struct Tmux {
    pub command: String,
    pub socket_name: String,
}

impl Default for Tmux {
    fn default() -> Self {
        Self::new("tmux", env!("CARGO_PKG_NAME"))
    }
}

impl Tmux {
    pub fn new(command: &str, socket_name: &str) -> Self {
        Tmux {
            command: String::from(command),
            socket_name: String::from(socket_name),
        }
    }

    pub async fn execute(&self, arguments: &[String]) -> io::Result<ExitStatus> {
        tokio::process::Command::new(&self.command)
            .args(arguments)
            .spawn()?
            .wait()
            .await
    }

    pub async fn clean_up(&self) -> io::Result<()> {
        let _ = self.commander().push("kill-server").execute().await;
        Ok(())
    }

    pub fn commander(&self) -> Commander<'_> {
        Commander {
            tmux: self,
            arguments: Vec::default(),
        }
    }
}

pub struct Commander<'a> {
    tmux: &'a Tmux,
    arguments: Vec<String>,
}

impl Commander<'_> {
    pub fn push(&mut self, argument: &str) -> &mut Self {
        self.arguments.push(String::from(argument));
        self
    }

    pub async fn execute(&self) -> io::Result<ExitStatus> {
        self.tmux.execute(&self.arguments).await
    }
}
