use super::*;
use crate::device_fs::fake::{self, dcim};
use crate::gui::selection::Tree;

struct FakeConnector;

impl Connector for FakeConnector {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        Ok(fake::devices())
    }

    fn open(&self, _index: usize) -> Result<Box<dyn DeviceFs>> {
        Ok(Box::new(dcim()))
    }
}

fn handle(cache: &Path) -> DeviceHandle {
    handle_with_limit(cache, u64::MAX)
}

fn handle_with_limit(cache: &Path, cache_max: u64) -> DeviceHandle {
    DeviceHandle::spawn(
        Box::new(FakeConnector),
        Some(cache.to_path_buf()),
        cache_max,
        Box::new(|| {}),
    )
}

fn next(h: &DeviceHandle) -> Reply {
    h.rx.recv_timeout(Duration::from_secs(10)).unwrap()
}

/// Replies until the first one that `done` accepts. Return all of them.
fn until(h: &DeviceHandle, done: impl Fn(&Reply) -> bool) -> Vec<Reply> {
    let mut all = Vec::new();
    loop {
        let r = next(h);
        let stop = done(&r);
        all.push(r);
        if stop {
            return all;
        }
    }
}

fn open(h: &DeviceHandle) -> Tree {
    h.send(Request::ListDevices);
    assert!(matches!(next(h), Reply::Devices(Ok(d)) if d.len() == 1));
    h.send(Request::Open { index: 0 });
    let Reply::Opened(Ok(opened)) = next(h) else {
        panic!("open failed");
    };
    assert_eq!(opened.root.path, "/");
    assert!(opened.cache_dir.is_some());
    Tree::new(opened.root)
}

fn load(h: &DeviceHandle, tree: &mut Tree, path: &str) {
    h.send(Request::List { path: path.into() });
    match next(h) {
        Reply::Listed {
            path: p,
            result: Ok(entries),
        } => {
            assert_eq!(p, path);
            tree.set_children(path, entries);
        }
        _ => panic!("list failed"),
    }
}

