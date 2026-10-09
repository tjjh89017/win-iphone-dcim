//! `ls`: list one folder level, like Unix `ls`.

use std::io::Write;

use super::report;
use crate::device_fs::{DeviceFs, resolve};
use crate::devpath::DevicePath;
use crate::error::{Result, stdout_err};
use crate::model::{LsRecord, Node, join_device_path};

#[derive(Debug, Clone, Copy, Default)]
pub struct LsOptions {
    pub long: bool,
    pub recursive: bool,
    pub json: bool,
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
    let children = match fs.list(dir) {
        Ok(c) => c,
        Err(e) => {
            report(e)?;
            return Ok(1);
        }
    };
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
            node.modified.as_deref().unwrap_or("-"),
            name,
            node.id.display()
        )
    } else {
        writeln!(out, "{name}")
    }
    .map_err(stdout_err)
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn missing_path_counts_as_failure() {
        let (out, failures) = ls(&["/nope", "/Internal Storage"], LsOptions::default());
        assert_eq!(failures, 1);
        assert!(out.contains("DCIM"));
    }
}
