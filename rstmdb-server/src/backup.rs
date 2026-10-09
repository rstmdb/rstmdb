//! Server-side hot-backup cursor registry.
//!
//! `BackupBegin` stages a `.rstmbak` archive to a temp file under
//! `<data_dir>/.backup-staging/` and registers a cursor. The client then pulls
//! fixed-size chunks (`BackupChunk`) from the immutable staged file until EOF,
//! and `BackupEnd` releases it. Cursors are reaped on end, session close, TTL,
//! and server startup so an abandoned client can never leak temp files.

use rstmdb_backup::{write_backup, Compression, Manifest, ManifestMeta};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;

/// Raw bytes served per `BackupChunk` (base64 ≈ 5.6 MiB, under the 16 MiB cap).
pub const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

/// Subdirectory (under the data dir) that holds in-progress staged archives.
pub const STAGING_SUBDIR: &str = ".backup-staging";

/// Errors surfaced by backup registry operations.
#[derive(Debug)]
pub enum BackupOpError {
    /// No cursor with that id.
    NotFound,
    /// Cursor exists but belongs to another session.
    NotOwner,
    /// Session already has an active backup.
    AlreadyActive,
    /// Requested chunk range is outside the archive.
    BadRange,
    /// Archive construction failed.
    Backup(rstmdb_backup::BackupError),
    /// Temp file I/O failed.
    Io(std::io::Error),
}

impl std::fmt::Display for BackupOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupOpError::NotFound => write!(f, "no such backup id"),
            BackupOpError::NotOwner => write!(f, "backup id belongs to another session"),
            BackupOpError::AlreadyActive => {
                write!(f, "session already has an active backup")
            }
            BackupOpError::BadRange => write!(f, "requested chunk range is out of bounds"),
            BackupOpError::Backup(e) => write!(f, "backup staging failed: {e}"),
            BackupOpError::Io(e) => write!(f, "backup io error: {e}"),
        }
    }
}

impl std::error::Error for BackupOpError {}

impl From<rstmdb_backup::BackupError> for BackupOpError {
    fn from(e: rstmdb_backup::BackupError) -> Self {
        BackupOpError::Backup(e)
    }
}
impl From<std::io::Error> for BackupOpError {
    fn from(e: std::io::Error) -> Self {
        BackupOpError::Io(e)
    }
}

/// Reads the current WAL head (offset, sequence) for the manifest, read-only.
pub fn backup_head(data_dir: &Path) -> (u64, u64) {
    use rstmdb_wal::{FsyncPolicy, Wal, WalConfig};
    let wal_dir = data_dir.join("wal");
    match Wal::open(WalConfig::new(&wal_dir).with_fsync_policy(FsyncPolicy::Never)) {
        Ok(wal) => {
            let seq = wal.next_sequence().saturating_sub(1);
            // `stats().bytes_written` is a per-instance write counter (zero on
            // a freshly reopened WAL); `total_size()` reflects the bytes
            // actually recovered from segments on disk, which is what a
            // read-only head check needs.
            (wal.total_size(), seq)
        }
        Err(_) => (0, 0),
    }
}

/// The result of `begin`.
pub struct BeginOutcome {
    pub backup_id: String,
    pub manifest: Manifest,
    pub total_bytes: u64,
    pub chunk_size: u64,
}

struct Cursor {
    session_id: String,
    temp_path: PathBuf,
    total_bytes: u64,
    last_access: Instant,
}

/// Tracks in-progress hot backups. Cheap to clone-share behind an `Arc`.
#[derive(Default)]
pub struct BackupRegistry {
    cursors: Mutex<HashMap<String, Cursor>>,
    seq: std::sync::atomic::AtomicU64,
}

