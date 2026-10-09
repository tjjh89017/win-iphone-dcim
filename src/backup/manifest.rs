//! JSONL manifest of committed files.
//!
//! The manifest is `DEST/.win-iphone-dcim/manifest.jsonl`. Each committed
//! file appends one JSON line. Each append is flushed and synced, so an
//! unexpected shutdown loses at most the last, incomplete line. On load a
//! line that does not parse is logged and skipped, and later records for the
//! same path win.
//!
//! The manifest never holds the raw WPD device ID. It holds a BLAKE3 based
//! device key (see `device_key`).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{LocalTime, Verification};

/// Folder under DEST that holds the tool's own files.
pub const DIR_NAME: &str = ".win-iphone-dcim";
/// Manifest file name in `DIR_NAME`.
pub const FILE_NAME: &str = "manifest.jsonl";
/// Current record schema version.
pub const SCHEMA: u32 = 1;
/// Hash algorithm name in the records.
pub const HASH_ALG: &str = "blake3";

/// Hex characters of the device key in the manifest.
const DEVICE_KEY_HEX: usize = 16;
/// Hex characters of the device key in logs.
pub const DEVICE_LOG_HEX: usize = 8;

/// One committed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Schema version.
    pub v: u32,
    /// Device key from `device_key`. `None` if the backend gives no device ID.
    #[serde(default)]
    pub device: Option<String>,
    /// Path under DEST with `/` separators.
    pub path: String,
    /// Device path of the source, for example `/Internal Storage/DCIM/a/IMG_0001.HEIC`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Bytes written.
    pub size: u64,
    /// Device modified time, device local time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// Device created time, device local time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    pub verification: Verification,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_alg: Option<String>,
    /// Lowercase hex of the local file hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// UTC commit time, RFC 3339.
    pub committed_at: String,
}

impl Record {
    /// A record for a file that was just committed.
    #[allow(clippy::too_many_arguments)]
    pub fn committed(
        device: Option<String>,
        path: String,
        source: String,
        size: u64,
        modified: Option<LocalTime>,
        created: Option<LocalTime>,
        verification: Verification,
        hash: Option<[u8; 32]>,
    ) -> Self {
        Self {
            v: SCHEMA,
            device,
            path,
            source: Some(source),
            size,
            modified: modified.map(|t| t.to_string()),
            created: created.map(|t| t.to_string()),
            verification,
            hash_alg: hash.map(|_| HASH_ALG.to_owned()),
            hash: hash.map(|h| to_hex(&h)),
            committed_at: utc_now(),
        }
    }

    /// The stored BLAKE3 hash, if any.
    pub fn blake3(&self) -> Option<&str> {
        match self.hash_alg.as_deref() {
            Some(HASH_ALG) => self.hash.as_deref(),
            _ => None,
        }
    }
}

/// One indexed record and its state after `reconcile`.
#[derive(Debug, Clone)]
pub struct Entry {
    pub record: Record,
    /// Why the record no longer matches the local file. `None` if it matches.
    pub stale: Option<String>,
}

/// Counts from `reconcile`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub ok: usize,
    pub stale: usize,
}

pub struct Manifest {
    root: PathBuf,
    index: BTreeMap<String, Entry>,
    file: Option<File>,
    /// The file does not end with a newline (a truncated last line). The
    /// next append starts a new line first.
    needs_newline: bool,
}

impl Manifest {
    /// The manifest path for a copy root.
    pub fn path_for(root: &Path) -> PathBuf {
        root.join(DIR_NAME).join(FILE_NAME)
    }

