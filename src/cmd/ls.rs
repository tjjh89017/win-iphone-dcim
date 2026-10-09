//! `ls`: list one folder level, like Unix `ls`.

use std::io::Write;

use super::report;
use super::sort::{SortKey, sort_entries};
use crate::device_fs::{DeviceFs, resolve};
use crate::devpath::DevicePath;
use crate::error::{Result, stdout_err};
use crate::model::{LsRecord, Node, join_device_path};

#[derive(Debug, Clone, Copy, Default)]
pub struct LsOptions {
    pub long: bool,
    pub recursive: bool,
    pub json: bool,
    pub sort: SortKey,
    pub reverse: bool,
}

/// Return the number of paths that failed.
pub fn run(
    fs: &dyn DeviceFs,
    paths: &[DevicePath],
    opts: LsOptions,
    out: &mut dyn Write,
) -> Result<usize> {
    let root = [DevicePath::root()];
    let paths = if paths.is_empty() { &root[..] } else { paths };
    let headers = !opts.json && (paths.len() > 1 || opts.recursive);
    let mut failures = 0;
    let mut first = true;
    for path in paths {
        let node = match resolve(fs, path) {
            Ok(n) => n,
            Err(e) => {
                report(e)?;
                failures += 1;
                continue;
            }
        };
        let display = path.normalized();
        if node.is_folder {
            failures += list_dir(fs, &display, &node, opts, headers, &mut first, out)?;
        } else {
            entry(&display, &node.display_name(), &node, opts, out)?;
        }
    }
    Ok(failures)
}

fn list_dir(
    fs: &dyn DeviceFs,
    dir_path: &str,
    dir: &Node,
    opts: LsOptions,
    headers: bool,
    first: &mut bool,
    out: &mut dyn Write,
) -> Result<usize> {
    if headers {
        if !*first {
            writeln!(out).map_err(stdout_err)?;
        }
        writeln!(out, "{dir_path}:").map_err(stdout_err)?;
    }
    *first = false;
    let mut children = match fs.list(dir) {
        Ok(c) => c,
        Err(e) => {
            report(e)?;
            return Ok(1);
        }
    };
    sort_entries(&mut children, opts.sort, opts.reverse);
    for child in &children {
        let name = child.display_name();
        entry(&join_device_path(dir_path, &name), &name, child, opts, out)?;
    }
    let mut failures = 0;
    if opts.recursive {
        for child in children.iter().filter(|c| c.is_folder) {
            let path = join_device_path(dir_path, &child.display_name());
            failures += list_dir(fs, &path, child, opts, headers, first, out)?;
        }
    }
    Ok(failures)
}

fn entry(path: &str, name: &str, node: &Node, opts: LsOptions, out: &mut dyn Write) -> Result<()> {
    if opts.json {
        let line = serde_json::to_string(&LsRecord::new(path.to_owned(), node))
            .expect("LsRecord serializes");
        writeln!(out, "{line}")
    } else if opts.long {
        let size = match (node.is_folder, node.size) {
            (false, Some(s)) => s.to_string(),
            _ => "-".to_owned(),
        };
        writeln!(
            out,
            "{} {:>12}  {:<19}  {}  [id: {}]",
            if node.is_folder { 'd' } else { '-' },
            size,
            node.modified
                .map(|t| t.to_string())
                .unwrap_or_else(|| "-".into()),
            name,
            node.id.display()
        )
    } else {
        writeln!(out, "{name}")
    }
    .map_err(stdout_err)
}

#[cfg(test)]
mod tests;
