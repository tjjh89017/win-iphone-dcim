use super::*;
use crate::backup::transfer::local_to_system_time;
use crate::device_fs::fake::{FakeFs, dcim};
use crate::model::LocalTime;

fn paths(list: &[&str]) -> Vec<DevicePath> {
    list.iter().map(|p| DevicePath::parse(p).unwrap()).collect()
}

fn cp_fs(fs: &FakeFs, list: &[&str], dest: &Path, opts: CpOptions) -> Result<(String, usize)> {
    let mut out = Vec::new();
    let failures = run(fs, &paths(list), dest, opts, &mut out)?;
    Ok((String::from_utf8(out).unwrap(), failures))
}

fn cp(list: &[&str], dest: &Path, opts: CpOptions) -> Result<(String, usize)> {
    cp_fs(&dcim(), list, dest, opts)
}

fn opts() -> CpOptions {
    CpOptions {
        sleep: |_| {},
        ..CpOptions::default()
    }
}

fn rec() -> CpOptions {
    CpOptions {
        recursive: true,
        ..opts()
    }
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != crate::backup::manifest::DIR_NAME)
        .collect();
    v.sort();
    v
}

const DCIM: &str = "/Internal Storage/DCIM";
const A1: &str = "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC";
const A2: &str = "/Internal Storage/DCIM/202601_a/IMG_0002.MOV";
const B1: &str = "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC";

#[test]
fn copies_into_folder_with_device_name() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, failures) = cp(&[A1, A2], tmp.path(), opts()).unwrap();
    assert_eq!(failures, 0);
    assert_eq!(
        std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
        b"heic-a"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("IMG_0002.MOV"))
            .unwrap()
            .len(),
        2048
    );
    assert!(
        out.contains("[done] copied=2 skipped=0 exists=0 failed=0  total="),
        "{out}"
    );
    assert!(out.contains("size-ok"), "{out}");
}

#[test]
fn single_source_to_new_file_name() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("b.heic");
    cp(&[B1], &target, opts()).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"heic-b");
    assert_eq!(files_in(tmp.path()), ["b.heic"]);
}

#[test]
fn recursive_copy_creates_named_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, failures) = cp(&[DCIM], tmp.path(), rec()).unwrap();
    assert_eq!(failures, 0, "{out}");
    let d = tmp.path().join("DCIM");
    assert_eq!(
        std::fs::read(d.join("202601_a/IMG_0001.HEIC")).unwrap(),
        b"heic-a"
    );
    assert_eq!(
        std::fs::read(d.join("202601_b/IMG_0001.HEIC")).unwrap(),
        b"heic-b"
    );
    assert_eq!(
        d.join("202601_a/IMG_0002.MOV").metadata().unwrap().len(),
        2048
    );
    assert!(out.contains("copied=3 skipped=0"), "{out}");
}

#[test]
fn trailing_slash_copies_contents() {
    let tmp = tempfile::tempdir().unwrap();
    cp(&["/Internal Storage/DCIM/"], tmp.path(), rec()).unwrap();
    assert_eq!(files_in(tmp.path()), ["202601_a", "202601_b"]);
    assert_eq!(
        std::fs::read(tmp.path().join("202601_b/IMG_0001.HEIC")).unwrap(),
        b"heic-b"
    );
}

#[test]
fn missing_dest_gets_folder_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let new = tmp.path().join("New");
    cp(&["/Internal Storage/DCIM/202601_a"], &new, rec()).unwrap();
    assert_eq!(files_in(&new), ["IMG_0001.HEIC", "IMG_0002.MOV"]);
}

#[test]
fn folder_source_needs_recursive() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, failures) = cp(&[DCIM], tmp.path(), opts()).unwrap();
    assert_eq!(failures, 1);
    assert!(out.contains("[failed] usage: 1"), "{out}");
    assert!(out.contains("use -r"), "{out}");
    assert!(files_in(tmp.path()).is_empty());
}

#[test]
fn many_sources_need_folder_dest() {
    let tmp = tempfile::tempdir().unwrap();
    let err = cp(&[A1, B1], &tmp.path().join("missing"), opts()).unwrap_err();
    assert!(matches!(err, Error::DestNotFolder(_)));
}

