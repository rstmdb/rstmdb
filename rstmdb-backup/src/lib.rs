//! Physical backup/restore archive format (`.rstmbak`) for rstmdb.
//!
//! A backup is a self-describing container:
//!
//! ```text
//! [8-byte magic "RSTMBAK\x01"][1-byte compression][ (gzip?) tar stream ]
//! ```
//!
//! The tar stream holds `manifest.json` (first), then the raw `snapshots/` and
//! `wal/` files that make up a rstmdb `data_dir`. Restore extracts those files
//! back into a fresh `data_dir`; the server's normal WAL replay reconstructs all
//! state on the next start — no re-apply logic required.
//!
//! Integrity is per-file (`sha256` of each file's bytes, recorded in the
//! manifest and re-verified on read) plus a manifest-level digest.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Container magic: "RSTMBAK" + format byte.
pub const MAGIC: [u8; 8] = *b"RSTMBAK\x01";

/// Current manifest format version.
pub const FORMAT_VERSION: u32 = 1;

/// Compression applied to the tar stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
}

impl Compression {
    fn byte(self) -> u8 {
        match self {
            Compression::None => 0,
            Compression::Gzip => 1,
        }
    }
    fn from_byte(b: u8) -> Result<Self, BackupError> {
        match b {
            0 => Ok(Compression::None),
            1 => Ok(Compression::Gzip),
            other => Err(BackupError::UnknownCompression(other)),
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Compression::None => "none",
            Compression::Gzip => "gzip",
        }
    }
}

/// Errors produced while writing or reading a backup archive.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not a rstmdb backup archive (bad magic)")]
    BadMagic,
    #[error("unknown compression byte: {0}")]
    UnknownCompression(u8),
    #[error("archive is missing its manifest (first entry must be manifest.json)")]
    MissingManifest,
    #[error("unsupported archive format version {0} (this build supports {FORMAT_VERSION})")]
    UnsupportedVersion(u32),
    #[error("unsafe path in archive: {0}")]
    UnsafePath(String),
    #[error("checksum mismatch for {path}: manifest={expected} actual={actual}")]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("destination {0} is not empty (use force to overwrite)")]
    DestinationNotEmpty(PathBuf),
    #[error("wal directory not found under data dir: {0}")]
    NoWalDir(PathBuf),
}

/// A single file captured in the archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to the data dir, always `wal/...` or `snapshots/...`.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Metadata the caller supplies (it knows the WAL head and counts); the archive
/// layer fills in the file list, checksums, and timestamps.
#[derive(Debug, Clone, Default)]
pub struct ManifestMeta {
    pub rstmdb_version: String,
    pub wal_head_offset: u64,
    pub wal_head_sequence: u64,
    pub machine_count: Option<u64>,
    pub instance_count: Option<u64>,
    /// Free-form provenance (role, host, ...).
    pub source: Option<serde_json::Value>,
}

/// The archive manifest (first tar entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub kind: String,
    pub rstmdb_version: String,
    pub created_at: String,
    pub compression: String,
    pub wal_head_offset: u64,
    pub wal_head_sequence: u64,
    pub segment_count: usize,
    pub snapshot_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_count: Option<u64>,
    /// sha256 over the concatenation of each file's sha256 (manifest digest).
    pub payload_sha256: String,
    pub files: Vec<FileEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<serde_json::Value>,
}

fn sha256_file(path: &Path) -> Result<(u64, String), BackupError> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}

/// Collect `wal/*` and `snapshots/*` files under `data_dir`, sorted, with sizes
/// and checksums.
fn collect_files(data_dir: &Path) -> Result<Vec<(PathBuf, FileEntry)>, BackupError> {
    let wal_dir = data_dir.join("wal");
    if !wal_dir.is_dir() {
        return Err(BackupError::NoWalDir(wal_dir));
    }

    let mut out: Vec<(PathBuf, FileEntry)> = Vec::new();
    for sub in ["snapshots", "wal"] {
        let dir = data_dir.join(sub);
        if !dir.is_dir() {
            continue;
        }
        let mut names: Vec<String> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            let abs = dir.join(&name);
            let (size, sha256) = sha256_file(&abs)?;
            let rel = format!("{}/{}", sub, name);
            out.push((abs, FileEntry { path: rel, size, sha256 }));
        }
    }
    Ok(out)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Writes a physical backup of `data_dir` (its `wal/` and `snapshots/` subdirs)
