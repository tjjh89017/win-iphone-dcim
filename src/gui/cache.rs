//! Local cache for files that the GUI opens (SPEC.md section 10, Phase 4).
//!
//! Layout: `<base>\<device key>\<device path>`, with the original folder
//! and file names. `<base>` is `cache_dir` from the config, else a `cache`
//! folder next to the GUI exe. If that folder is not writable, `<base>` is
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

/// The cache base folder: `configured`, else `cache` next to the exe. If
/// that folder cannot be created or written, fall back to `base_dir`.
pub fn choose_base(configured: Option<&Path>, exe_dir: Option<&Path>) -> Option<PathBuf> {
    let wanted = configured
        .map(Path::to_path_buf)
        .or_else(|| exe_dir.map(|d| d.join("cache")));
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

/// Create `dir` and write and delete a small file in it.
fn writable(dir: &Path) -> std::io::Result<()> {
    let dir = to_verbatim(dir);
    std::fs::create_dir_all(&dir)?;
    let probe = dir.join(PROBE_FILE);
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
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

/// Delete the cache folder of one device. A missing folder is not an error.
pub fn clear(device_dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(device_dir) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(Error::Io {
            context: format!("delete the cache folder {}", device_dir.display()),
            source: e,
        }),
        _ => Ok(()),
    }
}

/// Delete everything under `base`, all devices. A file that cannot be
/// deleted (open in a viewer) is logged at debug level and left. Return the
/// number of entries left. Never fails.
pub fn clear_all(base: &Path) -> usize {
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
            left += clear_all(&path);
            if let Err(e) = std::fs::remove_dir(&path) {
                tracing::debug!("cache {}: {e}", path.display());
                left += 1;
            }
        } else if let Err(e) = std::fs::remove_file(&path) {
            tracing::debug!("cache {}: {e}", path.display());
            left += 1;
        }
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_follows_the_platform_variables() {
        let s = |v: &str| Some(OsString::from(v));
        assert_eq!(
            base_from(true, s("C:/Users/u/AppData/Local"), s("/x"), s("/h")),
            Some(PathBuf::from(
                "C:/Users/u/AppData/Local/win-iphone-dcim/cache"
            ))
        );
        assert_eq!(base_from(true, None, s("/x"), s("/h")), None);
        assert_eq!(
            base_from(false, s("C:/L"), s("/xdg"), s("/home/u")),
            Some(PathBuf::from("/xdg/win-iphone-dcim"))
        );
        assert_eq!(
            base_from(false, None, None, s("/home/u")),
            Some(PathBuf::from("/home/u/.cache/win-iphone-dcim"))
        );
        assert_eq!(base_from(false, None, None, None), None);
    }

    #[test]
    fn file_path_keeps_device_names_under_the_device_key() {
        let dir = device_dir(Path::new("/c"), Some("0123456789abcdef"));
        assert_eq!(dir, Path::new("/c/0123456789abcdef"));
        assert_eq!(
            device_dir(Path::new("/c"), None),
            Path::new("/c/unknown-device")
        );
        let p = file_path(&dir, "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC").unwrap();
        assert_eq!(
            p,
            Path::new("/c/0123456789abcdef/Internal Storage/DCIM/202601_a/IMG_0001.HEIC")
        );
        let p = file_path(&dir, "/DCIM/写真 📷.HEIC").unwrap();
        assert_eq!(p.file_name().unwrap(), "写真 📷.HEIC");
    }

    #[test]
    fn unsafe_names_and_the_root_are_errors() {
        let dir = Path::new("/c/k");
        assert!(matches!(
            file_path(dir, "/DCIM/a:b.HEIC"),
            Err(Error::UnsafeFileName { .. })
        ));
        assert!(matches!(
            file_path(dir, "/DCIM/../x"),
            Err(Error::UnsafeFileName { .. })
        ));
        assert!(file_path(dir, "/").is_err());
    }

    #[test]
    fn fresh_needs_the_same_size_and_clear_removes_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = device_dir(tmp.path(), Some("k"));
        let f = file_path(&dir, "/DCIM/IMG.HEIC").unwrap();
        assert!(!is_fresh(&f, Some(3)));
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, b"abc").unwrap();
        assert!(is_fresh(&f, Some(3)));
        assert!(!is_fresh(&f, Some(4)));
        assert!(!is_fresh(&f, None));
        clear(&dir).unwrap();
        assert!(!dir.exists());
        clear(&dir).unwrap();
    }

    #[test]
    fn base_on_windows_is_the_cache_folder_of_the_app() {
        let base = base_from(true, Some("C:\\L".into()), None, None).unwrap();
        assert_eq!(
            base,
            PathBuf::from("C:\\L").join("win-iphone-dcim").join("cache")
        );
    }

    /// Write `len` bytes at `base/rel` with a modification time of `secs`
    /// after the epoch.
    fn put(base: &Path, rel: &str, len: usize, secs: u64) -> PathBuf {
        let path = base.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![0u8; len]).unwrap();
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(time)
            .unwrap();
        path
    }

    #[test]
    fn make_room_deletes_the_oldest_files_first() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let old = put(base, "k/DCIM/x/old.HEIC", 100, 1_000);
        let mid = put(base, "k/DCIM/y/mid.HEIC", 100, 2_000);
        let new = put(base, "k/DCIM/y/new.HEIC", 100, 3_000);
        let room = make_room(base, 100, 250);
        assert_eq!(
            room,
            Room {
                used: 100,
                freed: 200,
                deleted: 2
            }
        );
        assert!(!old.exists() && !mid.exists() && new.exists());
        // The emptied folder is gone; the base and used folders stay.
        assert!(!base.join("k/DCIM/x").exists());
        assert!(base.join("k/DCIM/y").exists());
        assert!(base.exists());
    }

    #[test]
    fn make_room_under_the_limit_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let f = put(tmp.path(), "k/a", 100, 1_000);
        assert_eq!(make_room(tmp.path(), 100, 200).deleted, 0);
        assert!(f.exists());
        let missing = tmp.path().join("missing");
        assert_eq!(make_room(&missing, 1, 0), Room::default());
    }

    #[test]
    fn soft_limit_lets_a_file_larger_than_the_limit_through() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "k/a", 100, 1_000);
        put(tmp.path(), "k/b", 100, 2_000);
        let room = make_room(tmp.path(), 1_000, 250);
        assert_eq!(room.deleted, 2);
        assert_eq!(room.used, 0);
        // The caller still downloads; only the base folder is left.
        assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
    }

    #[test]
    fn make_room_skips_files_it_cannot_delete_and_counts_them() {
        let tmp = tempfile::tempdir().unwrap();
        let held = put(tmp.path(), "k/held", 100, 1_000);
        let next = put(tmp.path(), "k/next", 100, 2_000);
        let last = put(tmp.path(), "k/last", 100, 3_000);
        let held_v = to_verbatim(&held);
        let room = make_room_with(tmp.path(), 100, 250, |p| {
            if p == held_v {
                Err(std::io::Error::other("in use"))
            } else {
                std::fs::remove_file(p)
            }
        });
        assert!(held.exists() && !next.exists() && !last.exists());
        assert_eq!(
            room,
            Room {
                used: 100,
                freed: 200,
                deleted: 2
            }
        );
    }

    #[test]
    fn touch_makes_a_file_the_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let a = put(tmp.path(), "k/a", 100, 1_000);
        let b = put(tmp.path(), "k/b", 100, 2_000);
        touch(&a);
        make_room(tmp.path(), 0, 150);
        assert!(a.exists() && !b.exists());
    }

    #[test]
    fn choose_base_prefers_the_exe_folder_and_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("app");
        std::fs::create_dir(&exe).unwrap();
        assert_eq!(choose_base(None, Some(&exe)), Some(exe.join("cache")));
        assert!(exe.join("cache").is_dir());
        assert!(!exe.join("cache").join(PROBE_FILE).exists());
        let configured = tmp.path().join("mine");
        assert_eq!(
            choose_base(Some(&configured), Some(&exe)),
            Some(configured.clone())
        );
        // A file in the way makes the folder impossible to create.
        let blocker = tmp.path().join("file");
        std::fs::write(&blocker, b"x").unwrap();
        let fallback = tmp.path().join("fallback");
        assert_eq!(
            choose_base_with(Some(blocker.join("cache")), || Some(fallback.clone())),
            Some(fallback.clone())
        );
        assert_eq!(choose_base_with(None, || None), None);
    }

    #[test]
    fn clear_all_removes_every_device_and_ignores_a_missing_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("cache");
        assert_eq!(clear_all(&base), 0);
        for key in ["a", "b"] {
            let f = file_path(&device_dir(&base, Some(key)), "/DCIM/x/IMG.HEIC").unwrap();
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, b"abc").unwrap();
        }
        assert_eq!(clear_all(&base), 0);
        assert!(std::fs::read_dir(&base).unwrap().next().is_none());
    }
}