#[test]
fn existing_target_is_skipped_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"old").unwrap();
    let (out, failures) = cp(&[A1, A2], tmp.path(), opts()).unwrap();
    assert_eq!(failures, 0);
    assert_eq!(
        std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
        b"old"
    );
    assert!(
        out.contains("copied=1 skipped=1 exists=1 failed=0"),
        "{out}"
    );
}

#[test]
fn no_clobber_skips_silently() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"old").unwrap();
    let opts = CpOptions {
        on_exists: OnExists::SkipQuiet,
        ..opts()
    };
    let (out, failures) = cp(&[A1], tmp.path(), opts).unwrap();
    assert_eq!(failures, 0);
    assert_eq!(
        std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
        b"old"
    );
    assert!(out.contains("copied=0 skipped=1 exists=1"), "{out}");
}

#[test]
fn force_replaces_with_new_content() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"old content").unwrap();
    let opts = CpOptions {
        on_exists: OnExists::Overwrite,
        ..opts()
    };
    let (out, failures) = cp(&[A1], tmp.path(), opts).unwrap();
    assert_eq!(failures, 0);
    assert_eq!(
        std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
        b"heic-a"
    );
    assert_eq!(files_in(tmp.path()), ["IMG_0001.HEIC"]);
    assert!(out.contains("copied=1 skipped=0 exists=0"), "{out}");
}

#[test]
fn force_keeps_old_file_when_transfer_fails() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"old").unwrap();
    let mut fs = dcim();
    // Node 4 is 202601_a/IMG_0001.HEIC.
    fs.fail_read(4, 3, false);
    let opts = CpOptions {
        on_exists: OnExists::Overwrite,
        ..opts()
    };
    let (_, failures) = cp_fs(&fs, &[A1], tmp.path(), opts).unwrap();
    assert_eq!(failures, 1);
    assert_eq!(
        std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
        b"old"
    );
    assert_eq!(files_in(tmp.path()), ["IMG_0001.HEIC"]);
}

#[test]
fn same_name_from_two_folders_skips_the_second() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, failures) = cp(&[A1, B1], tmp.path(), opts()).unwrap();
    assert_eq!(failures, 0);
    assert!(out.contains("exists=1"), "{out}");
    assert_eq!(
        std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
        b"heic-a"
    );
}

#[test]
fn dry_run_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fs = dcim();
    let dcim_dir = 2;
    fs.file(dcim_dir, "CON", b"x");
    let opts = CpOptions {
        dry_run: true,
        ..rec()
    };
    let (out, failures) = cp_fs(&fs, &[DCIM], tmp.path(), opts).unwrap();
    assert_eq!(failures, 1);
    assert!(out.contains("[plan] mkdir "), "{out}");
    assert!(
        out.contains("[plan] /Internal Storage/DCIM/202601_a/IMG_0001.HEIC -> "),
        "{out}"
    );
    assert!(
        out.contains("[error] /Internal Storage/DCIM/CON  "),
        "{out}"
    );
    assert!(
        out.contains("[done] planned=3 skipped=0 exists=0 failed=1"),
        "{out}"
    );
    assert!(files_in(tmp.path()).is_empty());
}

#[test]
fn size_mismatch_deletes_part_and_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fs = dcim();
    fs.node_mut(4).size = Some(7);
    let (out, failures) = cp_fs(&fs, &[A1], tmp.path(), opts()).unwrap();
    assert_eq!(failures, 1);
    assert!(out.contains("[failed] size mismatch: 1"), "{out}");
    assert!(files_in(tmp.path()).is_empty());
}

#[test]
fn read_failure_leaves_no_file_and_continues() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fs = dcim();
    fs.fail_read(5, 100, false);
    let (out, failures) = cp_fs(&fs, &[DCIM], tmp.path(), rec()).unwrap();
    assert_eq!(failures, 1);
    assert!(out.contains("[failed] device: 1"), "{out}");
    let a = tmp.path().join("DCIM/202601_a");
    assert_eq!(files_in(&a), ["IMG_0001.HEIC"]);
    assert!(tmp.path().join("DCIM/202601_b/IMG_0001.HEIC").exists());
}

