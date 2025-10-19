//! Snapshot file I/O for state machine.
//!
//! This module provides a compact, forward‑compatible on‑disk format to persist and
//! restore FSM state including orders, etc. It is intended for periodic snapshots of the mahcine state,
//! not for an append‑only event log.
//!
//! File format (little‑endian):
//! - 8‑byte ASCII magic: `MACHNSNP`
//! - u16 global snapshot version (currently 1)
//! - Metadata (bincode-encoded):
//!   - u32 metadata length M
//!   - M bytes: `bincode`-serialized Metadata
//! - Repeating sequence of batches, each encoded as:
//!   - u32 payload length N
//!   - N bytes payload, encoded with [`order::wire`]
//!
//! Versioning and compatibility:
//! - Readers validate the magic and the global version and will refuse to open a file with
//!   an unsupported version. If a future incompatible change is introduced, bump the
//!   global version.
//! - The batch payload is delegated to the `order::wire` codec, which may carry its own
//!   versioning. This module treats the batch bytes as opaque and only length‑prefixes
//!   them.
//!
//! Batching semantics:
//! - Orders are written and read in batches. Each call to [`SnapshotWriter::write_order_batch`]
//!   produces a single batch in the file. [`SnapshotReader::next_order_batch`] returns exactly
//!   those batches. When no more data is available, it returns `Ok(None)` (EOF).
//!
//! Durability:
//! - Calling [`SnapshotWriter::finish`] flushes buffers and calls `sync_all()` on the file
//!   to ensure data is durably persisted on supported platforms.
//!
use crate::market::Symbol;
use crate::order::{Order, wire};
use crate::seq::Seq;
use serde::{Deserialize, Serialize};
use std::{io, path::Path};
use thiserror::Error;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};

#[derive(Error, Debug)]
/// Errors that can occur while reading or writing snapshot files.
pub enum Error {
    /// Wrapped `std::io::Error` arising from filesystem operations.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// The batch payload failed to encode/decode via the `order::wire` codec.
    #[error("Codec error: {0}")]
    Codec(#[from] wire::Error),

    /// The file does not start with the expected `MACHNSNP` magic bytes.
    #[error("Invalid file magic")]
    BadMagic,

    /// The snapshot's global version is not recognized by this reader implementation.
    #[error("Unsupported global snapshot version: {0}")]
    UnsupportedGlobalVersion(u16),

    /// A batch length was read but the expected number of bytes could not be read
    /// (likely due to an unexpected EOF), implying a truncated or corrupt file.
    #[error("Truncated or corrupt snapshot file")]
    Truncated,

    /// The encoded batch would exceed `u32::MAX` bytes and cannot be written.
    #[error("Snapshot file is too large")]
    TooLarge,

    /// Metadata bincode encode/decode failure.
    #[error("Metadata codec error: {0}")]
    MetaCodec(#[from] bincode::Error),
}

const MAGIC: &[u8; 8] = b"MACHNSNP"; // 8-byte magic at the very start
const SUPPORTED_GLOBAL_VERSION: u16 = 1;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Metadata persisted in the snapshot header section, immediately after magic and version.
pub struct Metadata {
    /// Highest applied sequence number at the time of snapshot.
    pub latest_seq: Seq,
    /// Highest archived sequence number at the time of snapshot (inclusive).
    pub latest_archived_seq: Seq,
}

async fn write_header(f: &mut File, global_version: u16) -> Result<(), Error> {
    f.write_all(MAGIC).await?;
    f.write_all(&global_version.to_le_bytes()).await?;
    Ok(())
}

async fn read_header<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<u16, Error> {
    let mut magic = [0u8; 8];
    let mut ver = [0u8; 2];
    r.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        return Err(Error::BadMagic);
    }
    r.read_exact(&mut ver).await?;
    let version = u16::from_le_bytes(ver);
    if version != SUPPORTED_GLOBAL_VERSION {
        return Err(Error::UnsupportedGlobalVersion(version));
    }
    Ok(version)
}

async fn write_metadata(w: &mut BufWriter<File>, md: &Metadata) -> Result<(), Error> {
    let bytes = bincode::serialize(md)?;
    write_len_prefixed(w, &bytes).await
}

async fn read_metadata(r: &mut BufReader<File>) -> Result<Metadata, Error> {
    let Some(buf) = read_len_prefixed(r).await? else {
        return Err(Error::Truncated);
    };
    let md: Metadata = bincode::deserialize(&buf)?;
    Ok(md)
}

async fn write_len_prefixed(w: &mut BufWriter<File>, bytes: &[u8]) -> Result<(), Error> {
    let len = u32::try_from(bytes.len()).map_err(|_| Error::TooLarge)?;
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(bytes).await?;
    Ok(())
}

async fn read_len_prefixed(r: &mut BufReader<File>) -> Result<Option<Vec<u8>>, Error> {
    let mut len_buf = [0u8; 4];

    // Try to read the length. Hitting EOF here means no more batches left.
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    }

    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::Truncated
        } else {
            Error::Io(e)
        }
    })?;
    Ok(Some(buf))
}

