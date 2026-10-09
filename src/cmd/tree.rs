//! `tree`: branch view like the Unix `tree` command.

use std::io::Write;

use super::report;
use super::sort::{SortKey, folders_first, sort_entries};
use crate::device_fs::{DeviceFs, resolve};
use crate::devpath::DevicePath;
use crate::error::{Result, stdout_err};
use crate::model::{Node, TreeRecord, human_size, join_device_path};

#[derive(Default)]
struct Counts {
    folders: usize,
    files: usize,
    failures: usize,
}

/// Return the number of folders that could not be listed.
pub fn run(
    fs: &dyn DeviceFs,
    path: &DevicePath,
    max_depth: Option<usize>,
    json: bool,
    dirs_first: bool,
    out: &mut dyn Write,
) -> Result<usize> {
    let root = resolve(fs, path)?;
    let root_path = path.normalized();
    let mut counts = Counts::default();
    let mut walker = Walker {
        fs,
        max_depth,
        json,
        dirs_first,
        out,
        counts: &mut counts,
    };
    if json {
        walker.record(0, &root_path, &root)?;
    } else if root.is_folder {
        writeln!(walker.out, "{root_path}").map_err(stdout_err)?;
    } else {
        writeln!(walker.out, "{}", label(&root_path, &root)).map_err(stdout_err)?;
    }
    if root.is_folder {
        walker.children(&root, &root_path, "", 1)?;
    }
    if !json {
        writeln!(
            out,
            "\n{} {}, {} {}",
            counts.folders,
            if counts.folders == 1 {
                "directory"
            } else {
                "directories"
            },
            counts.files,
            if counts.files == 1 { "file" } else { "files" },
        )
        .map_err(stdout_err)?;
    }
    Ok(counts.failures)
}

fn label(name: &str, node: &Node) -> String {
    match (node.is_folder, node.size) {
        (false, Some(size)) => format!("{name}  ({})", human_size(size)),
        _ => name.to_owned(),
    }
}

struct Walker<'a> {
    fs: &'a dyn DeviceFs,
    max_depth: Option<usize>,
    json: bool,
    dirs_first: bool,
    out: &'a mut dyn Write,
    counts: &'a mut Counts,
}

impl Walker<'_> {
    fn record(&mut self, depth: usize, path: &str, node: &Node) -> Result<()> {
        let rec = TreeRecord {
            depth,
            path: path.to_owned(),
            name: node.display_name(),
            is_folder: node.is_folder,
            size: node.size,
            object_id: node.id.display(),
        };
        let line = serde_json::to_string(&rec).expect("TreeRecord serializes");
        writeln!(self.out, "{line}").map_err(stdout_err)
    }

    /// Print the children of `dir`, which sit at `depth`.
    fn children(&mut self, dir: &Node, dir_path: &str, prefix: &str, depth: usize) -> Result<()> {
        if self.max_depth.is_some_and(|m| depth > m) {
            return Ok(());
        }
        let mut children = match self.fs.list(dir) {
            Ok(c) => c,
            Err(e) => {
                report(e)?;
                self.counts.failures += 1;
                return Ok(());
            }
        };
        sort_entries(&mut children, SortKey::Name, false);
        if self.dirs_first {
            folders_first(&mut children);
        }
        let last_index = children.len().saturating_sub(1);
        for (i, child) in children.iter().enumerate() {
            let name = child.display_name();
            let path = join_device_path(dir_path, &name);
            if child.is_folder {
                self.counts.folders += 1;
            } else {
                self.counts.files += 1;
            }
            let last = i == last_index;
            if self.json {
                self.record(depth, &path, child)?;
            } else {
                let branch = if last { "└── " } else { "├── " };
                writeln!(self.out, "{prefix}{branch}{}", label(&name, child))
                    .map_err(stdout_err)?;
            }
            if child.is_folder {
                let next = format!("{prefix}{}", if last { "    " } else { "│   " });
                self.children(child, &path, &next, depth + 1)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