    /// Load the manifest of `root`. A missing manifest gives an empty index.
    /// A line that does not parse is logged and skipped.
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path_for(root);
        let mut data = Vec::new();
        match File::open(&path) {
            Ok(mut f) => {
                f.read_to_end(&mut data).map_err(|source| Error::Io {
                    context: format!("read {}", path.display()),
                    source,
                })?;
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Io {
                    context: format!("open {}", path.display()),
                    source,
                });
            }
        }
        let mut index = BTreeMap::new();
        let needs_newline = !data.is_empty() && !data.ends_with(b"\n");
        let lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
        let last = lines.len().saturating_sub(1);
        for (i, line) in lines.iter().enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<Record>(line) {
                Ok(r) if r.v > SCHEMA => tracing::warn!(
                    "{}: line {}: schema version {} is newer than {SCHEMA}; record skipped",
                    path.display(),
                    i + 1,
                    r.v
                ),
                Ok(r) => {
                    index.insert(
                        r.path.clone(),
                        Entry {
                            record: r,
                            stale: None,
                        },
                    );
                }
                Err(e) if i == last && needs_newline => tracing::warn!(
                    "{}: incomplete last line skipped (an earlier run stopped while it wrote it): {e}",
                    path.display()
                ),
                Err(e) => tracing::warn!(
                    "{}: line {}: invalid record skipped: {e}",
                    path.display(),
                    i + 1
                ),
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            index,
            file: None,
            needs_newline,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn get(&self, rel: &str) -> Option<&Entry> {
        self.index.get(rel)
    }

    /// Records sorted by path.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.index.values()
    }

    /// Compare each record with the local file. A missing file or a
    /// different size marks the record stale.
    pub fn reconcile(&mut self) -> Reconciled {
        let mut counts = Reconciled::default();
        for entry in self.index.values_mut() {
            let local = local_path(&self.root, &entry.record.path);
            entry.stale = match std::fs::metadata(&local) {
                Ok(m) if m.is_file() && m.len() == entry.record.size => None,
                Ok(m) if m.is_file() => Some(format!(
                    "local size {} differs from manifest size {}",
                    m.len(),
                    entry.record.size
                )),
                Ok(_) => Some("local path is not a file".into()),
                Err(e) if e.kind() == ErrorKind::NotFound => Some("local file is missing".into()),
                Err(e) => Some(format!("cannot inspect the local file: {e}")),
            };
            match entry.stale {
                Some(_) => counts.stale += 1,
                None => counts.ok += 1,
            }
        }
        counts
    }

    /// Append one record, then flush and sync it. Create the folder and the
    /// file on the first write. The record replaces the indexed one.
    pub fn append(&mut self, record: Record) -> Result<()> {
        let path = Self::path_for(&self.root);
        let io_err = |source| Error::Io {
            context: format!("write {}", path.display()),
            source,
        };
        if self.file.is_none() {
            self.file = Some(self.open_append().map_err(io_err)?);
        }
        let mut line = Vec::new();
        if self.needs_newline {
            line.push(b'\n');
        }
        serde_json::to_writer(&mut line, &record).map_err(|e| io_err(io::Error::other(e)))?;
        line.push(b'\n');
        let file = self.file.as_mut().expect("opened above");
        file.write_all(&line)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_data())
            .map_err(io_err)?;
        self.needs_newline = false;
        self.index.insert(
            record.path.clone(),
            Entry {
                record,
                stale: None,
            },
        );
        Ok(())
    }

    fn open_append(&self) -> io::Result<File> {
        std::fs::create_dir_all(self.root.join(DIR_NAME))?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(Self::path_for(&self.root))
    }
}

/// The relative manifest path of `target` under `root`, with `/`
/// separators. `None` if `target` is not below `root` or a component is not
/// valid Unicode.
pub fn relative_path(root: &Path, target: &Path) -> Option<String> {
    let rel = target.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The local path of a manifest path. Components that could leave `root`
/// are dropped, so a damaged manifest cannot point outside DEST.
pub fn local_path(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(['\\', ':']) {
            continue;
        }
        p.push(part);
    }
    p
}

/// Stable device key for the manifest: the first 16 hex characters of the
/// BLAKE3 hash of the raw device ID. The raw ID never goes to the manifest.
pub fn device_key(raw_id: &str) -> String {
    let mut hex = blake3::hash(raw_id.as_bytes()).to_hex().to_string();
    hex.truncate(DEVICE_KEY_HEX);
    hex
}

/// The short device key for logs.
pub fn device_log_key(key: &str) -> &str {
    &key[..key.len().min(DEVICE_LOG_HEX)]
}

/// BLAKE3 of a local file, read as a stream.
pub fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(File::open(path)?)?;
    Ok(*hasher.finalize().as_bytes())
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (y, mo, d, h, mi, s) = LocalTime(secs).civil();
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod tests;