#[test]
fn browse_and_copy_the_checked_set() {
    let cache = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let h = handle(cache.path());
    let mut tree = open(&h);
    for p in [
        "/",
        "/Internal Storage",
        "/Internal Storage/DCIM",
        "/Internal Storage/DCIM/202601_a",
    ] {
        load(&h, &mut tree, p);
    }
    let names: Vec<&str> = tree
        .children("/Internal Storage/DCIM/202601_a")
        .unwrap()
        .iter()
        .map(|p| tree.entry(p).unwrap().name.as_str())
        .collect();
    assert_eq!(names, ["IMG_0001.HEIC", "IMG_0002.MOV"]);
    // 202601_b is not loaded; checking it means all of it.
    tree.set_checked("/Internal Storage/DCIM/202601_b", true);
    tree.set_checked("/Internal Storage/DCIM/202601_a/IMG_0002.MOV", true);
    h.send(Request::Copy {
        what: CopySet::Checked(tree.selection()),
        dest: dest.path().to_path_buf(),
        force: false,
        totals: None,
    });
    let replies = until(&h, |r| matches!(r, Reply::CopyDone(_)));
    let Some(Reply::CopyDone(Ok(summary))) = replies.last() else {
        panic!("copy failed");
    };
    assert_eq!((summary.copied, summary.failed), (2, 0));
    let d = dest.path().join("DCIM");
    assert_eq!(
        std::fs::read(d.join("202601_b/IMG_0001.HEIC")).unwrap(),
        b"heic-b"
    );
    assert_eq!(
        d.join("202601_a/IMG_0002.MOV").metadata().unwrap().len(),
        2048
    );
    assert!(!d.join("202601_a/IMG_0001.HEIC").exists());
    assert!(
        dest.path()
            .join(".win-iphone-dcim/manifest.jsonl")
            .is_file()
    );
    let last_progress = replies.iter().rev().find_map(|r| match r {
        Reply::CopyProgress(p) => Some(p.clone()),
        _ => None,
    });
    let p = last_progress.unwrap();
    assert_eq!((p.files_found, p.files_done), (2, 2));
    assert_eq!(p.bytes_done, 2048 + 6);
    // The totals come from the pre-scan, before the first file starts.
    let first = replies
        .iter()
        .find_map(|r| match r {
            Reply::CopyProgress(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!((first.files_found, first.bytes_found), (2, 2048 + 6));

    // A second copy skips the verified files.
    h.send(Request::Copy {
        what: CopySet::Checked(tree.selection()),
        dest: dest.path().to_path_buf(),
        force: false,
        totals: None,
    });
    let replies = until(&h, |r| matches!(r, Reply::CopyDone(_)));
    let Some(Reply::CopyDone(Ok(summary))) = replies.last() else {
        panic!("copy failed");
    };
    assert_eq!((summary.copied, summary.skipped), (0, 2));
    assert!(
        replies
            .iter()
            .any(|r| matches!(r, Reply::CopyNote(Note::Skip, t) if t.ends_with("verified")))
    );
}

#[test]
fn copy_to_copies_the_given_paths() {
    let cache = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let h = handle(cache.path());
    open(&h);
    h.send(Request::Copy {
        what: CopySet::Paths(vec![
            "/Internal Storage/DCIM/202601_a".into(),
            "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC".into(),
        ]),
        dest: dest.path().to_path_buf(),
        force: false,
        totals: None,
    });
    let replies = until(&h, |r| matches!(r, Reply::CopyDone(_)));
    let Some(Reply::CopyDone(Ok(summary))) = replies.last() else {
        panic!("copy failed");
    };
    assert_eq!((summary.copied, summary.failed), (3, 0));
    assert!(dest.path().join("202601_a/IMG_0002.MOV").is_file());
    assert_eq!(
        std::fs::read(dest.path().join("IMG_0001.HEIC")).unwrap(),
        b"heic-b"
    );
}

fn copy_paths(h: &DeviceHandle, dest: &Path, totals: Option<(u64, u64)>) -> Vec<Reply> {
    h.send(Request::Copy {
        what: CopySet::Paths(vec![
            "/Internal Storage/DCIM/202601_a".into(),
            "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC".into(),
        ]),
        dest: dest.to_path_buf(),
        force: false,
        totals,
    });
    let replies = until(h, |r| matches!(r, Reply::CopyDone(_)));
    let Some(Reply::CopyDone(Ok(summary))) = replies.last() else {
        panic!("copy failed");
    };
    assert_eq!((summary.copied, summary.failed), (3, 0));
    replies
}

fn progresses(replies: &[Reply]) -> Vec<CopyProgress> {
    replies
        .iter()
        .filter_map(|r| match r {
            Reply::CopyProgress(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn copy_without_totals_reports_the_scan_start_and_end() {
    let cache = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let h = handle(cache.path());
    open(&h);
    let replies = copy_paths(&h, dest.path(), None);
    let scans: Vec<(u64, u64)> = replies
        .iter()
        .filter_map(|r| match r {
            Reply::CopyScan { files, bytes } => Some((*files, *bytes)),
            _ => None,
        })
        .collect();
    assert_eq!(scans.first(), Some(&(0, 0)));
    assert_eq!(scans.last(), Some(&(3, 2060)));
    // All scan replies come before the first progress reply.
    let last_scan = replies
        .iter()
        .rposition(|r| matches!(r, Reply::CopyScan { .. }))
        .unwrap();
    let first_progress = replies
        .iter()
        .position(|r| matches!(r, Reply::CopyProgress(_)))
        .unwrap();
    assert!(last_scan < first_progress);
    let p = progresses(&replies);
    assert_eq!((p[0].files_found, p[0].bytes_found), (3, 2060));
}

#[test]
fn copy_with_totals_skips_the_scan_and_lets_totals_grow() {
    let cache = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let h = handle(cache.path());
    open(&h);
    // The totals are too small, like a tree that is out of date.
    let replies = copy_paths(&h, dest.path(), Some((2, 100)));
    assert!(!replies.iter().any(|r| matches!(r, Reply::CopyScan { .. })));
    let p = progresses(&replies);
    assert_eq!((p[0].files_found, p[0].bytes_found), (2, 100));
    let last = p.last().unwrap();
    assert_eq!((last.files_found, last.bytes_found), (3, 2060));
    assert_eq!((last.files_done, last.bytes_done), (3, 2060));
}

#[test]
fn cancel_before_copy_copies_nothing() {
    let cache = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let h = handle(cache.path());
    let mut tree = open(&h);
    load(&h, &mut tree, "/");
    tree.set_checked("/Internal Storage", true);
    h.cancel.store(true, Ordering::SeqCst);
    h.send(Request::Copy {
        what: CopySet::Checked(tree.selection()),
        dest: dest.path().to_path_buf(),
        force: false,
        totals: None,
    });
    let replies = until(&h, |r| matches!(r, Reply::CopyDone(_)));
    let Some(Reply::CopyDone(Ok(summary))) = replies.last() else {
        panic!("copy failed");
    };
    assert!(summary.cancelled);
    assert_eq!(summary.copied, 0);
}

#[test]
fn download_goes_to_the_cache_and_is_reused() {
    let cache = tempfile::tempdir().unwrap();
    let h = handle(cache.path());
    let mut tree = open(&h);
    load(&h, &mut tree, "/");
    let path = "/Internal Storage/DCIM/202601_a/IMG_0002.MOV";
    h.send(Request::Download { path: path.into() });
    let Reply::Downloaded {
        local,
        reused,
        path: p,
    } = until(&h, |r| !matches!(r, Reply::DownloadProgress { .. }))
        .pop()
        .unwrap()
    else {
        panic!("download failed");
    };
    assert_eq!(p, path);
    assert!(!reused);
    // The fake gives no device ID.
    assert_eq!(
        local,
        cache
            .path()
            .join("unknown-device/Internal Storage/DCIM/202601_a/IMG_0002.MOV")
    );
    assert_eq!(local.metadata().unwrap().len(), 2048);
    h.send(Request::Download { path: path.into() });
    assert!(matches!(next(&h), Reply::Downloaded { reused: true, .. }));
    h.send(Request::ClearCache);
    assert!(matches!(next(&h), Reply::CacheCleared(Ok(_))));
    assert!(!local.exists());
    h.send(Request::Download {
        path: "/Internal Storage/DCIM".into(),
    });
    assert!(matches!(next(&h), Reply::DownloadFailed { .. }));
}

fn download(h: &DeviceHandle, path: &str) -> PathBuf {
    h.send(Request::Download { path: path.into() });
    match until(h, |r| !matches!(r, Reply::DownloadProgress { .. })).pop() {
        Some(Reply::Downloaded { local, .. }) => local,
        _ => panic!("download of {path} failed"),
    }
}

#[test]
fn download_evicts_older_files_over_the_limit() {
    let cache = tempfile::tempdir().unwrap();
    let h = handle_with_limit(cache.path(), 2050);
    let mut tree = open(&h);
    load(&h, &mut tree, "/");
    let mov = download(&h, "/Internal Storage/DCIM/202601_a/IMG_0002.MOV");
    assert!(mov.exists());
    let heic = download(&h, "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC");
    assert!(!mov.exists());
    assert!(heic.exists());
    let other = download(&h, "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC");
    assert!(heic.exists() && other.exists());
}

#[test]
fn enumerate_and_stream_for_explorer() {
    let cache = tempfile::tempdir().unwrap();
    let h = handle(cache.path());
    open(&h);
    let slot = Arc::new(Pending::default());
    h.send(Request::Enumerate {
        paths: vec![
            "/Internal Storage/DCIM/202601_a".into(),
            "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC".into(),
        ],
        slot: slot.clone(),
    });
    let listing = slot.wait(Duration::from_secs(10)).unwrap().unwrap();
    let names: Vec<(String, bool)> = (0..listing.len())
        .map(|i| match listing.file(i as i32) {
            Some(f) => (f.path.clone(), true),
            None => (String::new(), false),
        })
        .collect();
    assert_eq!(listing.files, 3);
    assert_eq!(names[0], (String::new(), false));
    assert_eq!(
        names[1..],
        [
            ("/Internal Storage/DCIM/202601_a/IMG_0001.HEIC".into(), true),
            ("/Internal Storage/DCIM/202601_a/IMG_0002.MOV".into(), true),
            ("/Internal Storage/DCIM/202601_b/IMG_0001.HEIC".into(), true),
        ]
    );
    let mov = listing.file(2).unwrap();
    assert_eq!((mov.number, mov.size), (2, Some(2048)));

    let (tx, rx) = chunks::channel();
    h.send(Request::OpenRead {
        path: mov.path.clone(),
        number: mov.number,
        files: listing.files,
        tx,
    });
    let mut reader = chunks::ChunkReader::new(rx);
    let mut buf = vec![0; 4096];
    assert_eq!(reader.read(&mut buf).unwrap(), 2048);
    assert!(reader.is_done());
    let done = until(&h, |r| matches!(r, Reply::PasteFileDone { .. }));
    assert!(matches!(
        done.last(),
        Some(Reply::PasteFileDone {
            number: 2,
            files: 3,
            result: Ok(2048),
            ..
        })
    ));

    // A folder has no stream.
    let (tx, rx) = chunks::channel();
    h.send(Request::OpenRead {
        path: "/Internal Storage/DCIM".into(),
        number: 1,
        files: 1,
        tx,
    });
    assert!(chunks::ChunkReader::new(rx).read(&mut buf).is_err());
}

#[test]
fn requests_without_a_device_fail_cleanly() {
    let cache = tempfile::tempdir().unwrap();
    let h = handle(cache.path());
    h.send(Request::List { path: "/".into() });
    assert!(matches!(next(&h), Reply::Listed { result: Err(_), .. }));
}
