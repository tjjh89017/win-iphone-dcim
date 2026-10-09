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
mod tests {
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
}