/// to `out`. Returns the manifest that was written.
pub fn write_backup<W: Write>(
    data_dir: &Path,
    meta: ManifestMeta,
    compression: Compression,
    mut out: W,
) -> Result<Manifest, BackupError> {
    let files = collect_files(data_dir)?;

    let segment_count = files.iter().filter(|(_, f)| f.path.starts_with("wal/")).count();
    let snapshot_count = files
        .iter()
        .filter(|(_, f)| f.path.starts_with("snapshots/"))
        .count();

    // Manifest-level digest over each file's checksum.
    let mut digest = Sha256::new();
    for (_, f) in &files {
        digest.update(f.sha256.as_bytes());
    }
    let payload_sha256 = hex::encode(digest.finalize());

    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        kind: "physical".to_string(),
        rstmdb_version: meta.rstmdb_version,
        created_at: now_rfc3339(),
        compression: compression.as_str().to_string(),
        wal_head_offset: meta.wal_head_offset,
        wal_head_sequence: meta.wal_head_sequence,
        segment_count,
        snapshot_count,
        machine_count: meta.machine_count,
        instance_count: meta.instance_count,
        payload_sha256,
        files: files.iter().map(|(_, f)| f.clone()).collect(),
        source: meta.source,
    };

    // Container header first (raw), then the (optionally compressed) tar stream.
    out.write_all(&MAGIC)?;
    out.write_all(&[compression.byte()])?;

    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    match compression {
        Compression::Gzip => {
            let enc = flate2::write::GzEncoder::new(out, flate2::Compression::default());
            let enc = build_tar(enc, &manifest_bytes, &files)?;
            enc.finish()?; // flush gzip trailer
        }
        Compression::None => {
            build_tar(out, &manifest_bytes, &files)?;
        }
    }

    Ok(manifest)
}

fn build_tar<W: Write>(
    w: W,
    manifest_bytes: &[u8],
    files: &[(PathBuf, FileEntry)],
) -> Result<W, BackupError> {
    let mut tar = tar::Builder::new(w);

    // manifest.json first
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    tar.append_data(&mut header, "manifest.json", manifest_bytes)?;

    for (abs, entry) in files {
        let mut f = File::open(abs)?;
        tar.append_file(&entry.path, &mut f)?;
    }

    Ok(tar.into_inner()?)
}

/// Reject archive paths that escape the data dir or aren't under wal/snapshots.
fn safe_relpath(raw: &str) -> Result<PathBuf, BackupError> {
    let p = Path::new(raw);
    let is_bad = p.components().any(|c| {
        matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_))
    });
    if is_bad || !(raw.starts_with("wal/") || raw.starts_with("snapshots/")) {
        return Err(BackupError::UnsafePath(raw.to_string()));
    }
    Ok(p.to_path_buf())
}

/// Reads the container header and returns (compression, reader positioned at the
/// tar stream).
fn open_container<R: Read + 'static>(
    mut src: R,
) -> Result<(Compression, Box<dyn Read>), BackupError> {
    let mut magic = [0u8; 8];
    src.read_exact(&mut magic)?;
    if magic != MAGIC {
        return Err(BackupError::BadMagic);
    }
    let mut cbyte = [0u8; 1];
    src.read_exact(&mut cbyte)?;
    let compression = Compression::from_byte(cbyte[0])?;
    let reader: Box<dyn Read> = match compression {
        Compression::Gzip => Box::new(flate2::read::GzDecoder::new(src)),
        Compression::None => Box::new(src),
    };
    Ok((compression, reader))
}

/// Reads and validates a backup, invoking `sink` with each file's relative path
/// and its verified bytes. Returns the manifest. `sink` receiving is what
/// extract/verify differ on.
fn read_archive<R, F>(src: R, mut sink: F) -> Result<Manifest, BackupError>
where
    R: Read + 'static,
    F: FnMut(&str, &[u8]) -> Result<(), BackupError>,
{
    let (_c, reader) = open_container(src)?;
    let mut archive = tar::Archive::new(reader);

    let mut manifest: Option<Manifest> = None;
    let mut expected: std::collections::HashMap<String, FileEntry> = std::collections::HashMap::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();

        if manifest.is_none() {
            if path != "manifest.json" {
                return Err(BackupError::MissingManifest);
            }
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            let m: Manifest = serde_json::from_slice(&buf)?;
            if m.format_version > FORMAT_VERSION {
                return Err(BackupError::UnsupportedVersion(m.format_version));
            }
            for f in &m.files {
                expected.insert(f.path.clone(), f.clone());
            }
            manifest = Some(m);
            continue;
        }

        // A data file — validate path, read + hash, verify against manifest.
        let rel = safe_relpath(&path)?;
        let rel_str = rel.to_string_lossy().into_owned();

        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        let actual = hex::encode(Sha256::digest(&buf));
        if let Some(e) = expected.get(&rel_str) {
            if e.sha256 != actual {
                return Err(BackupError::ChecksumMismatch {
                    path: rel_str,
                    expected: e.sha256.clone(),
                    actual,
                });
            }
        }
        sink(&rel_str, &buf)?;
    }

    manifest.ok_or(BackupError::MissingManifest)
}