#[test]
fn fatal_error_stops_after_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fs = dcim();
    fs.fail_read(4, 2, true);
    let mut out = Vec::new();
    let err = run(&fs, &paths(&[DCIM]), tmp.path(), rec(), &mut out).unwrap_err();
    assert!(err.is_fatal());
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("[done] copied=0"), "{out}");
    assert_eq!(
        files_in(&tmp.path().join("DCIM/202601_a")),
        Vec::<String>::new()
    );
}

#[test]
fn leftover_part_is_not_treated_as_complete() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("202601_a");
    std::fs::create_dir(&dir).unwrap();
    // A crash in an earlier run left this file.
    let leftover = dir.join("IMG_0001.HEIC.0123456789abcdef.part");
    std::fs::write(&leftover, b"hei").unwrap();
    let (out, failures) = cp(&["/Internal Storage/DCIM/202601_a"], tmp.path(), rec()).unwrap();
    assert_eq!(failures, 0);
    assert!(out.contains("copied=2"), "{out}");
    assert_eq!(std::fs::read(dir.join("IMG_0001.HEIC")).unwrap(), b"heic-a");
    assert_eq!(std::fs::read(&leftover).unwrap(), b"hei");
}

#[test]
fn case_collision_copies_neither() {
    let mut fs = FakeFs::new();
    let d = fs.folder(0, "DCIM");
    fs.file(d, "IMG_0001.HEIC", b"1");
    fs.file(d, "img_0001.heic", b"2");
    fs.file(d, "IMG_0002.HEIC", b"3");
    let tmp = tempfile::tempdir().unwrap();
    let (out, failures) = cp_fs(&fs, &["/DCIM/"], tmp.path(), rec()).unwrap();
    assert_eq!(failures, 2);
    assert!(out.contains("[failed] collision: 2"), "{out}");
    assert_eq!(files_in(tmp.path()), ["IMG_0002.HEIC"]);
}

#[test]
fn cjk_and_emoji_names_round_trip() {
    let mut fs = FakeFs::new();
    let d = fs.folder(0, "旅行 🗾");
    fs.file(d, "照片🎉.HEIC", b"x");
    let tmp = tempfile::tempdir().unwrap();
    cp_fs(&fs, &["/旅行 🗾"], tmp.path(), rec()).unwrap();
    assert_eq!(files_in(tmp.path()), ["旅行 🗾"]);
    assert_eq!(files_in(&tmp.path().join("旅行 🗾")), ["照片🎉.HEIC"]);
}

#[test]
fn long_target_path_is_copied() {
    let mut fs = FakeFs::new();
    let mut parent = 0;
    for i in 0..4 {
        parent = fs.folder(parent, &format!("{i}{}", "x".repeat(90)));
    }
    fs.file(parent, &format!("{}.MOV", "y".repeat(80)), b"long");
    let tmp = tempfile::tempdir().unwrap();
    let (out, failures) = cp_fs(&fs, &["/"], tmp.path(), rec()).unwrap();
    assert_eq!(failures, 0, "{out}");
    let mut path = tmp.path().to_path_buf();
    for i in 0..4 {
        path.push(format!("{i}{}", "x".repeat(90)));
    }
    path.push(format!("{}.MOV", "y".repeat(80)));
    assert!(path.as_os_str().len() > 300);
    assert_eq!(
        std::fs::read(crate::paths::to_verbatim(&path)).unwrap(),
        b"long"
    );
}

