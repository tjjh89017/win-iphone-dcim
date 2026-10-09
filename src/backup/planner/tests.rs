use std::cell::Cell;
use std::io::Write;

use super::*;
use crate::device_fs::fake::{FakeFs, dcim};

fn p(list: &[&str]) -> Vec<DevicePath> {
    list.iter().map(|s| DevicePath::parse(s).unwrap()).collect()
}

/// Simplified plan: ("dir"|"copy"|"error", source, target relative to `base`).
fn plan(
    fs: &dyn DeviceFs,
    sources: &[&str],
    dest: &Path,
    recursive: bool,
) -> Result<Vec<(&'static str, String, String)>> {
    let srcs = p(sources);
    let base = dest
        .ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == "D"))
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    let rel = |t: &Path| {
        t.strip_prefix(&base)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    };
    let planner = Planner::new(fs, &srcs, dest, PlanOptions { recursive })?;
    Ok(planner
        .map(|item| match item {
            PlanItem::Dir { source, target } => ("dir", source, rel(&target)),
            PlanItem::Copy(c) => ("copy", c.source, rel(&c.target)),
            PlanItem::Error { source, error } => ("error", source, error.to_string()),
        })
        .collect())
}

fn tmp_dest() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("D");
    std::fs::create_dir(&dest).unwrap();
    (tmp, dest)
}

const DCIM: &str = "/Internal Storage/DCIM";

#[test]
fn folder_without_trailing_slash_creates_named_folder() {
    let (_t, dest) = tmp_dest();
    let items = plan(&dcim(), &[DCIM], &dest, true).unwrap();
    let targets: Vec<_> = items.iter().map(|(k, _, t)| format!("{k} {t}")).collect();
    assert_eq!(
        targets,
        [
            "dir D/DCIM",
            "dir D/DCIM/202601_a",
            "copy D/DCIM/202601_a/IMG_0001.HEIC",
            "copy D/DCIM/202601_a/IMG_0002.MOV",
            "dir D/DCIM/202601_b",
            "copy D/DCIM/202601_b/IMG_0001.HEIC",
        ]
    );
}

#[test]
fn trailing_slash_copies_contents() {
    let (_t, dest) = tmp_dest();
    let items = plan(&dcim(), &["/Internal Storage/DCIM/"], &dest, true).unwrap();
    let targets: Vec<_> = items.iter().map(|(k, _, t)| format!("{k} {t}")).collect();
    assert_eq!(
        targets,
        [
            "dir D",
            "dir D/202601_a",
            "copy D/202601_a/IMG_0001.HEIC",
            "copy D/202601_a/IMG_0002.MOV",
            "dir D/202601_b",
            "copy D/202601_b/IMG_0001.HEIC",
        ]
    );
}

#[test]
fn same_file_name_in_two_folders_keeps_its_folder() {
    let (_t, dest) = tmp_dest();
    let items = plan(&dcim(), &[DCIM], &dest, true).unwrap();
    let heic: Vec<_> = items
        .iter()
        .filter(|(k, s, _)| *k == "copy" && s.ends_with("IMG_0001.HEIC"))
        .map(|(_, s, t)| (s.as_str(), t.as_str()))
        .collect();
    assert_eq!(
        heic,
        [
            (
                "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC",
                "D/DCIM/202601_a/IMG_0001.HEIC"
            ),
            (
                "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC",
                "D/DCIM/202601_b/IMG_0001.HEIC"
            ),
        ]
    );
}

#[test]
fn folder_needs_recursive() {
    let (_t, dest) = tmp_dest();
    let items = plan(&dcim(), &[DCIM], &dest, false).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, "error");
    assert!(items[0].2.contains("use -r"), "{}", items[0].2);
}

