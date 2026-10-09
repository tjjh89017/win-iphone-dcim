//! The GUI tree model: device objects by path, lazily loaded, with a check
//! mark per object.
//!
//! A checked folder means its whole subtree, also the parts that are not
//! loaded yet. A folder shows `Partial` when only some objects below it are
//! checked. `Selection` is a snapshot of the marks for the copy.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::io::Write;

use super::filedesc::{self, FileItem};
use crate::cmd::sort::{SortKey, desc_none_last, name_order};
use crate::device_fs::DeviceFs;
use crate::error::Result;
use crate::model::{LocalTime, Node, ObjectId, join_device_path};

/// One device object as the GUI shows it. No WPD object ID here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Device path, for example `/Internal Storage/DCIM/202601_a`.
    pub path: String,
    pub name: String,
    pub is_folder: bool,
    pub size: Option<u64>,
    pub modified: Option<LocalTime>,
    pub created: Option<LocalTime>,
    /// WPD content type GUID, for the properties window.
    pub content_type: Option<String>,
}

impl Entry {
    /// The entry of `node` in the folder `parent`. `None` for the root.
    pub fn new(parent: Option<&str>, node: &Node) -> Self {
        let name = node.display_name();
        let path = match parent {
            Some(parent) => join_device_path(parent, &name),
            None => "/".to_owned(),
        };
        Self {
            path,
            name,
            is_folder: node.is_folder,
            size: node.size,
            modified: node.modified,
            created: node.created,
            content_type: node.content_type.clone(),
        }
    }
}

/// The check state that a checkbox shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Unchecked,
    Partial,
    Checked,
}

struct Item {
    entry: Entry,
    parent: Option<String>,
    /// `None` until the folder is listed.
    children: Option<Vec<String>>,
    /// The object and everything below it are checked.
    checked: bool,
    /// The object or something below it is checked.
    any: bool,
}

/// The loaded part of the device tree, by device path.
pub struct Tree {
    root: String,
    items: HashMap<String, Item>,
}

