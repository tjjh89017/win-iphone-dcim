//! Local cache for files that the GUI opens (SPEC.md section 10, Phase 4).
//!
//! Layout: `<base>\<device key>\<device path>`, with the original folder
//! and file names. On Windows `<base>` is
//! `%LOCALAPPDATA%\win-iphone-dcim\cache`. Elsewhere it is
//! `$XDG_CACHE_HOME/win-iphone-dcim`, else `$HOME/.cache/win-iphone-dcim`.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::backup::paths::safe_file_name;
use crate::error::{Error, Result};

const APP_DIR: &str = "win-iphone-dcim";
/// Folder name for a device that gives no device ID.
const UNKNOWN_DEVICE: &str = "unknown-device";

/// The cache base folder from the environment.
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
}
