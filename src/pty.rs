use crate::tmux::Tmux;
use tokio::io;
use tokio::net::UnixStream;

pub struct Pty {
    stream: UnixStream,
    tmux: Tmux,
}

impl Pty {
    pub fn new(stream: UnixStream, tmux: Tmux) -> Self {
        Self { stream, tmux }
    }
    pub async fn run(self) -> io::Result<()> {
        Ok(())
    }
}
