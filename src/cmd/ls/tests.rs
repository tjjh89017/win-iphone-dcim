use super::*;
use crate::device_fs::fake::dcim;

fn ls(paths: &[&str], opts: LsOptions) -> (String, usize) {
    let fs = dcim();
    let paths: Vec<DevicePath> = paths
        .iter()
        .map(|p| DevicePath::parse(p).unwrap())
        .collect();
    let mut out = Vec::new();
    let failures = run(&fs, &paths, opts, &mut out).unwrap();
    (String::from_utf8(out).unwrap(), failures)
}

#[test]
fn no_path_lists_root() {
    assert_eq!(
        ls(&[], LsOptions::default()),
        ("Internal Storage\n".into(), 0)
    );
}

#[test]
fn lists_one_level() {
    let (out, _) = ls(&["/Internal Storage/DCIM"], LsOptions::default());
    assert_eq!(out, "202601_a\n202601_b\n");
}

#[test]
fn long_format_shows_type_size_and_id() {
    let opts = LsOptions {
        long: true,
        ..Default::default()
    };
    let (out, _) = ls(&["/Internal Storage/DCIM/202601_a"], opts);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        "-            6  -                    IMG_0001.HEIC  [id: o4]"
    );
    assert!(lines[1].starts_with("-         2048  "));
}

#[test]
fn recursive_prints_headers() {
    let opts = LsOptions {
        recursive: true,
        ..Default::default()
    };
    let (out, _) = ls(&["/Internal Storage/DCIM"], opts);
    assert_eq!(
        out,
        "/Internal Storage/DCIM:\n202601_a\n202601_b\n\n\
             /Internal Storage/DCIM/202601_a:\nIMG_0001.HEIC\nIMG_0002.MOV\n\n\
             /Internal Storage/DCIM/202601_b:\nIMG_0001.HEIC\n"
    );
}

#[test]
fn json_has_full_paths() {
    let opts = LsOptions {
        json: true,
        ..Default::default()
    };
    let (out, _) = ls(&["/Internal Storage/DCIM/202601_b"], opts);
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["path"], "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC");
    assert_eq!(v["is_folder"], false);
    assert_eq!(v["size"], 6);
}

fn sorted_fs() -> crate::device_fs::fake::FakeFs {
    let mut fs = crate::device_fs::fake::FakeFs::new();
    fs.file(0, "b.txt", b"22");
    fs.file(0, "B.txt", b"4444");
    let n = fs.file(0, "a.txt", b"1");
    fs.node_mut(n).modified = Some(crate::model::LocalTime::parse("2024-01-01 00:00:00"));
    let z = fs.folder(0, "Zdir");
    fs.node_mut(z).modified = Some(crate::model::LocalTime::parse("2020-01-01 00:00:00"));
    let m = fs.file(0, "m.txt", b"333");
    fs.node_mut(m).modified = Some(crate::model::LocalTime::parse("2022-01-01 00:00:00"));
    fs
}

fn ls_sorted(sort: SortKey, reverse: bool) -> Vec<String> {
    let fs = sorted_fs();
    let opts = LsOptions {
        sort,
        reverse,
        ..Default::default()
    };
    let mut out = Vec::new();
    run(&fs, &[DevicePath::root()], opts, &mut out).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(Into::into)
        .collect()
}

#[test]
fn default_sorts_by_name_mixed_case() {
    assert_eq!(
        ls_sorted(SortKey::Name, false),
        ["a.txt", "B.txt", "b.txt", "m.txt", "Zdir"]
    );
}

#[test]
fn size_sort_puts_missing_size_last() {
    assert_eq!(
        ls_sorted(SortKey::Size, false),
        ["B.txt", "m.txt", "b.txt", "a.txt", "Zdir"]
    );
}

#[test]
fn time_sort_puts_missing_time_last() {
    assert_eq!(
        ls_sorted(SortKey::Time, false),
        ["a.txt", "m.txt", "Zdir", "B.txt", "b.txt"]
    );
}

#[test]
fn reverse_flips_name_order() {
    assert_eq!(
        ls_sorted(SortKey::Name, true),
        ["Zdir", "m.txt", "b.txt", "B.txt", "a.txt"]
    );
}

#[test]
fn unsorted_keeps_insertion_order() {
    assert_eq!(
        ls_sorted(SortKey::None, false),
        ["b.txt", "B.txt", "a.txt", "Zdir", "m.txt"]
    );
}

#[test]
fn recursive_sorts_every_level() {
    let mut fs = crate::device_fs::fake::FakeFs::new();
    let d = fs.folder(0, "d");
    fs.file(d, "y", b"");
    fs.file(d, "x", b"");
    fs.folder(0, "c");
    let opts = LsOptions {
        recursive: true,
        ..Default::default()
    };
    let mut out = Vec::new();
    run(&fs, &[DevicePath::root()], opts, &mut out).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "/:\nc\nd\n\n/c:\n\n/d:\nx\ny\n"
    );
}

#[test]
fn missing_path_counts_as_failure() {
    let (out, failures) = ls(&["/nope", "/Internal Storage"], LsOptions::default());
    assert_eq!(failures, 1);
    assert!(out.contains("DCIM"));
}
