use super::*;
use crate::device_fs::fake::dcim;

const DCIM: &str = "/Internal Storage/DCIM";
const A: &str = "/Internal Storage/DCIM/202601_a";
const A1: &str = "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC";
const A2: &str = "/Internal Storage/DCIM/202601_a/IMG_0002.MOV";
const B: &str = "/Internal Storage/DCIM/202601_b";
const B1: &str = "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC";

fn row(name: &str, folder: bool, size: Option<u64>, modified: Option<i64>) -> Entry {
    Entry {
        path: format!("/{name}"),
        name: name.into(),
        is_folder: folder,
        size,
        modified: modified.map(LocalTime),
        created: None,
        content_type: None,
    }
}

fn sorted(rows: &[Entry], key: SortKey, descending: bool) -> Vec<&str> {
    let mut v: Vec<&Entry> = rows.iter().collect();
    sort_rows(&mut v, key, descending);
    v.iter().map(|e| e.name.as_str()).collect()
}

fn sample() -> Vec<Entry> {
    vec![
        row("b.mov", false, Some(30), Some(3)),
        row("Zeta", true, None, None),
        row("a.heic", false, Some(10), Some(1)),
        row("nosize.jpg", false, None, None),
        row("alpha", true, Some(99), Some(9)),
        row("c.png", false, Some(20), Some(2)),
    ]
}

#[test]
fn name_sort_mixes_folders_and_files_like_ls() {
    let rows = sample();
    assert_eq!(
        sorted(&rows, SortKey::Name, false),
        ["a.heic", "alpha", "b.mov", "c.png", "nosize.jpg", "Zeta"]
    );
    assert_eq!(
        sorted(&rows, SortKey::Name, true),
        ["Zeta", "nosize.jpg", "c.png", "b.mov", "alpha", "a.heic"]
    );
}

#[test]
fn size_sort_puts_folders_first_and_missing_last() {
    let rows = sample();
    assert_eq!(
        sorted(&rows, SortKey::Size, false),
        ["alpha", "Zeta", "a.heic", "c.png", "b.mov", "nosize.jpg"]
    );
    // Descending flips only the files; the folders keep the name order.
    assert_eq!(
        sorted(&rows, SortKey::Size, true),
        ["alpha", "Zeta", "b.mov", "c.png", "a.heic", "nosize.jpg"]
    );
}

#[test]
fn time_sort_puts_folders_first_and_missing_last() {
    let rows = sample();
    assert_eq!(
        sorted(&rows, SortKey::Time, false),
        ["alpha", "Zeta", "a.heic", "c.png", "b.mov", "nosize.jpg"]
    );
    assert_eq!(
        sorted(&rows, SortKey::Time, true),
        ["alpha", "Zeta", "b.mov", "c.png", "a.heic", "nosize.jpg"]
    );
}

/// Load the whole fake device into a tree, as the GUI does on expand.
fn loaded(fs: &dyn DeviceFs) -> Tree {
    let root = fs.root();
    let mut tree = Tree::new(Entry::new(None, &root));
    let mut stack = vec![(root, "/".to_owned())];
    while let Some((node, path)) = stack.pop() {
        let children = fs.list(&node).unwrap();
        let entries: Vec<Entry> = children
            .iter()
            .map(|c| Entry::new(Some(&path), c))
            .collect();
        for (c, e) in children.into_iter().zip(&entries) {
            if c.is_folder {
                stack.push((c, e.path.clone()));
            }
        }
        tree.set_children(&path, entries);
    }
    tree
}

#[test]
fn top_paths_lists_the_highest_fully_checked_objects() {
    let mut tree = loaded(&dcim());
    assert!(tree.selection().top_paths().is_empty());
    tree.set_checked(A1, true);
    tree.set_checked(B1, true);
    let mut top = tree.selection().top_paths();
    top.sort();
    // B1 is the only file of B, so B is fully checked.
    assert_eq!(top, [A1, B]);
    tree.set_checked(DCIM, true);
    assert_eq!(tree.selection().top_paths(), ["/"]);
}

#[test]
fn totals_add_up_loaded_files_and_folders() {
    let tree = loaded(&dcim());
    assert_eq!(tree.totals(&[A2.into()]), Some((1, 2048)));
    assert_eq!(tree.totals(&[A.into(), B1.into()]), Some((3, 2060)));
    assert_eq!(tree.totals(&["/".into()]), Some((3, 2060)));
    assert_eq!(tree.totals(&[]), Some((0, 0)));
}

#[test]
fn files_under_lists_folders_before_their_contents() {
    let tree = loaded(&dcim());
    let items = tree.files_under(&[A.into(), B1.into()]).unwrap();
    let got: Vec<(&str, &str, bool)> = items
        .iter()
        .map(|i| (i.path.as_str(), i.rel.as_str(), i.is_folder))
        .collect();
    assert_eq!(
        got,
        [
            (A, "202601_a", true),
            (A1, "202601_a\\IMG_0001.HEIC", false),
            (A2, "202601_a\\IMG_0002.MOV", false),
            (B1, "202601_b\\IMG_0001.HEIC", false),
        ]
    );
    let a2 = items.iter().find(|i| i.path == A2).unwrap();
    assert_eq!(a2.size, Some(2048));
    assert_eq!(a2.modified, tree.entry(A2).unwrap().modified);
    assert_eq!(a2.created, tree.entry(A2).unwrap().created);
}