/// Reads a backup and verifies it (manifest + per-file checksums) WITHOUT
/// writing anything to disk. Returns the manifest.
pub fn verify_backup<R: Read + 'static>(src: R) -> Result<Manifest, BackupError> {
    read_archive(src, |_path, _bytes| Ok(()))
}

/// Restores a backup into `data_dir`, recreating `wal/` and `snapshots/`.
///
/// If `data_dir/wal` or `data_dir/snapshots` already contain files, this errors
/// unless `force` is set (in which case those directories are cleared first).
pub fn read_backup<R: Read + 'static>(
    src: R,
    data_dir: &Path,
    force: bool,
) -> Result<Manifest, BackupError> {
    for sub in ["wal", "snapshots"] {
        let dir = data_dir.join(sub);
        let non_empty = dir
            .read_dir()
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
        if non_empty {
            if !force {
                return Err(BackupError::DestinationNotEmpty(dir));
            }
            fs::remove_dir_all(&dir)?;
        }
    }
    fs::create_dir_all(data_dir.join("wal"))?;
    fs::create_dir_all(data_dir.join("snapshots"))?;

    let data_dir = data_dir.to_path_buf();
    read_archive(src, move |rel, bytes| {
        let dest = data_dir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = File::create(&dest)?;
        f.write_all(bytes)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_data_dir() -> tempfile::TempDir {
        let td = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(td.path().join("wal")).unwrap();
        fs::create_dir_all(td.path().join("snapshots")).unwrap();
        fs::write(td.path().join("wal/0000000000000001.wal"), b"wal-segment-bytes").unwrap();
        fs::write(td.path().join("snapshots/i-1.snap"), b"snapshot-1").unwrap();
        fs::write(td.path().join("snapshots/i-2.snap"), b"snapshot-2").unwrap();
        td
    }

    fn roundtrip(compression: Compression) {
        let src = make_data_dir();
        let meta = ManifestMeta {
            rstmdb_version: "0.3.0".into(),
            wal_head_offset: 42,
            wal_head_sequence: 7,
            instance_count: Some(2),
            ..Default::default()
        };
        let mut buf = Vec::new();
        let m = write_backup(src.path(), meta, compression, &mut buf).unwrap();
        assert_eq!(m.segment_count, 1);
        assert_eq!(m.snapshot_count, 2);
        assert_eq!(m.wal_head_sequence, 7);

        // verify (no extraction)
        let vm = verify_backup(std::io::Cursor::new(buf.clone())).unwrap();
        assert_eq!(vm.payload_sha256, m.payload_sha256);

        // restore into a fresh dir
        let dst = tempfile::TempDir::new().unwrap();
        let rm = read_backup(std::io::Cursor::new(buf), dst.path(), false).unwrap();
        assert_eq!(rm.instance_count, Some(2));
        assert_eq!(
            fs::read(dst.path().join("wal/0000000000000001.wal")).unwrap(),
            b"wal-segment-bytes"
        );
        assert_eq!(fs::read(dst.path().join("snapshots/i-1.snap")).unwrap(), b"snapshot-1");
    }

    #[test]
    fn roundtrip_uncompressed() {
        roundtrip(Compression::None);
    }

    #[test]
    fn roundtrip_gzip() {
        roundtrip(Compression::Gzip);
    }

    #[test]
    fn restore_into_nonempty_requires_force() {
        let src = make_data_dir();
        let mut buf = Vec::new();
        write_backup(src.path(), ManifestMeta::default(), Compression::Gzip, &mut buf).unwrap();

        let dst = make_data_dir(); // already populated
        let err = read_backup(std::io::Cursor::new(buf.clone()), dst.path(), false);
        assert!(matches!(err, Err(BackupError::DestinationNotEmpty(_))));

        // force succeeds and replaces content
        read_backup(std::io::Cursor::new(buf), dst.path(), true).unwrap();
    }

    #[test]
    fn detects_corruption() {
        let src = make_data_dir();
        let mut buf = Vec::new();
        write_backup(src.path(), ManifestMeta::default(), Compression::None, &mut buf).unwrap();
        // Corrupt a byte inside a file's content (uncompressed, so the content
        // bytes appear verbatim in the archive). This must trip the per-file
        // checksum on read.
        let needle = b"wal-segment-bytes";
        let pos = buf
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("file content present in uncompressed archive");
        buf[pos + 1] ^= 0xff;
        let dst = tempfile::TempDir::new().unwrap();
        let res = read_backup(std::io::Cursor::new(buf), dst.path(), false);
        assert!(
            matches!(res, Err(BackupError::ChecksumMismatch { .. })),
            "corruption should be a checksum mismatch, got {res:?}"
        );
    }

    #[test]
    fn rejects_bad_magic() {
        let bad = vec![0u8; 32];
        assert!(matches!(
            verify_backup(std::io::Cursor::new(bad)),
            Err(BackupError::BadMagic)
        ));
    }
}