#[test]
fn archive_sets_modified_time() {
    let mut fs = dcim();
    let t = LocalTime::parse("2023-03-04 05:06:07");
    fs.node_mut(4).modified = Some(t);
    fs.node_mut(4).created = Some(t);
    let tmp = tempfile::tempdir().unwrap();
    let opts = CpOptions {
        preserve: true,
        ..rec()
    };
    cp_fs(&fs, &["/Internal Storage/DCIM/202601_a/"], tmp.path(), opts).unwrap();
    let m = std::fs::metadata(tmp.path().join("IMG_0001.HEIC"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(m, local_to_system_time(t).unwrap());
    // No device date: the copy time stays.
    let m2 = std::fs::metadata(tmp.path().join("IMG_0002.MOV"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(m2 > local_to_system_time(t).unwrap());
}

#[test]
fn existing_dest_folder_as_file_fails_the_subtree() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("DCIM"), b"file").unwrap();
    let (out, failures) = cp(&[DCIM], tmp.path(), rec()).unwrap();
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("[failed] target exists: 1"), "{out}");
}

// Phase 2: manifest, incremental rules, retries.

use crate::backup::manifest::{Manifest, device_key};
use std::cell::{Cell, RefCell};

const RAW_ID: &str = r"\\?\usb#vid_05ac&pid_12a8&mi_00#00008030001a2b3c4d5e6f70#{6ac27878-a6fa-4155-ba85-f98f491d4f33}";

/// A fake device that fails the first `fail_first` reads with `error`.
struct Flaky {
    inner: FakeFs,
    reads: Cell<usize>,
    fail_first: usize,
    error: fn() -> Error,
    id: Option<String>,
}

impl Flaky {
    fn new(fail_first: usize, error: fn() -> Error) -> Self {
        Self {
            inner: dcim(),
            reads: Cell::new(0),
            fail_first,
            error,
            id: Some(RAW_ID.into()),
        }
    }
}

impl DeviceFs for Flaky {
    fn root(&self) -> Node {
        self.inner.root()
    }

    fn list(&self, dir: &Node) -> Result<Vec<Node>> {
        self.inner.list(dir)
    }

    fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64> {
        let n = self.reads.get();
        self.reads.set(n + 1);
        if n < self.fail_first {
            out.write_all(b"pa").unwrap();
            return Err((self.error)());
        }
        self.inner.read_to(file, out)
    }

    fn device_id(&self) -> Option<String> {
        self.id.clone()
    }
}

fn worker_restarted() -> Error {
    Error::WorkerRestarted {
        context: "read".into(),
        reason: "watchdog timeout".into(),
    }
}

fn cp_dyn(fs: &dyn DeviceFs, list: &[&str], dest: &Path, opts: CpOptions) -> (String, usize) {
    let mut out = Vec::new();
    let failures = run(fs, &paths(list), dest, opts, &mut out).unwrap();
    (String::from_utf8(out).unwrap(), failures)
}

fn manifest_text(root: &Path) -> String {
    std::fs::read_to_string(Manifest::path_for(root)).unwrap_or_default()
}

#[test]
fn second_run_skips_verified_files() {
    let tmp = tempfile::tempdir().unwrap();
    let fs = Flaky::new(0, worker_restarted);
    let (out, _) = cp_dyn(&fs, &[DCIM], tmp.path(), rec());
    assert!(out.contains("copied=3 skipped=0"), "{out}");
    assert_eq!(manifest_text(tmp.path()).lines().count(), 3);
    let (out, failures) = cp_dyn(&fs, &[DCIM], tmp.path(), rec());
    assert_eq!(failures, 0);
    assert!(
        out.contains("[skip] /Internal Storage/DCIM/202601_a/IMG_0001.HEIC  verified"),
        "{out}"
    );
    assert!(
        out.contains("copied=0 skipped=3 exists=0 failed=0"),
        "{out}"
    );
    assert_eq!(fs.reads.get(), 3);
    assert_eq!(files_in(tmp.path()), ["DCIM"]);
    assert!(tmp.path().join(".win-iphone-dcim/manifest.jsonl").is_file());
}

#[test]
fn manifest_records_path_under_dest_and_hides_device_id() {
    let tmp = tempfile::tempdir().unwrap();
    let fs = Flaky::new(0, worker_restarted);
    cp_dyn(&fs, &[DCIM], tmp.path(), rec());
    let text = manifest_text(tmp.path());
    assert!(!text.contains(RAW_ID));
    assert!(!text.contains("00008030001a2b3c4d5e6f70"));
    assert!(!text.contains("vid_05ac"));
    assert!(text.contains(&device_key(RAW_ID)));
    let m = Manifest::load(tmp.path()).unwrap();
    let e = m.get("DCIM/202601_b/IMG_0001.HEIC").unwrap();
    assert_eq!(e.record.size, 6);
    assert_eq!(
        e.record.source.as_deref(),
        Some("/Internal Storage/DCIM/202601_b/IMG_0001.HEIC")
    );
}

#[test]
fn single_file_to_new_name_records_in_parent() {
    let tmp = tempfile::tempdir().unwrap();
    cp(&[B1], &tmp.path().join("b.heic"), opts()).unwrap();
    let m = Manifest::load(tmp.path()).unwrap();
    assert!(m.get("b.heic").is_some());
    let (out, _) = cp(&[B1], &tmp.path().join("b.heic"), opts()).unwrap();
    assert!(out.contains("verified"), "{out}");
}

#[test]
fn truncated_last_manifest_line_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    cp(&[DCIM], tmp.path(), rec()).unwrap();
    let path = Manifest::path_for(tmp.path());
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(br#"{"v":1,"path":"DCIM/202601_a/IMG_0002.M"#)
        .unwrap();
    drop(f);
    let (out, failures) = cp(&[DCIM], tmp.path(), rec()).unwrap();
    assert_eq!(failures, 0);
    assert!(out.contains("copied=0 skipped=3 exists=0"), "{out}");
    // A new record after the broken line is still readable.
    std::fs::remove_file(tmp.path().join("DCIM/202601_b/IMG_0001.HEIC")).unwrap();
    let (out, _) = cp(&[DCIM], tmp.path(), rec()).unwrap();
    assert!(out.contains("copied=1 skipped=2"), "{out}");
    let m = Manifest::load(tmp.path()).unwrap();
    assert_eq!(m.len(), 3);
}

#[test]
fn stale_record_is_a_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    cp(&[A1], tmp.path(), opts()).unwrap();
    // The record says 99 bytes; the local file and the device say 6.
    let path = Manifest::path_for(tmp.path());
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("\"size\":6", "\"size\":99")).unwrap();
    let (out, failures) = cp(&[A1], tmp.path(), opts()).unwrap();
    assert_eq!(failures, 0);
    assert!(out.contains("copied=0 skipped=1 exists=1"), "{out}");
    let force = CpOptions {
        on_exists: OnExists::Overwrite,
        ..opts()
    };
    let (out, _) = cp(&[A1], tmp.path(), force).unwrap();
    assert!(out.contains("copied=1"), "{out}");
    let m = Manifest::load(tmp.path()).unwrap();
    assert_eq!(m.get("IMG_0001.HEIC").unwrap().record.size, 6);
    let (out, _) = cp(&[A1], tmp.path(), opts()).unwrap();
    assert!(out.contains("verified"), "{out}");
}