#[test]
fn files_under_one_file_is_its_name() {
    let tree = loaded(&dcim());
    let items = tree.files_under(&[A2.into()]).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].rel, "IMG_0002.MOV");
    assert_eq!(tree.files_under(&[]), Some(vec![]));
}

#[test]
fn files_under_is_unknown_with_an_unloaded_folder() {
    let mut tree = loaded(&dcim());
    let mut b = tree.entry(B).unwrap().clone();
    b.name = "202601_c".into();
    b.path = format!("{DCIM}/202601_c");
    let mut kids: Vec<Entry> = tree
        .children(DCIM)
        .unwrap()
        .iter()
        .map(|p| tree.entry(p).unwrap().clone())
        .collect();
    kids.push(b.clone());
    tree.set_children(DCIM, kids);
    assert!(tree.files_under(&[A.into()]).is_some());
    assert_eq!(tree.files_under(&[b.path.clone()]), None);
    assert_eq!(tree.files_under(&[DCIM.into()]), None);
    assert_eq!(tree.files_under(&["/missing".into()]), None);
}

#[test]
fn totals_are_unknown_with_an_unloaded_folder() {
    let mut tree = loaded(&dcim());
    // Add a folder to DCIM that is not listed yet.
    let mut b = tree.entry(B).unwrap().clone();
    b.name = "202601_c".into();
    b.path = format!("{DCIM}/202601_c");
    let mut kids: Vec<Entry> = tree
        .children(DCIM)
        .unwrap()
        .iter()
        .map(|p| tree.entry(p).unwrap().clone())
        .collect();
    kids.push(b.clone());
    tree.set_children(DCIM, kids);
    assert_eq!(tree.totals(&[A.into()]), Some((2, 2054)));
    assert_eq!(tree.totals(&[A.into(), b.path.clone()]), None);
    assert_eq!(tree.totals(&[DCIM.into()]), None);
    assert_eq!(tree.totals(&["/missing".into()]), None);
}

#[test]
fn checking_a_folder_checks_everything_below() {
    let mut tree = loaded(&dcim());
    tree.set_checked(DCIM, true);
    for p in [DCIM, A, A1, A2, B, B1] {
        assert_eq!(tree.check(p), Check::Checked, "{p}");
    }
    assert_eq!(tree.check("/Internal Storage"), Check::Checked);
    assert_eq!(tree.check("/"), Check::Checked);
    let mut folders = tree.loaded_folders(DCIM);
    folders.sort();
    assert_eq!(folders, [DCIM, A, B]);
}

#[test]
fn unchecking_one_child_makes_the_parents_partial() {
    let mut tree = loaded(&dcim());
    tree.set_checked(DCIM, true);
    tree.toggle(A2);
    assert_eq!(tree.check(A2), Check::Unchecked);
    assert_eq!(tree.check(A1), Check::Checked);
    assert_eq!(tree.check(A), Check::Partial);
    assert_eq!(tree.check(DCIM), Check::Partial);
    assert_eq!(tree.check(B), Check::Checked);
    // Checking it again makes the parents checked.
    tree.toggle(A2);
    assert_eq!(tree.check(DCIM), Check::Checked);
    // A click on a partial folder checks all of it.
    tree.toggle(A1);
    tree.toggle(DCIM);
    assert_eq!(tree.check(A1), Check::Checked);
    tree.toggle(DCIM);
    assert_eq!(tree.check("/"), Check::Unchecked);
    assert!(tree.selection().is_empty());
}

#[test]
fn lazily_loaded_children_take_the_folder_mark() {
    let fs = dcim();
    let root = fs.root();
    let mut tree = Tree::new(Entry::new(None, &root));
    let top: Vec<Entry> = fs
        .list(&root)
        .unwrap()
        .iter()
        .map(|c| Entry::new(Some("/"), c))
        .collect();
    tree.set_children("/", top);
    tree.set_checked("/Internal Storage", true);
    assert_eq!(tree.check("/"), Check::Checked);
    let storage = fs.list(&root).unwrap().remove(0);
    let below: Vec<Entry> = fs
        .list(&storage)
        .unwrap()
        .iter()
        .map(|c| Entry::new(Some("/Internal Storage"), c))
        .collect();
    tree.set_children("/Internal Storage", below);
    assert_eq!(tree.check(DCIM), Check::Checked);
    assert_eq!(tree.selection().copy_root().as_deref(), Some("/"));
}

