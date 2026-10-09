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
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

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
    /// See `stamp`.
    stamp: u64,
}

/// A value that no tree had before, in this process.
fn next_stamp() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, AtomicOrdering::Relaxed)
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
        Self {
            root: path,
            items,
            stamp: next_stamp(),
        }
    }

    /// Changes when a listing or an entry changes, also to a value that
    /// no other tree had. Check marks do not change it.
    pub fn stamp(&self) -> u64 {
        self.stamp
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
        let mut seen = HashSet::with_capacity(children.len());
        for entry in children {
            let child = entry.path.clone();
            if !seen.insert(child.clone()) {
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
        for gone in old.iter().filter(|p| !seen.contains(*p)) {
            self.remove(gone);
        }
        if let Some(item) = self.items.get_mut(path) {
            item.children = Some(paths);
        }
        self.stamp = next_stamp();
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

/// The sorted rows of the file list, kept until the folder, the sort or the
/// tree stamp changes. The file list asks for them every frame.
#[derive(Default)]
pub struct RowCache {
    key: Option<(u64, String, SortKey, bool)>,
    rows: Rc<[String]>,
}

impl RowCache {
    /// The child paths of `folder` in `sort_rows` order.
    pub fn rows(
        &mut self,
        tree: &Tree,
        folder: &str,
        key: SortKey,
        descending: bool,
    ) -> Rc<[String]> {
        if let Some((stamp, f, k, d)) = &self.key
            && *stamp == tree.stamp()
            && f == folder
            && *k == key
            && *d == descending
        {
            return self.rows.clone();
        }
        let mut entries: Vec<&Entry> = tree
            .children(folder)
            .unwrap_or_default()
            .iter()
            .filter_map(|p| tree.entry(p))
            .collect();
        sort_rows(&mut entries, key, descending);
        self.rows = entries.into_iter().map(|e| e.path.clone()).collect();
        self.key = Some((tree.stamp(), folder.to_owned(), key, descending));
        self.rows.clone()
    }
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

/// The copy sources for `paths`. When `paths` are all the loaded children
/// of one folder, the folder contents (`<folder>/`) replace them: the copy
/// then lists the folder once and does not resolve each path. Else `paths`
/// as they are.
pub fn collapse_to_parent(paths: Vec<String>, tree: &Tree) -> Vec<String> {
    let Some(parent) = paths.first().and_then(|p| parent_of(p)) else {
        return paths;
    };
    if paths.len() < 2 || paths.iter().any(|p| parent_of(p) != Some(parent)) {
        return paths;
    }
    let Some(children) = tree.children(parent) else {
        return paths;
    };
    let set: HashSet<&str> = paths.iter().map(String::as_str).collect();
    if set.len() != children.len() || !children.iter().all(|c| set.contains(c.as_str())) {
        return paths;
    }
    let contents = if parent.ends_with('/') {
        parent.to_owned()
    } else {
        format!("{parent}/")
    };
    vec![contents]
}

/// The parent folder of a device path. `None` for the root.
fn parent_of(path: &str) -> Option<&str> {
    match path.rsplit_once('/')? {
        (_, "") => None,
        ("", _) => Some("/"),
        (parent, _) => Some(parent),
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
mod tests;