impl Tree {
    pub fn new(root: Entry) -> Self {
        let path = root.path.clone();
        let mut items = HashMap::new();
        items.insert(
            path.clone(),
            Item {
                entry: root,
                parent: None,
                children: None,
                checked: false,
                any: false,
            },
        );
        Self { root: path, items }
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn entry(&self, path: &str) -> Option<&Entry> {
        self.items.get(path).map(|i| &i.entry)
    }

    /// The child paths of a folder in device order. `None` if not loaded.
    pub fn children(&self, path: &str) -> Option<&[String]> {
        self.items.get(path)?.children.as_deref()
    }

    /// The loaded folders at and below `path`.
    pub fn loaded_folders(&self, path: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![path.to_owned()];
        while let Some(p) = stack.pop() {
            let Some(item) = self.items.get(&p) else {
                continue;
            };
            if !item.entry.is_folder {
                continue;
            }
            stack.extend(item.children.iter().flatten().cloned());
            out.push(p);
        }
        out
    }

    /// The file count and bytes at and below `paths`, like the planner of a
    /// copy counts them. A file without a size counts 0 bytes. `None` if a
    /// path or a folder below it is not loaded.
    pub fn totals(&self, paths: &[String]) -> Option<(u64, u64)> {
        let (mut files, mut bytes) = (0, 0);
        let mut stack: Vec<&str> = paths.iter().map(String::as_str).collect();
        while let Some(p) = stack.pop() {
            let item = self.items.get(p)?;
            if item.entry.is_folder {
                stack.extend(item.children.as_ref()?.iter().map(String::as_str));
            } else {
                files += 1;
                bytes += item.entry.size.unwrap_or(0);
            }
        }
        Some((files, bytes))
    }

    /// Every object at and below `paths` for an Explorer paste, in the
    /// order of the device thread's walk: each folder before its contents.
    /// Relative paths start at the common parent of `paths`. `None` if a
    /// path or a folder below it is not loaded.
    pub fn files_under(&self, paths: &[String]) -> Option<Vec<FileItem>> {
        let base = filedesc::common_parent(paths);
        let mut items = Vec::new();
        for path in paths {
            let rel = filedesc::relative(&base, path)?;
            let mut stack = vec![(path.as_str(), rel)];
            while let Some((p, rel)) = stack.pop() {
                let item = self.items.get(p)?;
                let e = &item.entry;
                items.push(FileItem {
                    path: e.path.clone(),
                    rel: rel.clone(),
                    is_folder: e.is_folder,
                    size: e.size,
                    modified: e.modified,
                    created: e.created,
                });
                if e.is_folder {
                    // Reversed, so the stack pops them in listing order.
                    for child in item.children.as_ref()?.iter().rev() {
                        let name = &self.items.get(child)?.entry.name;
                        stack.push((child.as_str(), format!("{rel}\\{name}")));
                    }
                }
            }
        }
        Some(items)
    }

    /// Set the listing of a folder. A new child takes the mark of the
    /// folder. A child that was loaded before keeps its mark and subtree.
    pub fn set_children(&mut self, path: &str, children: Vec<Entry>) {
        let Some(item) = self.items.get(path) else {
            return;
        };
        let inherit = item.checked;
        let old = item.children.clone().unwrap_or_default();
        let mut paths = Vec::with_capacity(children.len());
        for entry in children {
            let child = entry.path.clone();
            if paths.contains(&child) {
                // Two objects with the same name share one path.
                continue;
            }
            paths.push(child.clone());
            match self.items.get_mut(&child) {
                Some(existing) => existing.entry = entry,
                None => {
                    self.items.insert(
                        child,
                        Item {
                            entry,
                            parent: Some(path.to_owned()),
                            children: None,
                            checked: inherit,
                            any: inherit,
                        },
                    );
                }
            }
        }
        for gone in old.iter().filter(|p| !paths.contains(p)) {
            self.remove(gone);
        }
        if let Some(item) = self.items.get_mut(path) {
            item.children = Some(paths);
        }
        self.update_up(path);
    }

    fn remove(&mut self, path: &str) {
        if let Some(item) = self.items.remove(path) {
            for child in item.children.unwrap_or_default() {
                self.remove(&child);
            }
        }
    }

    pub fn check(&self, path: &str) -> Check {
        match self.items.get(path) {
            Some(i) if i.checked => Check::Checked,
            Some(i) if i.any => Check::Partial,
            _ => Check::Unchecked,
        }
    }

    /// Check or uncheck an object and everything below it.
    pub fn set_checked(&mut self, path: &str, checked: bool) {
        if !self.items.contains_key(path) {
            return;
        }
        self.set_subtree(path, checked);
        if let Some(parent) = self.items.get(path).and_then(|i| i.parent.clone()) {
            self.update_up(&parent);
        }
    }

    /// A click on a checkbox: unchecked and partial become checked.
    pub fn toggle(&mut self, path: &str) {
        let checked = self.check(path) != Check::Checked;
        self.set_checked(path, checked);
    }

    fn set_subtree(&mut self, path: &str, checked: bool) {
        let Some(item) = self.items.get_mut(path) else {
            return;
        };
        item.checked = checked;
        item.any = checked;
        for child in item.children.clone().unwrap_or_default() {
            self.set_subtree(&child, checked);
        }
    }

    /// Recompute the marks of `path` and its ancestors from their children.
    fn update_up(&mut self, path: &str) {
        let mut current = Some(path.to_owned());
        while let Some(path) = current {
            let Some(item) = self.items.get(&path) else {
                return;
            };
            let children = item.children.clone().unwrap_or_default();
            if !children.is_empty() {
                let marks: Vec<(bool, bool)> = children
                    .iter()
                    .filter_map(|c| self.items.get(c).map(|i| (i.checked, i.any)))
                    .collect();
                let checked = marks.iter().all(|m| m.0);
                let any = checked || marks.iter().any(|m| m.1);
                if let Some(item) = self.items.get_mut(&path) {
                    item.checked = checked;
                    item.any = any;
                }
            }
            current = self.items.get(&path).and_then(|i| i.parent.clone());
        }
    }

    /// The marks for a copy.
    pub fn selection(&self) -> Selection {
        let mut marks = HashMap::new();
        let mut kids: HashMap<String, Vec<String>> = HashMap::new();
        let mut stack = vec![self.root.clone()];
        while let Some(path) = stack.pop() {
            let Some(item) = self.items.get(&path) else {
                continue;
            };
            if item.checked {
                marks.insert(path, Mark::All);
            } else if item.any {
                let marked: Vec<String> = item
                    .children
                    .iter()
                    .flatten()
                    .filter(|c| self.items.get(*c).is_some_and(|i| i.any))
                    .cloned()
                    .collect();
                stack.extend(marked.iter().cloned());
                kids.insert(path.clone(), marked);
                marks.insert(path, Mark::Some);
            }
        }
        Selection {
            root: self.root.clone(),
            marks,
            kids,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The object and its whole subtree, loaded or not.
    All,
    /// Only the marked objects below it.
    Some,
}

/// A snapshot of the check marks. It can go to the device thread.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    root: String,
    marks: HashMap<String, Mark>,
    kids: HashMap<String, Vec<String>>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    pub fn mark(&self, path: &str) -> Option<Mark> {
        self.marks.get(path).copied()
    }

    /// The fully checked objects that no fully checked folder holds, in
    /// tree order. A folder in the list stands for its whole subtree.
    pub fn top_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(path) = stack.pop() {
            match self.mark(&path) {
                Some(Mark::All) => out.push(path),
                Some(Mark::Some) => {
                    if let Some(kids) = self.kids.get(&path) {
                        stack.extend(kids.iter().rev().cloned());
                    }
                }
                None => {}
            }
        }
        out
    }

    /// The deepest object that holds every checked object: a fully checked
    /// object, or a folder with two or more marked children. The copy
    /// starts there, like `cp -r <copy root> DEST`.
    pub fn copy_root(&self) -> Option<String> {
        let mut current = self.root.clone();
        loop {
            match self.mark(&current)? {
                Mark::All => return Some(current),
                Mark::Some => match self.kids.get(&current).map(Vec::as_slice) {
                    Some([only]) => current = only.clone(),
                    _ => return Some(current),
                },
            }
        }
    }
}

/// Sort the rows of the file list like the Details view of File Explorer.
///
/// `Name` mixes folders and files in the `ls` name order. `Size` and
/// `Time` put folders first, in name order whatever the direction, then
/// the files by the key; files without the value go last in both
/// directions. Descending is the `ls --sort` order (largest or newest
/// first); `descending` flips only the files and, for `Name`, everything.
pub fn sort_rows(entries: &mut [&Entry], key: SortKey, descending: bool) {
    fn by_value<T: Ord + Copy>(a: Option<T>, b: Option<T>, descending: bool) -> Ordering {
        if descending {
            desc_none_last(&a, &b)
        } else {
            a.is_none().cmp(&b.is_none()).then(a.cmp(&b))
        }
    }
    entries.sort_by(|a, b| match key {
        SortKey::Name | SortKey::None => {
            let o = name_order(&a.name, &b.name);
            if descending { o.reverse() } else { o }
        }
        SortKey::Size | SortKey::Time => b.is_folder.cmp(&a.is_folder).then_with(|| {
            let value = match (a.is_folder, key) {
                (true, _) => Ordering::Equal,
                (false, SortKey::Size) => by_value(a.size, b.size, descending),
                (false, _) => by_value(a.modified, b.modified, descending),
            };
            value.then_with(|| name_order(&a.name, &b.name))
        }),
    });
}

/// The highlighted rows of the file list. This is not the check marks:
/// Open and "Copy to..." act on it. Ctrl-click toggles one row,
/// Shift-click selects a range from the anchor row.
#[derive(Debug, Clone, Default)]
pub struct ListSelection {
    selected: HashSet<String>,
    anchor: Option<usize>,
    /// The rows kept during a rubber-band drag: empty, or with Ctrl the
    /// selection before the drag. `None` when no drag runs.
    band_base: Option<HashSet<String>>,
}

/// The rows that a rubber band from `y0` to `y1` touches. The values are
/// content coordinates of the list: row `i` covers `i * pitch` up to
/// `(i + 1) * pitch`.
pub fn band_rows(y0: f32, y1: f32, pitch: f32, count: usize) -> std::ops::Range<usize> {
    let (top, bottom) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
    if count == 0 || pitch <= 0.0 || bottom < 0.0 {
        return 0..0;
    }
    let first = (top.max(0.0) / pitch).floor() as usize;
    let last = ((bottom / pitch).floor() as usize).min(count - 1);
    if first > last {
        return 0..0;
    }
    first..last + 1
}

impl ListSelection {
    /// A rubber-band drag starts. With `add` (Ctrl) the rows of the band
    /// add to the current selection, else they replace it.
    pub fn begin_band(&mut self, add: bool) {
        let base = if add {
            self.selected.clone()
        } else {
            HashSet::new()
        };
        self.band_base = Some(base);
    }