#[test]
fn unverified_existing_is_skipped_and_not_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    // Same size as the device file, but no manifest record.
    std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"HEIC-A").unwrap();
    let (out, failures) = cp(&[A1], tmp.path(), opts()).unwrap();
    assert_eq!(failures, 0);
    assert!(out.contains("copied=0 skipped=1 exists=1"), "{out}");
    assert!(!out.contains("verified"), "{out}");
    assert!(manifest_text(tmp.path()).is_empty());
    assert_eq!(
        std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
        b"HEIC-A"
    );
    // A second run still does not claim it.
    let (out, _) = cp(&[A1], tmp.path(), opts()).unwrap();
    assert!(out.contains("exists=1"), "{out}");
    // -f replaces it and records it.
    let force = CpOptions {
        on_exists: OnExists::Overwrite,
        ..opts()
    };
    cp(&[A1], tmp.path(), force).unwrap();
    assert_eq!(
        std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
        b"heic-a"
    );
    assert!(
        Manifest::load(tmp.path())
            .unwrap()
            .get("IMG_0001.HEIC")
            .is_some()
    );
}

#[test]
fn local_hash_is_stored_and_a_mismatch_is_a_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let hashed = CpOptions {
        local_hash: true,
        ..opts()
    };
    let (out, _) = cp(&[A1], tmp.path(), hashed).unwrap();
    assert!(out.contains("local-hash"), "{out}");
    let m = Manifest::load(tmp.path()).unwrap();
    let r = &m.get("IMG_0001.HEIC").unwrap().record;
    assert_eq!(r.hash_alg.as_deref(), Some("blake3"));
    assert_eq!(
        r.hash.as_deref(),
        Some(blake3::hash(b"heic-a").to_hex().as_str())
    );
    let (out, _) = cp(&[A1], tmp.path(), hashed).unwrap();
    assert!(out.contains("verified"), "{out}");
    // Same size, other bytes.
    std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"heic-X").unwrap();
    let (out, failures) = cp(&[A1], tmp.path(), hashed).unwrap();
    assert_eq!(failures, 0);
    assert!(out.contains("copied=0 skipped=1 exists=1"), "{out}");
    // Size mode does not read the file, so it skips.
    let (out, _) = cp(&[A1], tmp.path(), opts()).unwrap();
    assert!(out.contains("verified"), "{out}");
}

