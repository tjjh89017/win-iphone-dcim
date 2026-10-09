//! Local cache for files that the GUI opens.
//!
//! Layout: `<base>\<device key>\<device path>`, with the original folder
//! and file names. `<base>` is `cache_dir` from the config, else a
//! `win-iphone-dcim-cache` folder next to the GUI exe. If that folder is not writable, `<base>` is
//! `%LOCALAPPDATA%\win-iphone-dcim\cache` on Windows, and elsewhere
//! `$XDG_CACHE_HOME/win-iphone-dcim`, else `$HOME/.cache/win-iphone-dcim`.
//!
//! The size limit is soft: `make_room` deletes the least recently used
//! files before a download, but a download always proceeds.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::backup::paths::safe_file_name;
use crate::error::{Error, Result};
use crate::paths::to_verbatim;

const APP_DIR: &str = "win-iphone-dcim";
/// Folder name for a device that gives no device ID.
const UNKNOWN_DEVICE: &str = "unknown-device";

/// File created and deleted to test that a cache folder is writable.
const PROBE_FILE: &str = ".write-test";

/// The default cache folder name next to the exe.
pub const DEFAULT_DIR: &str = "win-iphone-dcim-cache";

/// The cache base folder: `configured`, else `DEFAULT_DIR` next to the exe. If
/// that folder cannot be created or written, fall back to `base_dir`.
pub fn choose_base(configured: Option<&Path>, exe_dir: Option<&Path>) -> Option<PathBuf> {
    let wanted = configured
        .map(Path::to_path_buf)
        .or_else(|| exe_dir.map(|d| d.join(DEFAULT_DIR)));
    choose_base_with(wanted, || base_dir().ok())
}

fn choose_base_with(
    wanted: Option<PathBuf>,
    fallback: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let Some(wanted) = wanted else {
        return fallback();
    };
    match writable(&wanted) {
        Ok(()) => Some(wanted),
        Err(e) => {
            let other = fallback();
            tracing::info!(
                "cache folder {} is not writable ({e}), using {}",
                wanted.display(),
                other
                    .as_ref()
                    .map_or_else(|| "no cache".into(), |p| p.display().to_string())
            );
            other
        }
    }
}

/// Create `dir` and write and delete a small file in it. A folder that the
/// probe created is removed again, so an unused cache leaves nothing.
fn writable(dir: &Path) -> std::io::Result<()> {
    let dir = to_verbatim(dir);
    let existed = dir.exists();
    std::fs::create_dir_all(&dir)?;
    let probe = dir.join(PROBE_FILE);
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)?;
    if !existed {
        remove_dir_quiet(&dir);
    }
    Ok(())
}

/// Remove `dir` if it is empty. A missing or non-empty folder stays as is.
fn remove_dir_quiet(dir: &Path) {
    if let Err(e) = std::fs::remove_dir(dir)
        && e.kind() != ErrorKind::NotFound
    {
        tracing::debug!("cache {}: {e}", dir.display());
    }
}

/// The fallback cache base folder from the environment.
pub fn base_dir() -> Result<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    base_from(
        cfg!(windows),
        var("LOCALAPPDATA"),
        var("XDG_CACHE_HOME"),
        var("HOME"),
    )
    .ok_or_else(|| Error::Io {
        context: "find the cache folder".into(),
        source: std::io::Error::new(
            ErrorKind::NotFound,
            if cfg!(windows) {
                "LOCALAPPDATA is not set"
            } else {
                "neither XDG_CACHE_HOME nor HOME is set"
            },
        ),
    })
}

fn base_from(
    windows: bool,
    local_app_data: Option<OsString>,
    xdg_cache_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if windows {
        return local_app_data.map(|d| PathBuf::from(d).join(APP_DIR).join("cache"));
    }
    xdg_cache_home
        .map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".cache")))
        .map(|d| d.join(APP_DIR))
}

/// The cache folder of one device. `device_key` is the hashed device ID
/// (`backup::manifest::device_key`), never the raw ID.
pub fn device_dir(base: &Path, device_key: Option<&str>) -> PathBuf {
    base.join(device_key.unwrap_or(UNKNOWN_DEVICE))
}

/// The cache path of the device file `device_path`. Each component keeps
/// its device name. A name that Windows cannot hold is an error.
pub fn file_path(device_dir: &Path, device_path: &str) -> Result<PathBuf> {
    let mut path = device_dir.to_path_buf();
    let mut any = false;
    for component in device_path.split('/').filter(|c| !c.is_empty()) {
        path.push(safe_file_name(component)?);
        any = true;
    }
    if !any {
        return Err(Error::NotAFolder(device_path.to_owned()));
    }
    Ok(path)
}

