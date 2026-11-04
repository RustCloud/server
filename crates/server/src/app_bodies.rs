//! Bodies of requests to apps.
//!
//! An app gets a request's body in memory, at most what it takes at once
//! (`limits.data`). An app with `large_bodies` in its manifest gets larger
//! bodies too, kept in a file on disk instead: the request reaches the app
//! without the body, with the header `x-rustcloud-body-size`, and the app saves
//! the body into a user's file with `files.write-request-body`, streamed by
//! the server (since API 0.2.14). An editor's server saving a big document
//! (WOPI `PutFile`) needs it. The kept file is removed once the app answered.

use std::path::Path;

use axum::body::{Body, Bytes};
use futures_util::StreamExt;
use rustcloud_core::{ByteStream, Error, ErrorKind, Result};
use tempfile::NamedTempFile;
use tokio::io::AsyncWriteExt;

/// The body of a request to an app.
pub enum RequestBody {
    /// Given to the app.
    Memory(Bytes),
    /// Too large for the app: kept on disk until the request ends.
    Kept { file: NamedTempFile, size: u64 },
}

/// Where and how much to keep of a body too large for the app.
pub struct Keep<'a> {
    pub dir: &'a Path,
    /// The upload limit of the server; `None`: no limit.
    pub max: Option<u64>,
}

impl RequestBody {
    /// Reads a body: in memory up to `in_memory` bytes; past that, into a
    /// file with `keep`, else `FILE_TOO_LARGE`.
    pub async fn receive(body: Body, in_memory: usize, keep: Option<Keep<'_>>) -> Result<Self> {
        let mut chunks = body.into_data_stream();
        let mut memory = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|err| Error::bad_request("BAD_BODY", err.to_string()))?;
            if memory.len() + chunk.len() <= in_memory {
                memory.extend_from_slice(&chunk);
                continue;
            }
            let Some(keep) = keep else {
                return Err(too_large());
            };
            // Too large for the app: what came so far and the rest go to disk.
            std::fs::create_dir_all(keep.dir).map_err(Error::internal)?;
            let file = NamedTempFile::new_in(keep.dir).map_err(Error::internal)?;
            let mut out = tokio::fs::File::from_std(file.reopen().map_err(Error::internal)?);
            let mut size = 0u64;
            for chunk in [Bytes::from(memory), chunk] {
                size = write_kept(&mut out, &chunk, size, keep.max).await?;
            }
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.map_err(|err| Error::bad_request("BAD_BODY", err.to_string()))?;
                size = write_kept(&mut out, &chunk, size, keep.max).await?;
            }
            out.flush().await.map_err(Error::internal)?;
            return Ok(Self::Kept { file, size });
        }
        Ok(Self::Memory(memory.into()))
    }

    /// What the app gets as the body: none if it is kept.
    pub fn for_app(&self) -> Vec<u8> {
        match self {
            Self::Memory(bytes) => bytes.to_vec(),
            Self::Kept { .. } => Vec::new(),
        }
    }

    /// The size of a kept body, for the app's header.
    pub fn kept_size(&self) -> Option<u64> {
        match self {
            Self::Memory(_) => None,
            Self::Kept { size, .. } => Some(*size),
        }
    }

    /// The body from its start, as often as needed.
    pub async fn stream(&self) -> Result<ByteStream> {
        match self {
            Self::Memory(bytes) => {
                let bytes = bytes.clone();
                Ok(Box::pin(futures_util::stream::once(async move { Ok(bytes) })))
            }
            Self::Kept { file, .. } => {
                let file = tokio::fs::File::open(file.path()).await.map_err(Error::internal)?;
                Ok(Box::pin(tokio_util::io::ReaderStream::new(file)))
            }
        }
    }
}

async fn write_kept(out: &mut tokio::fs::File, chunk: &[u8], size: u64, max: Option<u64>) -> Result<u64> {
    let size = size + chunk.len() as u64;
    if max.is_some_and(|max| size > max) {
        return Err(too_large());
    }
    out.write_all(chunk).await.map_err(Error::internal)?;
    Ok(size)
}

fn too_large() -> Error {
    Error::new(ErrorKind::PayloadTooLarge, "FILE_TOO_LARGE", "Request body too large")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn all(body: &RequestBody) -> Vec<u8> {
        let chunks: Vec<Bytes> = body.stream().await.unwrap().map(|chunk| chunk.unwrap()).collect().await;
        chunks.concat()
    }

    fn body_of(parts: &[&'static str]) -> Body {
        let chunks = parts.iter().map(|part| Ok::<_, std::io::Error>(Bytes::from_static(part.as_bytes())));
        Body::from_stream(futures_util::stream::iter(chunks.collect::<Vec<_>>()))
    }

    #[tokio::test]
    async fn small_bodies_stay_in_memory() {
        let body = RequestBody::receive(body_of(&["ab", "cd"]), 4, None).await.unwrap();
        assert_eq!(body.for_app(), b"abcd");
        assert_eq!(body.kept_size(), None);
        assert_eq!(all(&body).await, b"abcd");
    }

    #[tokio::test]
    async fn larger_ones_are_kept_on_disk_or_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = RequestBody::receive(body_of(&["ab", "cde"]), 4, None).await.err().unwrap();
        assert_eq!(err.code, "FILE_TOO_LARGE");

        let keep = Keep { dir: dir.path(), max: Some(5) };
        let body = RequestBody::receive(body_of(&["ab", "cde"]), 4, Some(keep)).await.unwrap();
        assert!(body.for_app().is_empty());
        assert_eq!(body.kept_size(), Some(5));
        assert_eq!(all(&body).await, b"abcde");
        assert_eq!(all(&body).await, b"abcde", "read again");
        drop(body);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "removed when done");

        let keep = Keep { dir: dir.path(), max: Some(4) };
        let err = RequestBody::receive(body_of(&["ab", "cde"]), 2, Some(keep)).await.err().unwrap();
        assert_eq!(err.code, "FILE_TOO_LARGE", "past the upload limit");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