#[test]
fn copy_root_is_the_deepest_common_object() {
    let mut tree = loaded(&dcim());
    tree.set_checked(A1, true);
    assert_eq!(tree.selection().copy_root().as_deref(), Some(A1));
    tree.set_checked(A2, true);
    assert_eq!(tree.selection().copy_root().as_deref(), Some(A));
    tree.set_checked(A, false);
    tree.set_checked(A2, true);
    tree.set_checked(B1, true);
    assert_eq!(tree.selection().copy_root().as_deref(), Some(DCIM));
    assert_eq!(Selection::default().copy_root(), None);
}

fn rows(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("/f{i}")).collect()
}

#[test]
fn plain_and_ctrl_clicks_select_rows() {
    let r = rows(5);
    let mut s = ListSelection::default();
    s.click(&r, 1, false, false);
    s.click(&r, 3, false, false);
    assert_eq!(s.paths(&r), ["/f3"]);
    s.click(&r, 0, true, false);
    assert_eq!(s.paths(&r), ["/f0", "/f3"]);
    s.click(&r, 3, true, false);
    assert_eq!(s.paths(&r), ["/f0"]);
    s.select_all(&r);
    assert_eq!(s.len(), 5);
    s.clear();
    assert!(s.is_empty());
}

#[test]
fn shift_click_selects_a_range_from_the_anchor() {
    let r = rows(6);
    let mut s = ListSelection::default();
    s.click(&r, 2, false, false);
    s.click(&r, 4, false, true);
    assert_eq!(s.paths(&r), ["/f2", "/f3", "/f4"]);
    // The anchor stays: a second range replaces the first.
    s.click(&r, 0, false, true);
    assert_eq!(s.paths(&r), ["/f0", "/f1", "/f2"]);
    // Ctrl+Shift adds a range to the selection.
    s.click(&r, 5, true, false);
    s.click(&r, 4, true, true);
    assert_eq!(s.paths(&r), ["/f0", "/f1", "/f2", "/f4", "/f5"]);
    // Shift without an anchor selects one row.
    let mut t = ListSelection::default();
    t.click(&r, 3, false, true);
    assert_eq!(t.paths(&r), ["/f3"]);
}

#[test]
fn right_click_keeps_or_replaces_the_selection() {
    let r = rows(4);
    let mut s = ListSelection::default();
    s.click(&r, 0, false, false);
    s.click(&r, 2, false, true);
    s.context_click(&r, 1);
    assert_eq!(s.len(), 3);
    s.context_click(&r, 3);
    assert_eq!(s.paths(&r), ["/f3"]);
}

#[test]
fn band_rows_cover_the_touched_rows() {
    // Rows of 20 units.
    assert_eq!(band_rows(5.0, 45.0, 20.0, 10), 0..3);
    assert_eq!(band_rows(45.0, 5.0, 20.0, 10), 0..3);
    assert_eq!(band_rows(41.0, 42.0, 20.0, 10), 2..3);
    // Above the first row and past the last row.
    assert_eq!(band_rows(-30.0, 10.0, 20.0, 10), 0..1);
    assert_eq!(band_rows(150.0, 900.0, 20.0, 10), 7..10);
    assert_eq!(band_rows(-30.0, -10.0, 20.0, 10), 0..0);
    assert_eq!(band_rows(300.0, 400.0, 20.0, 10), 0..0);
    assert_eq!(band_rows(0.0, 10.0, 20.0, 0), 0..0);
}

#[test]
fn band_replaces_or_with_ctrl_adds_to_the_selection() {
    let r = rows(8);
    let mut s = ListSelection::default();
    s.click(&r, 7, false, false);
    s.begin_band(false);
    assert!(s.band_active());
    s.update_band(&r, 1..4);
    assert_eq!(s.paths(&r), ["/f1", "/f2", "/f3"]);
    // The band shrinks while the pointer moves back.
    s.update_band(&r, 1..2);
    assert_eq!(s.paths(&r), ["/f1"]);
    s.end_band();
    assert!(!s.band_active());
    // Ctrl: the band adds to the selection before the drag.
    s.begin_band(true);
    s.update_band(&r, 5..7);
    assert_eq!(s.paths(&r), ["/f1", "/f5", "/f6"]);
    s.update_band(&r, 6..20);
    assert_eq!(s.paths(&r), ["/f1", "/f6", "/f7"]);
    s.end_band();
    // An update without a drag does nothing.
    s.update_band(&r, 0..8);
    assert_eq!(s.len(), 3);
}

#[test]
fn selected_fs_lists_only_marked_objects() {
    let fs = dcim();
    let mut tree = loaded(&fs);
    tree.set_checked(A2, true);
    tree.set_checked(B, true);
    let selection = tree.selection();
    let view = SelectedFs::new(&fs, &selection);
    let names = |path: &str| -> Vec<String> {
        let node =
            crate::device_fs::resolve(&view, &crate::devpath::DevicePath::parse(path).unwrap())
                .unwrap();
        view.list(&node)
            .unwrap()
            .iter()
            .map(Node::display_name)
            .collect()
    };
    assert_eq!(names(DCIM), ["202601_a", "202601_b"]);
    assert_eq!(names(A), ["IMG_0002.MOV"]);
    assert_eq!(names(B), ["IMG_0001.HEIC"]);
}
