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