#[test]
fn two_sources_need_existing_folder() {
    let (_t, dest) = tmp_dest();
    let missing = dest.join("missing");
    let err = plan(&dcim(), &[DCIM, DCIM], &missing, true).unwrap_err();
    assert!(matches!(err, Error::DestNotFolder(_)));
    let file = dest.join("file");
    std::fs::write(&file, b"x").unwrap();
    let err = plan(&dcim(), &[DCIM, DCIM], &file, true).unwrap_err();
    assert!(matches!(err, Error::DestNotFolder(_)));
}

#[test]
fn two_sources_into_existing_folder() {
    let (_t, dest) = tmp_dest();
    let items = plan(
        &dcim(),
        &[
            "/Internal Storage/DCIM/202601_a",
            "/Internal Storage/DCIM/202601_b/",
        ],
        &dest,
        true,
    )
    .unwrap();
    let targets: Vec<_> = items.iter().map(|(k, _, t)| format!("{k} {t}")).collect();
    assert_eq!(
        targets,
        [
            "dir D/202601_a",
            "copy D/202601_a/IMG_0001.HEIC",
            "copy D/202601_a/IMG_0002.MOV",
            "dir D",
            "copy D/IMG_0001.HEIC",
        ]
    );
}

#[test]
fn one_folder_to_missing_dest_creates_dest_with_contents() {
    let (_t, dest) = tmp_dest();
    let new = dest.join("New");
    for src in [
        "/Internal Storage/DCIM/202601_a",
        "/Internal Storage/DCIM/202601_a/",
    ] {
        let items = plan(&dcim(), &[src], &new, true).unwrap();
        let targets: Vec<_> = items.iter().map(|(k, _, t)| format!("{k} {t}")).collect();
        assert_eq!(
            targets,
            [
                "dir D/New",
                "copy D/New/IMG_0001.HEIC",
                "copy D/New/IMG_0002.MOV"
            ],
            "{src}"
        );
    }
}

#[test]
fn folder_onto_existing_file_fails() {
    let (_t, dest) = tmp_dest();
    let file = dest.join("file");
    std::fs::write(&file, b"x").unwrap();
    let items = plan(&dcim(), &[DCIM], &file, true).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, "error");
    assert!(items[0].2.contains("not a folder"), "{}", items[0].2);
}

#[test]
fn single_file_rules_stay() {
    let (_t, dest) = tmp_dest();
    let f = "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC";
    let items = plan(&dcim(), &[f], &dest, false).unwrap();
    assert_eq!(items[0].2, "D/IMG_0001.HEIC");
    let items = plan(&dcim(), &[f], &dest.join("b.heic"), false).unwrap();
    assert_eq!(items[0].2, "D/b.heic");
    let items = plan(&dcim(), &[&format!("{f}/")], &dest, false).unwrap();
    assert_eq!(items[0].0, "error");
}

#[test]
fn root_source_copies_contents() {
    let (_t, dest) = tmp_dest();
    let items = plan(&dcim(), &["/"], &dest, true).unwrap();
    assert_eq!(items[0], ("dir", "/".into(), "D".into()));
    assert_eq!(items[1].2, "D/Internal Storage");
}

#[test]
fn missing_source_is_an_error_item() {
    let (_t, dest) = tmp_dest();
    let items = plan(&dcim(), &["/Internal Storage/X", DCIM], &dest, true).unwrap();
    assert_eq!(items[0].0, "error");
    assert_eq!(items[1].0, "dir");
}

#[test]
fn case_collision_reports_both_and_copies_neither() {
    let mut fs = FakeFs::new();
    let d = fs.folder(0, "DCIM");
    fs.file(d, "IMG_0001.HEIC", b"1");
    fs.file(d, "img_0001.heic", b"2");
    fs.file(d, "IMG_0002.HEIC", b"3");
    let (_t, dest) = tmp_dest();
    let items = plan(&fs, &["/DCIM/"], &dest, true).unwrap();
    let kinds: Vec<_> = items.iter().map(|(k, s, _)| format!("{k} {s}")).collect();
    assert_eq!(
        kinds,
        [
            "dir /DCIM",
            "error /DCIM/IMG_0001.HEIC",
            "error /DCIM/img_0001.heic",
            "copy /DCIM/IMG_0002.HEIC"
        ]
    );
    assert!(items[1].2.contains("img_0001.heic"), "{}", items[1].2);
}

