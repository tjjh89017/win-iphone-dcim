use super::*;
use crate::device_fs::fake::dcim;

fn tree(path: &str, max_depth: Option<usize>, json: bool) -> String {
    tree_with(&dcim(), path, max_depth, json, false)
}

fn tree_with(
    fs: &crate::device_fs::fake::FakeFs,
    path: &str,
    max_depth: Option<usize>,
    json: bool,
    dirs_first: bool,
) -> String {
    let mut out = Vec::new();
    run(
        fs,
        &DevicePath::parse(path).unwrap(),
        max_depth,
        json,
        dirs_first,
        &mut out,
    )
    .unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn renders_branches_from_root() {
    let expected = "\
/
└── Internal Storage
    └── DCIM
        ├── 202601_a
        │   ├── IMG_0001.HEIC  (6 B)
        │   └── IMG_0002.MOV  (2.0 KiB)
        └── 202601_b
            └── IMG_0001.HEIC  (6 B)

4 directories, 3 files
";
    assert_eq!(tree("/", None, false), expected);
}

fn mixed() -> crate::device_fs::fake::FakeFs {
    let mut fs = crate::device_fs::fake::FakeFs::new();
    fs.file(0, "b.txt", b"1");
    fs.folder(0, "Zdir");
    fs.file(0, "A.txt", b"1");
    fs.folder(0, "adir");
    fs
}

#[test]
fn sorts_by_name_mixed() {
    let out = tree_with(&mixed(), "/", None, false, false);
    let names: Vec<&str> = out.lines().skip(1).take(4).collect();
    assert_eq!(
        names,
        [
            "├── A.txt  (1 B)",
            "├── adir",
            "├── b.txt  (1 B)",
            "└── Zdir"
        ]
    );
}

#[test]
fn dirs_first_lists_folders_before_files() {
    let out = tree_with(&mixed(), "/", None, false, true);
    let names: Vec<&str> = out.lines().skip(1).take(4).collect();
    assert_eq!(
        names,
        [
            "├── adir",
            "├── Zdir",
            "├── A.txt  (1 B)",
            "└── b.txt  (1 B)"
        ]
    );
}

#[test]
fn depth_limit() {
    let expected = "\
/Internal Storage/DCIM
├── 202601_a
└── 202601_b

2 directories, 0 files
";
    assert_eq!(tree("/Internal Storage/DCIM", Some(1), false), expected);
}

#[test]
fn json_lines_have_depth_and_path() {
    let out = tree("/Internal Storage/DCIM/202601_b", None, true);
    let lines: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["depth"], 0);
    assert_eq!(lines[0]["path"], "/Internal Storage/DCIM/202601_b");
    assert_eq!(lines[1]["depth"], 1);
    assert_eq!(
        lines[1]["path"],
        "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC"
    );
    assert_eq!(lines[1]["size"], 6);
}