#[test]
fn dry_run_prints_decisions_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    cp(&[DCIM], tmp.path(), rec()).unwrap();
    std::fs::write(tmp.path().join("DCIM/202601_b/IMG_0001.HEIC"), b"x").unwrap();
    std::fs::remove_file(tmp.path().join("DCIM/202601_a/IMG_0002.MOV")).unwrap();
    let before = manifest_text(tmp.path());
    let dry = CpOptions {
        dry_run: true,
        ..rec()
    };
    let (out, _) = cp(&[DCIM], tmp.path(), dry).unwrap();
    assert!(
        out.contains("[skip] /Internal Storage/DCIM/202601_a/IMG_0001.HEIC  verified"),
        "{out}"
    );
    let conflict = out.lines().find(|l| l.contains("  conflict:")).unwrap();
    assert!(conflict.contains("202601_b"), "{out}");
    assert!(
        out.contains("[plan] /Internal Storage/DCIM/202601_a/IMG_0002.MOV -> "),
        "{out}"
    );
    assert!(out.contains("planned=1 skipped=2 exists=1"), "{out}");
    assert_eq!(manifest_text(tmp.path()), before);
    assert!(!tmp.path().join("DCIM/202601_a/IMG_0002.MOV").exists());
}

thread_local! {
    static SLEEPS: RefCell<Vec<Duration>> = const { RefCell::new(Vec::new()) };
}

fn record_sleep(d: Duration) {
    SLEEPS.with(|s| s.borrow_mut().push(d));
}

#[test]
fn transient_error_is_retried_then_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let fs = Flaky::new(usize::MAX, worker_restarted);
    let opts = CpOptions {
        retries: 3,
        sleep: record_sleep,
        ..opts()
    };
    SLEEPS.with(|s| s.borrow_mut().clear());
    let (out, failures) = cp_dyn(&fs, &[A1], tmp.path(), opts);
    assert_eq!(failures, 1);
    assert_eq!(fs.reads.get(), 4);
    for k in 1..=3 {
        assert!(
            out.contains(&format!("[retry {k}/3] {A1}  read: the device worker")),
            "{out}"
        );
    }
    assert!(!out.contains("[retry 4/3]"), "{out}");
    assert!(out.contains("[failed] worker: 1"), "{out}");
    let secs: Vec<u64> = SLEEPS.with(|s| s.borrow().iter().map(Duration::as_secs).collect());
    assert_eq!(secs, [1, 3, 10]);
    // No partial file is left.
    assert!(files_in(tmp.path()).is_empty());
    assert_eq!(backoff(4), Duration::from_secs(10));
}

#[test]
fn transient_error_then_success_copies() {
    let tmp = tempfile::tempdir().unwrap();
    let fs = Flaky::new(2, worker_restarted);
    let (out, failures) = cp_dyn(&fs, &[A1], tmp.path(), opts());
    assert_eq!(failures, 0);
    assert_eq!(fs.reads.get(), 3);
    assert!(out.contains("[retry 2/3]"), "{out}");
    assert!(out.contains("copied=1"), "{out}");
    assert_eq!(files_in(tmp.path()), ["IMG_0001.HEIC"]);
    assert_eq!(
        std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
        b"heic-a"
    );
}

#[test]
fn permanent_error_is_not_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let fs = Flaky::new(usize::MAX, || Error::Io {
        context: "write".into(),
        source: std::io::Error::from(ErrorKind::StorageFull),
    });
    let (out, failures) = cp_dyn(&fs, &[A1], tmp.path(), opts());
    assert_eq!(failures, 1);
    assert_eq!(fs.reads.get(), 1);
    assert!(!out.contains("[retry"), "{out}");
    assert!(out.contains("[failed] io: 1"), "{out}");
}

#[test]
fn zero_retries_means_one_attempt() {
    let tmp = tempfile::tempdir().unwrap();
    let fs = Flaky::new(usize::MAX, worker_restarted);
    let opts = CpOptions {
        retries: 0,
        ..opts()
    };
    let (_, failures) = cp_dyn(&fs, &[A1], tmp.path(), opts);
    assert_eq!(failures, 1);
    assert_eq!(fs.reads.get(), 1);
}