    /// The band now covers `range` of `rows`.
    pub fn update_band(&mut self, rows: &[String], range: std::ops::Range<usize>) {
        let Some(base) = &self.band_base else {
            return;
        };
        let mut selected = base.clone();
        let end = range.end.min(rows.len());
        let start = range.start.min(end);
        selected.extend(rows[start..end].iter().cloned());
        self.selected = selected;
        if start < end {
            self.anchor = Some(start);
        }
    }

    pub fn end_band(&mut self) {
        self.band_base = None;
    }

    pub fn band_active(&self) -> bool {
        self.band_base.is_some()
    }

    /// A click on row `index` of `rows`.
    pub fn click(&mut self, rows: &[String], index: usize, ctrl: bool, shift: bool) {
        let Some(path) = rows.get(index) else {
            return;
        };
        match (shift, self.anchor.filter(|&a| a < rows.len())) {
            (true, Some(anchor)) => {
                if !ctrl {
                    self.selected.clear();
                }
                let (lo, hi) = if anchor <= index {
                    (anchor, index)
                } else {
                    (index, anchor)
                };
                self.selected.extend(rows[lo..=hi].iter().cloned());
            }
            _ if ctrl => {
                if !self.selected.remove(path) {
                    self.selected.insert(path.clone());
                }
                self.anchor = Some(index);
            }
            _ => {
                self.selected.clear();
                self.selected.insert(path.clone());
                self.anchor = Some(index);
            }
        }
    }