#[test]
fn case_collision_between_folders_skips_both_subtrees() {
    let mut fs = FakeFs::new();
    let a = fs.folder(0, "Album");
    fs.file(a, "x", b"1");
    let b = fs.folder(0, "ALBUM");
    fs.file(b, "y", b"2");
    let (_t, dest) = tmp_dest();
    let items = plan(&fs, &["/"], &dest, true).unwrap();
    assert_eq!(items.iter().filter(|i| i.0 == "error").count(), 2);
    assert_eq!(items.iter().filter(|i| i.0 == "copy").count(), 0);
}

#[test]
fn unsafe_names_are_errors_not_renamed() {
    let mut fs = FakeFs::new();
    let d = fs.folder(0, "DCIM");
    for bad in ["CON", "a?b", "end.", "..", "x\\y"] {
        fs.file(d, bad, b"1");
    }
    fs.folder(d, "NUL");
    fs.file(d, "ok.jpg", b"1");
    let (_t, dest) = tmp_dest();
    let items = plan(&fs, &["/DCIM"], &dest, true).unwrap();
    assert_eq!(items.iter().filter(|i| i.0 == "error").count(), 6);
    assert_eq!(items.last().unwrap().2, "D/DCIM/ok.jpg");
}

#[test]
fn unicode_names_are_kept() {
    let mut fs = FakeFs::new();
    let d = fs.folder(0, "旅行 🗾");
    fs.file(d, "照片🎉.HEIC", b"1");
    let (_t, dest) = tmp_dest();
    let items = plan(&fs, &["/旅行 🗾"], &dest, true).unwrap();
    assert_eq!(items[1].2, "D/旅行 🗾/照片🎉.HEIC");
}

#[test]
fn skip_dir_drops_the_subtree() {
    let fs = dcim();
    let (_t, dest) = tmp_dest();
    let srcs = p(&[DCIM]);
    let mut planner = Planner::new(&fs, &srcs, &dest, PlanOptions { recursive: true }).unwrap();
    let mut copies = 0;
    while let Some(item) = planner.next() {
        match item {
            PlanItem::Dir { target, .. } if target.ends_with("202601_a") => planner.skip_dir(),
            PlanItem::Copy(_) => copies += 1,
            _ => {}
        }
    }
    assert_eq!(copies, 1);
}

/// Counts `list` calls, to show that folders are listed only when reached.
struct Counting<'a>(&'a FakeFs, Cell<usize>);

impl DeviceFs for Counting<'_> {
    fn root(&self) -> Node {
        self.0.root()
    }
    fn list(&self, dir: &Node) -> Result<Vec<Node>> {
        self.1.set(self.1.get() + 1);
        self.0.list(dir)
    }
    fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64> {
        self.0.read_to(file, out)
    }
}

#[test]
fn enumerates_lazily() {
    let fake = dcim();
    let fs = Counting(&fake, Cell::new(0));
    let (_t, dest) = tmp_dest();
    let srcs = p(&[DCIM]);
    let mut planner = Planner::new(&fs, &srcs, &dest, PlanOptions { recursive: true }).unwrap();
    // Resolving the source lists `/` and `Internal Storage`.
    assert!(matches!(planner.next(), Some(PlanItem::Dir { .. })));
    assert_eq!(fs.1.get(), 2);
    // DCIM is listed, then 202601_a is reached but not listed yet.
    assert!(matches!(planner.next(), Some(PlanItem::Dir { .. })));
    assert_eq!(fs.1.get(), 3);
    assert!(matches!(planner.next(), Some(PlanItem::Copy(_))));
    assert_eq!(fs.1.get(), 4);
    // 202601_b is not listed before its turn.
    assert!(matches!(planner.next(), Some(PlanItem::Copy(_))));
    assert_eq!(fs.1.get(), 4);
}