/// Builder to create a [`SnapshotWriter`] for writing a new snapshot file.
///
/// The builder only carries the target path. Use [`open`](SnapshotWriterBuilder::open)
/// to create and initialize the file (writing the header) and get a writer.
pub struct SnapshotWriterBuilder<'p> {
    path: &'p Path,
    metadata: Metadata,
}

impl<'p> SnapshotWriterBuilder<'p> {
    /// Create a new builder targeting the provided filesystem path.
    pub fn new(path: &'p Path) -> Self {
        Self {
            path,
            metadata: Metadata::default(),
        }
    }

    /// Set snapshot metadata that will be written after the header.
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Create the file, write the header and metadata, and return a ready [`SnapshotWriter`].
    ///
    /// This will truncate the file if it already exists.
    pub async fn open(self) -> Result<SnapshotWriter, Error> {
        let mut file = File::create(self.path).await?;
        write_header(&mut file, SUPPORTED_GLOBAL_VERSION).await?;
        // Use a temporary buffered writer to write length-prefixed metadata
        let mut temp_w = BufWriter::new(file);
        write_metadata(&mut temp_w, &self.metadata).await?;
        temp_w.flush().await?;
        let file = temp_w.into_inner();
        let writer = BufWriter::with_capacity(1 << 20, file); // 1 MiB buffer
        Ok(SnapshotWriter { writer })
    }
}

/// Asynchronously writes batches of orders into a snapshot file.
///
/// A writer is created via [`SnapshotWriterBuilder`]. Each call to
/// [`write_batch`](SnapshotWriter::write_order_batch) appends one encoded batch to the file.
/// Call [`finish`](SnapshotWriter::finish) to flush buffers and fsync the file.
pub struct SnapshotWriter {
    writer: BufWriter<File>,
}

impl SnapshotWriter {
    /// Encode and append a batch of orders to the snapshot file.
    ///
    /// Errors:
    /// - Returns [`Error::Codec`] if encoding the batch fails.
    /// - Returns [`Error::TooLarge`] if the encoded batch exceeds `u32::MAX` bytes.
    /// - Returns [`Error::Io`] if writing to the file fails.
    pub async fn write_order_batch(
        &mut self,
        symbol: Symbol,
        orders: &[Order],
    ) -> Result<(), Error> {
        let encoded = wire::encode_orders_batch(symbol, orders)?;
        write_len_prefixed(&mut self.writer, &encoded).await?;
        Ok(())
    }

    /// Flush internal buffers and call `sync_all()` on the underlying file to
    /// provide durability guarantees on supported platforms.
    ///
    /// After calling this method the writer is consumed and cannot be used again.
    pub async fn finish(mut self) -> Result<(), Error> {
        self.writer.flush().await?;
        let f = self.writer.into_inner();
        f.sync_all().await?;
        Ok(())
    }
}

