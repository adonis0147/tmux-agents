use crate::protocol::{self, Protocol};
use crate::tmux::Tmux;

use pty_process::Size;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
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
        let (incoming, mut outgoing) = self.stream.into_split();
        let mut protocol = Protocol::new(incoming);
        let (mut master, slave) = pty_process::open().map_err(io::Error::other)?;

        let mut child = pty_process::Command::new(&self.tmux.command)
            .args([
                "-L",
                &self.tmux.socket_name,
                "new-session",
                "-A",
                "-s",
                &self.tmux.session_name,
            ])
            .kill_on_drop(true)
            .spawn(slave)
            .map_err(io::Error::other)?;

        let mut buffer = [0u8; protocol::MAX_FRAME_LENGTH];
        loop {
            tokio::select! {
                message = protocol.receive() => {
                    let message = message?;
                    match message {
                        None => break,
                        Some(protocol::Message::Input(data)) => {
                            master.write_all(&data).await?;
                        },
                        Some(protocol::Message::Resize { rows, cols }) => {
                            master.resize(Size::new(rows, cols)).map_err(io::Error::other)?;
                        },
                        _ => unimplemented!(),
                    }
                },
                count = master.read(&mut buffer) => {
                    let count = count?;
                    outgoing.write_all(&buffer[..count]).await?;
                },
                result = child.wait() => {
                    result.map_err(io::Error::other)?;
                    break;
                }
            }
        }
        Ok(())
    }
}