/// True if a complete cached copy is at `path`: a file of the device size.
/// Without a device size the cache cannot be trusted.
pub fn is_fresh(path: &Path, size: Option<u64>) -> bool {
    match (std::fs::metadata(path), size) {
        (Ok(m), Some(size)) => m.is_file() && m.len() == size,
        _ => false,
    }
}

/// Set the modification time of a cached file to now, so that `make_room`
/// keeps recently used files. Errors (a viewer holds the file) are only
/// logged.
pub fn touch(path: &Path) {
    let result = std::fs::OpenOptions::new()
        .write(true)
        .open(to_verbatim(path))
        .and_then(|f| f.set_modified(SystemTime::now()));
    if let Err(e) = result {
        tracing::debug!("cache touch {}: {e}", path.display());
    }
}

/// What `make_room` did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    /// Bytes in the cache after the deletions.
    pub used: u64,
    /// Bytes deleted.
    pub freed: u64,
    /// Files deleted.
    pub deleted: usize,
}

struct CachedFile {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

/// Delete the oldest files under `base` (by modification time) until
/// `used + needed <= limit` or no file is left to try. A file that cannot be
/// deleted (open in a viewer) is skipped and still counts as used. Folders
/// that become empty are removed. Never fails: the caller downloads anyway.
pub fn make_room(base: &Path, needed: u64, limit: u64) -> Room {
    make_room_with(base, needed, limit, |p| std::fs::remove_file(p))
}

fn make_room_with(
    base: &Path,
    needed: u64,
    limit: u64,
    mut remove: impl FnMut(&Path) -> std::io::Result<()>,
) -> Room {
    let base = to_verbatim(base);
    let mut files = Vec::new();
    collect_files(&base, &mut files);
    let mut room = Room {
        used: files.iter().map(|f| f.len).sum(),
        ..Room::default()
    };
    if room.used.saturating_add(needed) <= limit {
        return room;
    }
    files.sort_by(|a, b| a.modified.cmp(&b.modified).then(a.path.cmp(&b.path)));
    for file in files {
        if room.used.saturating_add(needed) <= limit {
            break;
        }
        match remove(&file.path) {
            Ok(()) => {
                room.used -= file.len;
                room.freed += file.len;
                room.deleted += 1;
                remove_empty_parents(&file.path, &base);
            }
            Err(e) => tracing::debug!("cache evict {}: {e}", file.path.display()),
        }
    }
    room
}

fn collect_files(dir: &Path, files: &mut Vec<CachedFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_files(&entry.path(), files);
        } else if kind.is_file()
            && let Ok(meta) = entry.metadata()
        {
            files.push(CachedFile {
                path: entry.path(),
                len: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
}

/// Remove the empty folders above `file`, up to but not including `base`.
fn remove_empty_parents(file: &Path, base: &Path) {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d == base || !d.starts_with(base) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// Delete the cache folder of one device, and the base folder above it when
/// that is empty then. A missing folder is not an error.
pub fn clear(device_dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(device_dir) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(Error::Io {
            context: format!("delete the cache folder {}", device_dir.display()),
            source: e,
        }),
        _ => {
            if let Some(base) = device_dir.parent() {
                remove_dir_quiet(base);
            }
            Ok(())
        }
    }
}

/// Delete everything under `base`, all devices, and `base` itself. A file
/// that cannot be deleted (open in a viewer) is logged at debug level and
/// left, and so are the folders above it. Return the number of entries left.
/// Never fails.
pub fn clear_all(base: &Path) -> usize {
    clear_all_with(base, &mut |p| std::fs::remove_file(p))
}

fn clear_all_with(base: &Path, remove: &mut dyn FnMut(&Path) -> std::io::Result<()>) -> usize {
    let left = clear_contents(base, remove);
    if left == 0 {
        remove_dir_quiet(base);
    }
    left
}

fn clear_contents(base: &Path, remove: &mut dyn FnMut(&Path) -> std::io::Result<()>) -> usize {
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() != ErrorKind::NotFound {
                tracing::debug!("cache {}: {e}", base.display());
            }
            return 0;
        }
    };
    let mut left = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir {
            left += clear_contents(&path, remove);
            if let Err(e) = std::fs::remove_dir(&path) {
                tracing::debug!("cache {}: {e}", path.display());
                left += 1;
            }
        } else if let Err(e) = remove(&path) {
            tracing::debug!("cache {}: {e}", path.display());
            left += 1;
        }
    }
    left
}

#[cfg(test)]
mod tests;