/// Reads snapshot files and yields batches of orders.
///
/// Create a reader with [`SnapshotReader::open`], then call
/// [`next_batch`](SnapshotReader::next_order_batch) in a loop until it returns `Ok(None)`.
/// You can inspect the snapshot global version via [`global_version`](SnapshotReader::global_version).
pub struct SnapshotReader {
    reader: BufReader<File>,
    global_version: u16,
    metadata: Metadata,
}

impl SnapshotReader {
    /// Open an existing snapshot file and validate its header.
    ///
    /// Errors:
    /// - [`Error::BadMagic`] if the file does not start with the expected magic bytes.
    /// - [`Error::UnsupportedGlobalVersion`] if the version is not supported.
    /// - [`Error::Io`] for underlying filesystem errors.
    pub async fn open(path: &Path) -> Result<Self, Error> {
        let file = File::open(path).await?;
        let mut reader = BufReader::with_capacity(1 << 20, file);
        let version = read_header(&mut reader).await?;
        let metadata = read_metadata(&mut reader).await?;
        Ok(Self {
            reader,
            global_version: version,
            metadata,
        })
    }

    /// Return the validated global snapshot version read from the file header.
    pub fn global_version(&self) -> u16 {
        self.global_version
    }

    /// Return snapshot metadata read from the file.
    pub fn metadata(&self) -> Metadata {
        self.metadata
    }