    /// A right-click on a row: a row outside the selection becomes the
    /// only selected row. A row inside keeps the selection.
    pub fn context_click(&mut self, rows: &[String], index: usize) {
        if rows.get(index).is_some_and(|p| !self.selected.contains(p)) {
            self.click(rows, index, false, false);
        }
    }

    pub fn select_all(&mut self, rows: &[String]) {
        self.selected = rows.iter().cloned().collect();
        self.anchor = None;
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.band_base = None;
    }

    pub fn contains(&self, path: &str) -> bool {
        self.selected.contains(path)
    }

    pub fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// The selected paths in row order.
    pub fn paths(&self, rows: &[String]) -> Vec<String> {
        rows.iter()
            .filter(|p| self.selected.contains(*p))
            .cloned()
            .collect()
    }
}

/// A `DeviceFs` that shows only the selected objects. The copy engine runs
/// on it, so it copies the checked set with the normal planner.
pub struct SelectedFs<'a> {
    inner: &'a dyn DeviceFs,
    selection: &'a Selection,
    paths: RefCell<HashMap<ObjectId, String>>,
}

impl<'a> SelectedFs<'a> {
    pub fn new(inner: &'a dyn DeviceFs, selection: &'a Selection) -> Self {
        Self {
            inner,
            selection,
            paths: RefCell::new(HashMap::new()),
        }
    }
}

impl DeviceFs for SelectedFs<'_> {
    fn root(&self) -> Node {
        let root = self.inner.root();
        self.paths.borrow_mut().insert(root.id.clone(), "/".into());
        root
    }

    fn list(&self, dir: &Node) -> Result<Vec<Node>> {
        let children = self.inner.list(dir)?;
        let Some(parent) = self.paths.borrow().get(&dir.id).cloned() else {
            return Ok(Vec::new());
        };
        let all = self.selection.mark(&parent) == Some(Mark::All);
        let mut paths = self.paths.borrow_mut();
        Ok(children
            .into_iter()
            .filter(|child| {
                let path = Entry::new(Some(&parent), child).path;
                let keep = all || self.selection.mark(&path).is_some();
                if keep {
                    paths.insert(child.id.clone(), path);
                }
                keep
            })
            .collect())
    }

    fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64> {
        self.inner.read_to(file, out)
    }

    fn device_id(&self) -> Option<String> {
        self.inner.device_id()
    }
}

#[cfg(test)]
mod tests {
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
}
