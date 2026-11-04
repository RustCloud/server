//! What the server and a worker say to each other through the worker's
//! stdin and stdout: frames, each a length (8 bytes, little-endian) and that
//! many bytes.
//!
//! 1. The server sends the model; the worker answers [`Reply::Loaded`] or
//!    [`Reply::Failed`] (and exits).
//! 2. For each run the server sends a [`Run`], then a frame of data for each
//!    input; the worker answers [`Reply::Outputs`] and a frame of data for
//!    each output, or [`Reply::Failed`] and waits for the next run.
//!
//! The worker exits when its stdin closes: when the server stops it, or dies.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::tensor::{DType, ModelInfo};

/// The largest message that isn't data: a run's or a model's description.
pub(crate) const MAX_MESSAGE: u64 = 1 << 20;

/// A run of the model.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Run {
    pub inputs: Vec<Header>,
    /// Most bytes of all outputs together.
    pub output_limit: u64,
}

/// A tensor without its data, which follows in a frame of its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Header {
    pub name: String,
    pub dtype: DType,
    pub shape: Vec<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Reply {
    Loaded(ModelInfo),
    Outputs(Vec<Header>),
    Failed(String),
}

/// The next frame, at most `limit` bytes; none if the stream ended before it.
pub(crate) fn read_frame(reader: &mut impl Read, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut length = [0; 8];
    match reader.read_exact(&mut length) {
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other?,
    }
    let length = u64::from_le_bytes(length);
    if length > limit {
        return Err(std::io::Error::other(format!("a frame of {length} bytes is larger than allowed")));
    }
    let mut data = Vec::new();
    data.try_reserve_exact(length as usize).map_err(std::io::Error::other)?;
    reader.take(length).read_to_end(&mut data)?;
    if data.len() as u64 != length {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    Ok(Some(data))
}

pub(crate) fn write_frame(writer: &mut impl Write, data: &[u8]) -> std::io::Result<()> {
    writer.write_all(&(data.len() as u64).to_le_bytes())?;
    writer.write_all(data)
}

/// A message as a frame of JSON.
pub(crate) fn write_message(writer: &mut impl Write, message: &impl Serialize) -> std::io::Result<()> {
    write_frame(writer, &serde_json::to_vec(message).map_err(std::io::Error::other)?)
}

/// The next frame, at most `limit` bytes.
pub(crate) async fn read_frame_async(reader: &mut (impl AsyncRead + Unpin), limit: u64) -> std::io::Result<Vec<u8>> {
    let length = reader.read_u64_le().await?;
    if length > limit {
        return Err(std::io::Error::other(format!("a frame of {length} bytes is larger than allowed")));
    }
    let mut data = Vec::new();
    data.try_reserve_exact(length as usize).map_err(std::io::Error::other)?;
    reader.take(length).read_to_end(&mut data).await?;
    if data.len() as u64 != length {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    Ok(data)
}

/// The next message: a frame of JSON.
pub(crate) async fn read_message<T: serde::de::DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> std::io::Result<T> {
    let frame = read_frame_async(reader, MAX_MESSAGE).await?;
    serde_json::from_slice(&frame).map_err(std::io::Error::other)
}

pub(crate) async fn write_frame_async(writer: &mut (impl AsyncWrite + Unpin), data: &[u8]) -> std::io::Result<()> {
    writer.write_u64_le(data.len() as u64).await?;
    writer.write_all(data).await
}

pub(crate) async fn write_message_async(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &impl Serialize,
) -> std::io::Result<()> {
    write_frame_async(writer, &serde_json::to_vec(message).map_err(std::io::Error::other)?).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_come_back_as_sent_and_end_cleanly() {
        let mut stream = Vec::new();
        write_frame(&mut stream, b"model").unwrap();
        write_message(&mut stream, &Reply::Failed("no".into())).unwrap();
        let mut reader = stream.as_slice();
        assert_eq!(read_frame(&mut reader, 100).unwrap().unwrap(), b"model");
        let reply: Reply = serde_json::from_slice(&read_frame(&mut reader, 100).unwrap().unwrap()).unwrap();
        assert!(matches!(reply, Reply::Failed(why) if why == "no"));
        assert!(read_frame(&mut reader, 100).unwrap().is_none(), "the end, between frames");
    }

    #[test]
    fn a_frame_over_the_limit_is_refused() {
        let mut stream = Vec::new();
        write_frame(&mut stream, &[0; 10]).unwrap();
        assert!(read_frame(&mut stream.as_slice(), 9).is_err());
        assert!(read_frame(&mut &stream[..12], 100).is_err(), "cut short");
    }
}
