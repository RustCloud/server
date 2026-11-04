//! Blob storage: the strategy for *where file bytes live*.
//!
//! File metadata (names, folders, owners) is kept in the database. The bytes
//! themselves are stored as opaque blobs addressed by a key. The key is chosen
//! by the caller (RustCloud uses a fresh UUID for every stored version), so a
//! store never has to understand paths or users.
//!
//! Implementations: `rustcloud-storage-local` (a directory on disk). An S3 or
//! other backend only needs to implement this trait.

use std::hash::{BuildHasher, Hasher};
use std::io;
use std::pin::Pin;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{stream, Stream, StreamExt};
use serde::Serialize;

use crate::Result;

/// A stream of file bytes, used for both uploads and downloads.
pub type ByteStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send>>;

/// An inclusive byte range `start..=end` inside a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    /// Number of bytes in the range (never zero: `end >= start`).
    pub fn byte_count(&self) -> u64 {
        self.end - self.start + 1
    }
}

/// Where file contents are stored.
///
/// Keys are short ASCII strings without slashes (RustCloud passes UUIDs).
/// Implementations must reject anything else, so a key can never escape the
/// storage area.
#[async_trait]
pub trait BlobStore: Send + Sync + 'static {
    /// Stores the stream under `key`, replacing nothing: the key must be new.
    ///
    /// Returns the number of bytes written. If the stream fails midway, the
    /// partial blob is removed and the stream's error is returned.
    async fn put(&self, key: &str, data: ByteStream) -> Result<u64>;

    /// Reads the whole blob, or just `range` of it. The range is already
    /// validated against the blob size by the caller.
    async fn get(&self, key: &str, range: Option<ByteRange>) -> Result<ByteStream>;

    /// Removes the blob. Removing a missing blob is not an error.
    async fn delete(&self, key: &str) -> Result<()>;

    /// Copies blob `from` to the new key `to`.
    async fn copy(&self, from: &str, to: &str) -> Result<()>;

    /// The blob as a file opened for reading, where the storage keeps blobs as
    /// files: tools (ffmpeg) read files, not streams. `None` elsewhere; see
    /// [`open_as_file`].
    async fn open_file(&self, _key: &str) -> Result<Option<std::fs::File>> {
        Ok(None)
    }

    /// What the administration page shows about this storage.
    fn describe(&self) -> StorageInfo;

    /// Total and free space, for storages of a fixed size (a disk). `None` for
    /// storages without one, such as S3.
    async fn capacity(&self) -> Result<Option<Capacity>> {
        Ok(None)
    }

    /// Asks the system to forget cached contents of a blob, so the next read
    /// comes from the storage itself. Used by [`speed_test`](Self::speed_test).
    async fn forget_cached(&self, _key: &str) -> Result<()> {
        Ok(())
    }

    /// Writes `bytes` of random data as a temporary blob, reads it back and
    /// removes it, timing the write and the read.
    async fn speed_test(&self, bytes: u64) -> Result<SpeedTest> {
        let key = format!("speedtest-{}", random_u64());
        let started = Instant::now();
        self.put(&key, random_stream(bytes)).await?;
        let write = started.elapsed();

        let result = async {
            self.forget_cached(&key).await?;
            let started = Instant::now();
            let mut stream = self.get(&key, None).await?;
            while let Some(chunk) = stream.next().await {
                chunk?;
            }
            Ok::<_, crate::Error>(started.elapsed())
        }
        .await;
        self.delete(&key).await?;
        Ok(SpeedTest { bytes, write, read: result? })
    }
}

/// Kind and location of a blob store, e.g. `local` and `/data/files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageInfo {
    pub kind: &'static str,
    pub location: String,
}

/// Size of a storage, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capacity {
    pub total_bytes: u64,
    /// Space available for new data.
    pub free_bytes: u64,
}

/// Timings of [`BlobStore::speed_test`].
#[derive(Debug, Clone, Copy)]
pub struct SpeedTest {
    pub bytes: u64,
    pub write: Duration,
    pub read: Duration,
}

/// Size of the chunks the speed test writes.
const TEST_CHUNK: u64 = 1024 * 1024;

/// `bytes` of data that doesn't compress (so compressing file systems don't
/// make the disk look faster than it is): one random chunk, repeated.
fn random_stream(bytes: u64) -> ByteStream {
    let mut state = random_u64() | 1;
    let chunk: Bytes = (0..TEST_CHUNK)
        .map(|_| {
            // xorshift64: plenty random for this purpose.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect::<Vec<u8>>()
        .into();
    let sizes = (0..bytes.div_ceil(TEST_CHUNK)).map(move |i| (bytes - i * TEST_CHUNK).min(TEST_CHUNK));
    Box::pin(stream::iter(sizes.map(move |size| Ok(chunk.slice(..usize::try_from(size).unwrap_or(0))))))
}

fn random_u64() -> u64 {
    // A new hasher is seeded randomly by the standard library.
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(0);
    hasher.finish()
}

/// A blob as a file opened for reading: the storage's own file, or a copy in a
/// temporary file that disappears when closed.
pub async fn open_as_file(store: &dyn BlobStore, key: &str) -> Result<std::fs::File> {
    if let Some(file) = store.open_file(key).await? {
        return Ok(file);
    }
    let copy = tokio::task::spawn_blocking(tempfile::tempfile).await.map_err(crate::Error::internal)??;
    let mut writer = tokio::fs::File::from_std(copy);
    let mut stream = store.get(key, None).await?;
    while let Some(chunk) = stream.next().await {
        tokio::io::AsyncWriteExt::write_all(&mut writer, &chunk?).await?;
    }
    let mut copy = writer.into_std().await;
    std::io::Seek::rewind(&mut copy)?;
    Ok(copy)
}
