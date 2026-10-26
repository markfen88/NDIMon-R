//! A single u32 big-endian length followed by UTF-8 JSON. Allocation is bounded.
use crate::MAX_FRAME_BYTES;
use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};
pub fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let body =
        serde_json::to_vec(value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    check_length(body.len())?;
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend(body);
    Ok(frame)
}
pub fn check_length(len: usize) -> io::Result<()> {
    if len == 0 || len > MAX_FRAME_BYTES {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid privd frame length",
        ))
    } else {
        Ok(())
    }
}
pub fn read<R: Read, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    check_length(len)?;
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
pub fn write<W: Write, T: Serialize>(writer: &mut W, value: &T) -> io::Result<()> {
    writer.write_all(&encode(value)?)?;
    writer.flush()
}
#[cfg(feature = "client")]
pub async fn read_async<R: tokio::io::AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> io::Result<T> {
    use tokio::io::AsyncReadExt;
    let len = reader.read_u32().await? as usize;
    check_length(len)?;
    let mut body = vec![0; len];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
#[cfg(feature = "client")]
pub async fn write_async<W: tokio::io::AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> io::Result<()> {
    use tokio::io::AsyncWriteExt;
    writer.write_all(&encode(value)?).await?;
    writer.flush().await
}
