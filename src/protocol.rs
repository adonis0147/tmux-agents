use std::io::{Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::Result;

pub const VERSION: u32 = 1;
pub const MAX_MESSAGE: usize = 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub id: String,
    pub method: String,
    pub args: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u32,
    pub id: String,
    pub result: Value,
    pub error: Option<String>,
}

pub fn read<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;

    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_MESSAGE {
        return Err("invalid message size".into());
    }

    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;

    Ok(serde_json::from_slice(&bytes)?)
}

pub fn write(writer: &mut impl Write, message: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(message)?;
    if bytes.len() > MAX_MESSAGE {
        return Err("message exceeds 64 KiB".into());
    }

    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;

    Ok(())
}

pub async fn read_async<T: DeserializeOwned>(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<T> {
    use tokio::io::AsyncReadExt;

    let size = reader.read_u32().await? as usize;
    if size == 0 || size > MAX_MESSAGE {
        return Err("invalid message size".into());
    }

    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;

    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn write_async(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    message: &impl Serialize,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let bytes = serde_json::to_vec(message)?;
    if bytes.len() > MAX_MESSAGE {
        return Err("message exceeds 64 KiB".into());
    }

    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip() {
        let request = Request {
            version: VERSION,
            id: "test".into(),
            method: "status".into(),
            args: vec![],
        };

        let mut bytes = Vec::new();
        write(&mut bytes, &request).unwrap();

        let result: Request = read(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(result.id, "test");
    }

    #[test]
    fn rejects_invalid_frames() {
        for bytes in [
            vec![0, 1, 0, 1],
            vec![0, 0, 0, 0],
            vec![0, 0, 0, 2, b'{'],
            vec![0, 0, 0, 1, b'x'],
        ] {
            assert!(read::<Request>(&mut Cursor::new(bytes)).is_err());
        }
    }
}
