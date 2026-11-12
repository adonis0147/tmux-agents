use futures_util::{SinkExt, StreamExt};
use std::io;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

#[derive(Debug, bincode::Encode, bincode::Decode)]
pub enum Message {
    Input(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    StopServer,
}

pub const MAX_FRAME_LENGTH: usize = 8 * 1024;

pub struct Protocol<S> {
    framed: Framed<S, LengthDelimitedCodec>,
}

impl<S> Protocol<S> {
    pub fn new(stream: S) -> Self {
        let codec = LengthDelimitedCodec::builder()
            .max_frame_length(MAX_FRAME_LENGTH)
            .new_codec();
        Self {
            framed: Framed::new(stream, codec),
        }
    }
}

impl<S: AsyncWrite + Unpin> Protocol<S> {
    pub async fn send(&mut self, message: &Message) -> io::Result<()> {
        let payload = bincode::encode_to_vec(message, bincode::config::standard())
            .map_err(io::Error::other)?;
        self.framed.send(payload.as_slice()).await
    }
}

impl<S: AsyncRead + Unpin> Protocol<S> {
    pub async fn receive(&mut self) -> io::Result<Option<Message>> {
        let Some(frame) = self.framed.next().await else {
            return Ok(None);
        };

        let frame = frame?;
        let (message, consumed): (Message, usize) =
            bincode::decode_from_slice(&frame, bincode::config::standard())
                .map_err(io::Error::other)?;

        if consumed != frame.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame contains trailing bytes",
            ));
        }

        Ok(Some(message))
    }
}
