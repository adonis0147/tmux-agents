use crate::terminal::Terminal;
use crate::{
    connection::Connection,
    protocol::{self, Message, Protocol},
};

use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};

pub struct Client {
    connection: Connection,
}

impl Client {
    pub fn new(connection: Connection) -> Self {
        Self { connection }
    }

    pub fn connect(&self) -> impl Future<Output = Result<UnixStream, io::Error>> {
        UnixStream::connect(self.connection.socket_path.clone())
    }

    pub async fn run(&self, stream: UnixStream) -> io::Result<()> {
        let (mut incoming, outgoing) = stream.into_split();
        let mut protocol = Protocol::new(outgoing);

        let mut terminal = Terminal::new(tokio::io::stdin(), tokio::io::stdout());
        terminal.enable_raw_mode();

        let mut resize = signal(SignalKind::window_change())?;
        let (rows, cols) = terminal.size()?;
        protocol.send(&Message::Resize { rows, cols }).await?;

        let mut input = [0u8; protocol::MAX_FRAME_LENGTH];
        let mut output = [0u8; protocol::MAX_FRAME_LENGTH];
        loop {
            tokio::select! {
                Some(()) = resize.recv() => {
                    let (rows, cols) = terminal.size()?;
                    protocol.send(&Message::Resize { rows, cols }).await?;
                }
                count = terminal.stdin.read(&mut input) => {
                    let count = count?;
                    if count == 0 {
                        break;
                    }
                    protocol.send(&Message::Input(input[..count].to_vec())).await?;
                }
                count = incoming.read(&mut output) => {
                    let count = count?;
                    if count == 0 {
                        break;
                    }
                    terminal.stdout.write_all(&output[..count]).await?;
                    terminal.stdout.flush().await?;
                }
            }
        }
        Ok(())
    }
}
