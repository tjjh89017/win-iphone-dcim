use super::*;
use crate::device_fs::fake::FakeFs;

fn file_fs(data: &[u8]) -> (FakeFs, Node) {
    let mut fs = FakeFs::new();
    let i = fs.file(0, "IMG_0001.HEIC", data);
    let node = fs.node_mut(i).clone();
    (fs, node)
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn part_names_are_unique_and_recognized() {
    let target = Path::new("dir").join("IMG_0001.HEIC");
    let a = part_path(&target);
    let b = part_path(&target);
    assert_ne!(a, b);
    assert_eq!(a.parent(), target.parent());
    let name = a.file_name().unwrap();
    assert!(name.to_str().unwrap().starts_with("IMG_0001.HEIC."));
    assert!(is_part_file(name));
    assert!(!is_part_file(OsStr::new("IMG_0001.HEIC")));
    assert!(!is_part_file(OsStr::new("notes.part")));
    assert!(!is_part_file(OsStr::new("x.zzzzzzzzzzzzzzzz.part")));
    assert!(is_part_file(OsStr::new("x.0123456789abcdef.part")));
}

#[test]
fn transfer_writes_final_file_and_no_part() {
    let tmp = tempfile::tempdir().unwrap();
    let (fs, node) = file_fs(b"hello");
    let target = tmp.path().join("IMG_0001.HEIC");
    let mut seen = 0;
    let rep = transfer(&fs, &node, &target, false, false, &mut |n| seen += n).unwrap();
    assert_eq!(rep.bytes, 5);
    assert_eq!(seen, 5);
    assert_eq!(rep.verification, Verification::SizeOk);
    assert_eq!(std::fs::read(&target).unwrap(), b"hello");
    assert_eq!(files_in(tmp.path()), ["IMG_0001.HEIC"]);
}

#[test]
fn transfer_hashes_while_streaming() {
    let tmp = tempfile::tempdir().unwrap();
    let (fs, node) = file_fs(b"hello");
    let target = tmp.path().join("IMG_0001.HEIC");
    let rep = transfer(&fs, &node, &target, false, true, &mut |_| {}).unwrap();
    assert_eq!(rep.verification, Verification::LocalHash);
    assert_eq!(rep.hash, Some(*blake3::hash(b"hello").as_bytes()));
}

#[test]
fn missing_size_is_unverified() {
    let tmp = tempfile::tempdir().unwrap();
    let (fs, mut node) = file_fs(b"hello");
    node.size = None;
    let rep = transfer(&fs, &node, &tmp.path().join("x"), false, false, &mut |_| {}).unwrap();
    assert_eq!(rep.verification, Verification::SizeUnavailable);
}

#[test]
fn size_mismatch_removes_part_and_reports() {
    let tmp = tempfile::tempdir().unwrap();
    for wrong in [4, 6] {
        let (fs, mut node) = file_fs(b"hello");
        node.size = Some(wrong);
        let target = tmp.path().join("x");
        let err = transfer(&fs, &node, &target, false, false, &mut |_| {}).unwrap_err();
        assert!(
            matches!(err, Error::SizeMismatch { written: 5, expected, .. } if expected == wrong)
        );
        assert!(files_in(tmp.path()).is_empty());
    }
}

#[test]
fn read_failure_removes_part() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fs = FakeFs::new();
    let i = fs.file(0, "big.mov", &[7u8; 1000]);
    fs.fail_read(i, 300, false);
    let node = fs.node_mut(i).clone();
    let target = tmp.path().join("big.mov");
    assert!(transfer(&fs, &node, &target, false, false, &mut |_| {}).is_err());
    assert!(files_in(tmp.path()).is_empty());
}

#[test]
fn commit_refuses_existing_target() {
    let tmp = tempfile::tempdir().unwrap();
    let part = tmp.path().join("a.0123456789abcdef.part");
    let target = tmp.path().join("a");
    std::fs::write(&part, b"new").unwrap();
    std::fs::write(&target, b"old").unwrap();
    let err = commit_no_clobber(&part, &target).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(std::fs::read(&part).unwrap(), b"new");
}

#[test]
fn commit_moves_when_target_is_free() {
    let tmp = tempfile::tempdir().unwrap();
    let part = tmp.path().join("a.0123456789abcdef.part");
    let target = tmp.path().join("a");
    std::fs::write(&part, b"new").unwrap();
    commit_no_clobber(&part, &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert!(!part.exists());
}

#[test]
fn commit_replace_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let part = tmp.path().join("a.0123456789abcdef.part");
    let target = tmp.path().join("a");
    std::fs::write(&part, b"new").unwrap();
    std::fs::write(&target, b"old").unwrap();
    commit_replace(&part, &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert!(!part.exists());
}

#[test]
fn transfer_without_replace_keeps_existing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (fs, node) = file_fs(b"hello");
    let target = tmp.path().join("IMG_0001.HEIC");
    std::fs::write(&target, b"old").unwrap();
    let err = transfer(&fs, &node, &target, false, false, &mut |_| {}).unwrap_err();
    assert!(matches!(err, Error::OutputExists(_)));
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(files_in(tmp.path()), ["IMG_0001.HEIC"]);
}

#[test]
fn leftover_parts_are_listed_not_used() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.0123456789abcdef.part"), b"x").unwrap();
    std::fs::write(tmp.path().join("b.jpg"), b"x").unwrap();
    let parts = leftover_parts(tmp.path());
    assert_eq!(parts, [tmp.path().join("a.0123456789abcdef.part")]);
}

#[test]
fn set_file_times_sets_modified() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("f");
    std::fs::write(&path, b"x").unwrap();
    let t = LocalTime::parse("2021-06-15 10:20:30");
    set_file_times(&path, Some(t), Some(t)).unwrap();
    let got = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(got, local_to_system_time(t).unwrap());
}

#[test]
fn long_target_path_works() {
    let tmp = tempfile::tempdir().unwrap();
    let mut dir = crate::paths::normalize_local(tmp.path()).unwrap();
    for i in 0..4 {
        dir.push(format!("{i}{}", "d".repeat(90)));
    }
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join(format!("照片🎉{}.HEIC", "f".repeat(60)));
    assert!(target.as_os_str().len() > 300);
    let (fs, node) = file_fs(b"long");
    transfer(&fs, &node, &target, false, false, &mut |_| {}).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"long");
}