impl BackupRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stages an archive of `data_dir` to a temp file and registers a cursor.
    ///
    /// If `gate` is provided, staging holds it so auto-compaction cannot delete
    /// a WAL segment mid-copy.
    ///
    /// Note: this is not atomically exclusive under concurrent same-session
    /// calls — the one-per-session check and the cursor insert are separate
    /// lock acquisitions. Handler-level serialization is expected to prevent
    /// that in practice.
    pub fn begin(
        &self,
        session_id: &str,
        data_dir: &Path,
        meta: ManifestMeta,
        compression: Compression,
        gate: Option<&Arc<Mutex<()>>>,
    ) -> Result<BeginOutcome, BackupOpError> {
        // One active backup per session.
        {
            let cursors = self.cursors.lock();
            if cursors.values().any(|c| c.session_id == session_id) {
                return Err(BackupOpError::AlreadyActive);
            }
        }

        let staging_dir = data_dir.join(STAGING_SUBDIR);
        std::fs::create_dir_all(&staging_dir)?;

        let n = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let backup_id = format!("b-{session_id}-{n}");
        let temp_path = staging_dir.join(format!("{backup_id}.rstmbak"));

        // Stage the archive, holding the compaction gate across the copy. Any
        // failure here must clean up the temp file, since no cursor
        // references it and nothing else will ever reap it.
        let stage_result = {
            let _held = gate.map(|g| g.lock());
            (|| {
                let file = File::create(&temp_path)?;
                let writer = std::io::BufWriter::new(file);
                Ok::<_, BackupOpError>(write_backup(data_dir, meta, compression, writer)?)
            })()
        };
        let manifest = match stage_result {
            Ok(manifest) => manifest,
            Err(e) => {
                let _ = std::fs::remove_file(&temp_path);
                return Err(e);
            }
        };

        let total_bytes = std::fs::metadata(&temp_path)?.len();

        self.cursors.lock().insert(
            backup_id.clone(),
            Cursor {
                session_id: session_id.to_string(),
                temp_path,
                total_bytes,
                last_access: Instant::now(),
            },
        );

        Ok(BeginOutcome {
            backup_id,
            manifest,
            total_bytes,
            chunk_size: CHUNK_SIZE,
        })
    }

    /// Reads up to `len` bytes at `offset` from a staged archive.
    /// Returns `(bytes, eof)`. Re-reading the same offset is allowed (idempotent).
    pub fn read_chunk(
        &self,
        backup_id: &str,
        session_id: &str,
        offset: u64,
        len: u64,
    ) -> Result<(Vec<u8>, bool), BackupOpError> {
        let mut cursors = self.cursors.lock();
        let cursor = cursors.get_mut(backup_id).ok_or(BackupOpError::NotFound)?;
        if cursor.session_id != session_id {
            return Err(BackupOpError::NotOwner);
        }
        if offset > cursor.total_bytes {
            return Err(BackupOpError::BadRange);
        }
        let want = len.min(CHUNK_SIZE).min(cursor.total_bytes - offset);
        let mut file = File::open(&cursor.temp_path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; want as usize];
        file.read_exact(&mut buf)?;
        cursor.last_access = Instant::now();
        let eof = offset + want >= cursor.total_bytes;
        Ok((buf, eof))
    }

    /// Releases a cursor (deletes its temp file). Errors if not owner / absent.
    pub fn end(&self, backup_id: &str, session_id: &str) -> Result<(), BackupOpError> {
        let mut cursors = self.cursors.lock();
        match cursors.get(backup_id) {
            None => return Err(BackupOpError::NotFound),
            Some(c) if c.session_id != session_id => return Err(BackupOpError::NotOwner),
            Some(_) => {}
        }
        if let Some(c) = cursors.remove(backup_id) {
            let _ = std::fs::remove_file(&c.temp_path);
        }
        Ok(())
    }

    /// Drops (and deletes temp files for) all cursors owned by a session.
    pub fn release_session(&self, session_id: &str) {
        let mut cursors = self.cursors.lock();
        let ids: Vec<String> = cursors
            .iter()
            .filter(|(_, c)| c.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(c) = cursors.remove(&id) {
                let _ = std::fs::remove_file(&c.temp_path);
            }
        }
    }

    /// Drops cursors idle longer than `ttl`.
    pub fn sweep_expired(&self, ttl: std::time::Duration) {
        let mut cursors = self.cursors.lock();
        let ids: Vec<String> = cursors
            .iter()
            .filter(|(_, c)| c.last_access.elapsed() >= ttl)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(c) = cursors.remove(&id) {
                let _ = std::fs::remove_file(&c.temp_path);
            }
        }
    }

    /// Test/introspection helper: the temp path backing a cursor, if any.
    pub fn temp_path_for(&self, backup_id: &str) -> Option<PathBuf> {
        self.cursors
            .lock()
            .get(backup_id)
            .map(|c| c.temp_path.clone())
    }

    /// Removes any orphaned staging directory left by a previous run.
    /// Call once at server startup, before serving.
    pub fn clean_staging_dir(data_dir: &Path) {
        let staging = data_dir.join(STAGING_SUBDIR);
        if staging.exists() {
            let _ = std::fs::remove_dir_all(&staging);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstmdb_backup::{Compression, ManifestMeta};
    use rstmdb_core::StateMachineEngine;
    use rstmdb_wal::{FsyncPolicy, WalConfig};
    use serde_json::json;

    fn seed_data_dir() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        let engine = StateMachineEngine::new(
            WalConfig::new(tmp.path().join("wal")).with_fsync_policy(FsyncPolicy::EveryWrite),
        )
        .unwrap();
        engine
            .put_machine(
                "order",
                1,
                &json!({
                    "states": ["created", "paid"],
                    "initial": "created",
                    "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
                }),
            )
            .unwrap();
        engine
            .create_instance("o-1", "order", 1, json!({"n": 1}), None)
            .unwrap();
        engine.wal().sync().unwrap();
        tmp
    }

    fn meta() -> ManifestMeta {
        ManifestMeta {
            rstmdb_version: "test".into(),
            ..Default::default()
        }
    }

    #[test]
    fn begin_read_end_roundtrip() {
        let dir = seed_data_dir();
        let reg = BackupRegistry::new();
        let out = reg
            .begin("s1", dir.path(), meta(), Compression::Gzip, None)
            .unwrap();
        assert!(out.total_bytes > 0);
        assert_eq!(out.chunk_size, CHUNK_SIZE);

        // Pull the whole archive.
        let mut buf = Vec::new();
        let mut offset = 0u64;
        loop {
            let (bytes, eof) = reg
                .read_chunk(&out.backup_id, "s1", offset, CHUNK_SIZE)
                .unwrap();
            offset += bytes.len() as u64;
            buf.extend_from_slice(&bytes);
            if eof {
                break;
            }
        }
        assert_eq!(offset, out.total_bytes);

        // The assembled bytes verify as a real archive.
        rstmdb_backup::verify_backup(std::io::Cursor::new(buf)).unwrap();

        reg.end(&out.backup_id, "s1").unwrap();
        // Cursor is gone.
        assert!(matches!(
            reg.read_chunk(&out.backup_id, "s1", 0, CHUNK_SIZE),
            Err(BackupOpError::NotFound)
        ));
    }

    #[test]
    fn one_active_backup_per_session() {
        let dir = seed_data_dir();
        let reg = BackupRegistry::new();
        let _out = reg
            .begin("s1", dir.path(), meta(), Compression::None, None)
            .unwrap();
        assert!(matches!(
            reg.begin("s1", dir.path(), meta(), Compression::None, None),
            Err(BackupOpError::AlreadyActive)
        ));
    }

    #[test]
    fn cross_session_access_denied() {
        let dir = seed_data_dir();
        let reg = BackupRegistry::new();
        let out = reg
            .begin("s1", dir.path(), meta(), Compression::None, None)
            .unwrap();
        assert!(matches!(
            reg.read_chunk(&out.backup_id, "s2", 0, CHUNK_SIZE),
            Err(BackupOpError::NotOwner)
        ));
        assert!(matches!(
            reg.end(&out.backup_id, "s2"),
            Err(BackupOpError::NotOwner)
        ));
    }

    #[test]
    fn release_session_deletes_temp_file() {
        let dir = seed_data_dir();
        let reg = BackupRegistry::new();
        let out = reg
            .begin("s1", dir.path(), meta(), Compression::None, None)
            .unwrap();
        let path = reg.temp_path_for(&out.backup_id).unwrap();
        assert!(path.exists());
        reg.release_session("s1");
        assert!(!path.exists());
        assert!(matches!(
            reg.read_chunk(&out.backup_id, "s1", 0, CHUNK_SIZE),
            Err(BackupOpError::NotFound)
        ));
    }

    #[test]
    fn sweep_expired_reaps_idle_cursor() {
        let dir = seed_data_dir();
        let reg = BackupRegistry::new();
        let out = reg
            .begin("s1", dir.path(), meta(), Compression::None, None)
            .unwrap();
        let path = reg.temp_path_for(&out.backup_id).unwrap();
        // TTL of zero => everything is already expired.
        reg.sweep_expired(std::time::Duration::from_secs(0));
        assert!(!path.exists());
        assert!(matches!(
            reg.read_chunk(&out.backup_id, "s1", 0, CHUNK_SIZE),
            Err(BackupOpError::NotFound)
        ));
    }

    #[test]
    fn backup_head_reports_wal_progress() {
        let dir = seed_data_dir();
        let (offset, seq) = backup_head(dir.path());
        assert!(seq >= 1, "expected sequence >= 1, got {seq}");
        assert!(offset > 0, "expected non-zero offset, got {offset}");
    }

    #[test]
    fn backup_head_on_empty_dir_is_zero() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(backup_head(tmp.path()), (0, 0));
    }

    #[test]
    fn bad_range_is_rejected() {
        let dir = seed_data_dir();
        let reg = BackupRegistry::new();
        let out = reg
            .begin("s1", dir.path(), meta(), Compression::None, None)
            .unwrap();
        // offset past EOF
        assert!(matches!(
            reg.read_chunk(&out.backup_id, "s1", out.total_bytes + 1, CHUNK_SIZE),
            Err(BackupOpError::BadRange)
        ));
    }
}