    /// Read and decode the next batch of orders.
    ///
    /// Returns `Ok(None)` on EOF (no more batches). Returns an error if the
    /// batch cannot be read or decoded.
    pub async fn next_order_batch(&mut self) -> Result<Option<(Symbol, Vec<Order>)>, Error> {
        let Some(bytes) = read_len_prefixed(&mut self.reader).await? else {
            return Ok(None);
        };
        let orders = wire::decode_orders_batch(&bytes)?;
        Ok(Some(orders))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::order::{self};
    use crate::snapshot::Error as SnapErr;
    use crate::user;
    use std::path::PathBuf;
    use tokio::io::AsyncWriteExt;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("snapshot_test_{}.bin", Uuid::new_v4()));
        p
    }

    fn mk_order(id: u64) -> Order {
        let user_id: user::Id = "user-1".to_string();
        let client_id = format!("c{}", id);
        let side = order::Side::Bid;
        let price: order::Price = 1000 + id;
        let volume: order::Volume = 10 + id;
        Order::new(id, user_id, client_id, side, price, volume)
    }

    #[tokio::test]
    async fn write_read_roundtrip_multiple_batches() {
        let path = temp_path();

        // Write two batches
        let meta = Metadata {
            latest_seq: 123,
            latest_archived_seq: 120,
        };
        let mut w = SnapshotWriterBuilder::new(&path)
            .with_metadata(meta)
            .open()
            .await
            .expect("failed to open writer");
        let o1 = mk_order(1);
        let o2 = mk_order(2);
        w.write_order_batch("BTCUSDT".into(), &[o1.clone()])
            .await
            .expect("write_batch 1 failed");
        w.write_order_batch("BTCUSDT".into(), &[o2.clone()])
            .await
            .expect("write_batch 2 failed");
        w.finish().await.expect("finish writer failed");

        // Read back batches
        let mut r = SnapshotReader::open(&path)
            .await
            .expect("open reader failed");
        assert_eq!(
            r.global_version(),
            SUPPORTED_GLOBAL_VERSION,
            "unexpected global version in snapshot"
        );
        assert_eq!(r.metadata(), meta, "metadata mismatch");

        let (s1, b1) = r
            .next_order_batch()
            .await
            .expect("reading batch 1 errored")
            .expect("batch 1 missing");
        assert_eq!(b1.len(), 1, "batch 1 length mismatch: got {}", b1.len());
        assert_eq!(
            b1[0].id, 1,
            "batch 1 first order id mismatch: got {}",
            b1[0].id
        );
        assert_eq!(b1[0].client_id, o1.client_id, "batch 1 client_id mismatch");
        assert_eq!(s1, "BTCUSDT", "batch 1 symbol mismatch");

        let (s2, b2) = r
            .next_order_batch()
            .await
            .expect("reading batch 2 errored")
            .expect("batch 2 missing");
        assert_eq!(b2.len(), 1, "batch 2 length mismatch: got {}", b2.len());
        assert_eq!(
            b2[0].id, 2,
            "batch 2 first order id mismatch: got {}",
            b2[0].id
        );
        assert_eq!(
            b2[0].price, o2.price,
            "batch 2 price mismatch: got {}",
            b2[0].price
        );
        assert_eq!(s2, "BTCUSDT", "batch 2 symbol mismatch");

        let end = r.next_order_batch().await.expect("reading end errored");
        assert!(end.is_none(), "expected EOF None, got: {:?}", end);

        // cleanup
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn open_fails_on_bad_magic() {
        let path = temp_path();
        // Craft a file with wrong magic
        let mut f = File::create(&path).await.expect("create file failed");
        f.write_all(b"BADSIG!!").await.expect("write magic failed");
        f.write_all(&1u16.to_le_bytes())
            .await
            .expect("write version failed");
        f.flush().await.expect("flush failed");

        let err = match SnapshotReader::open(&path).await {
            Ok(_) => panic!("expected BadMagic error, but open succeeded"),
            Err(e) => e,
        };
        match err {
            SnapErr::BadMagic => {}
            other => panic!("expected BadMagic, got: {other:?}"),
        }
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn open_fails_on_unsupported_version() {
        let path = temp_path();
        let mut f = File::create(&path).await.expect("create file failed");
        f.write_all(MAGIC).await.expect("write magic failed");
        let bad_ver: u16 = SUPPORTED_GLOBAL_VERSION + 1;
        f.write_all(&bad_ver.to_le_bytes())
            .await
            .expect("write version failed");
        f.flush().await.expect("flush failed");

        let err = match SnapshotReader::open(&path).await {
            Ok(_) => panic!("expected UnsupportedGlobalVersion error, but open succeeded"),
            Err(e) => e,
        };
        match err {
            SnapErr::UnsupportedGlobalVersion(v) => assert_eq!(
                v, bad_ver,
                "unsupported version mismatch: got {} expected {}",
                v, bad_ver
            ),
            other => panic!("expected UnsupportedGlobalVersion, got: {other:?}"),
        }
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn next_batch_fails_on_truncated_payload() {
        let path = temp_path();
        // write header and a fake batch len, but no data
        let mut f = File::create(&path).await.expect("create file failed");
        // proper header
        f.write_all(MAGIC).await.expect("write magic failed");
        f.write_all(&SUPPORTED_GLOBAL_VERSION.to_le_bytes())
            .await
            .expect("write version failed");
        // write valid bincode-encoded metadata (default values)
        let md = Metadata::default();
        let md_bytes = bincode::serialize(&md).expect("serialize metadata");
        let md_len: u32 = md_bytes.len().try_into().expect("md too large");
        f.write_all(&md_len.to_le_bytes())
            .await
            .expect("write metadata length failed");
        f.write_all(&md_bytes)
            .await
            .expect("write metadata bytes failed");
        // one batch of length 10 but do not write the payload
        let len: u32 = 10;
        f.write_all(&len.to_le_bytes())
            .await
            .expect("write length failed");
        f.flush().await.expect("flush failed");

        let mut r = SnapshotReader::open(&path)
            .await
            .expect("open reader failed");
        let err = r
            .next_order_batch()
            .await
            .expect_err("expected Truncated error when reading batch");
        match err {
            SnapErr::Truncated => {}
            other => panic!("expected Truncated, got: {other:?}"),
        }
        let _ = tokio::fs::remove_file(&path).await;
    }
}
