//! `cp`: copy device files to a local path. Phase 0 copies single files only.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use super::report;
use crate::device_fs::{DeviceFs, resolve};
use crate::devpath::DevicePath;
use crate::error::{Error, Result, stdout_err};
use crate::model::{Node, human_size};
use crate::paths::{device_name_to_os, normalize_local};

#[derive(Debug, Clone, Copy, Default)]
pub struct CpOptions {
    /// Parsed but not implemented in Phase 0.
    pub recursive: bool,
    pub dry_run: bool,
}

/// Where the copies go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest {
    /// DEST is an existing folder. Each file goes into it with its device name.
    IntoFolder(PathBuf),
    /// DEST does not name a folder and there is one source. DEST is the file path.
    File(PathBuf),
}

impl Dest {
    /// Apply the DEST rules. With two or more sources DEST must be an existing folder.
    pub fn new(dest: &Path, source_count: usize) -> Result<Self> {
        if dest.is_dir() {
            Ok(Self::IntoFolder(dest.to_path_buf()))
        } else if source_count > 1 {
            Err(Error::DestNotFolder(dest.to_path_buf()))
        } else {
            Ok(Self::File(dest.to_path_buf()))
        }
    }

    /// The local path for `node`. In a folder the file name comes from the
    /// raw UTF-16 device name, so no lossy conversion happens.
    pub fn target(&self, node: &Node) -> Result<PathBuf> {
        match self {
            Self::IntoFolder(dir) => {
                let units = node
                    .raw_file_name
                    .as_deref()
                    .ok_or_else(|| Error::UnsafeFileName(String::new()))?;
                Ok(dir.join(device_name_to_os(units)?))
            }
            Self::File(path) => Ok(path.clone()),
        }
    }
}

/// Return the number of sources that failed.
pub fn run(
    fs: &dyn DeviceFs,
    sources: &[DevicePath],
    dest: &Path,
    opts: CpOptions,
    out: &mut dyn Write,
) -> Result<usize> {
    // Absolute and, on Windows, verbatim (`\\?\D:\...`, `\\?\UNC\...`):
    // long paths and UNC shares work without the LongPathsEnabled setting.
    let dest = Dest::new(&normalize_local(dest)?, sources.len())?;
    if opts.recursive {
        tracing::warn!("-r is not implemented yet; folders are refused");
    }
    let mut done = 0usize;
    let mut failures = 0usize;
    for src in sources {
        match copy_one(fs, src, &dest, opts.dry_run, out) {
            Ok(()) => done += 1,
            Err(e) => {
                report(e)?;
                failures += 1;
            }
        }
    }
    let verb = if opts.dry_run { "planned" } else { "copied" };
    writeln!(out, "[done] {verb}={done} failed={failures}").map_err(stdout_err)?;
    Ok(failures)
}

fn copy_one(
    fs: &dyn DeviceFs,
    src: &DevicePath,
    dest: &Dest,
    dry_run: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let node = resolve(fs, src)?;
    if node.is_folder {
        return Err(Error::FolderNeedsRecursive(src.to_string()));
    }
    let target = dest.target(&node)?;
    let size = node
        .size
        .map(human_size)
        .unwrap_or_else(|| "size unknown".into());
    if dry_run {
        if target.exists() {
            return Err(Error::OutputExists(target));
        }
        writeln!(out, "[plan] {src} -> {}  ({size})", target.display()).map_err(stdout_err)?;
        return Ok(());
    }
    let written = stream_to_new_file(fs, &node, &target)?;
    let check = match node.size {
        Some(_) => "size ok",
        None => "size unavailable",
    };
    writeln!(
        out,
        "[copy] {src} -> {}  {written} bytes ({}), {check}",
        target.display(),
        human_size(written)
    )
    .map_err(stdout_err)?;
    Ok(())
}

/// Create `target` with create_new, stream the file, and check the size.
/// On any failure the new file is removed, so no partial file remains.
///
/// create_new guarantees that no existing file is overwritten. A later
/// `.part` + commit step must not use `std::fs::rename` for a no-clobber
/// commit on Windows: it replaces an existing target file.
fn stream_to_new_file(fs: &dyn DeviceFs, node: &Node, target: &Path) -> Result<u64> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|e| match e.kind() {
            ErrorKind::AlreadyExists => Error::OutputExists(target.to_path_buf()),
            _ => Error::Io {
                context: format!("create {}", target.display()),
                source: e,
            },
        })?;
    let result = write_and_check(fs, node, &mut file, target);
    drop(file);
    if result.is_err()
        && let Err(e) = std::fs::remove_file(target)
    {
        tracing::warn!("cannot remove partial file {}: {e}", target.display());
    }
    result
}

fn write_and_check(fs: &dyn DeviceFs, node: &Node, file: &mut File, target: &Path) -> Result<u64> {
    let written = fs.read_to(node, file)?;
    file.sync_all().map_err(|e| Error::Io {
        context: format!("flush {}", target.display()),
        source: e,
    })?;
    match node.size {
        Some(expected) if expected != written => Err(Error::SizeMismatch {
            path: target.to_path_buf(),
            written,
            expected,
        }),
        _ => Ok(written),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_fs::fake::dcim;

    fn paths(list: &[&str]) -> Vec<DevicePath> {
        list.iter().map(|p| DevicePath::parse(p).unwrap()).collect()
    }

    fn cp(list: &[&str], dest: &Path, opts: CpOptions) -> Result<(String, usize)> {
        let fs = dcim();
        let mut out = Vec::new();
        let failures = run(&fs, &paths(list), dest, opts, &mut out)?;
        Ok((String::from_utf8(out).unwrap(), failures))
    }

    const A1: &str = "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC";
    const A2: &str = "/Internal Storage/DCIM/202601_a/IMG_0002.MOV";
    const B1: &str = "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC";

    #[test]
    fn dest_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(
            Dest::new(dir, 3).unwrap(),
            Dest::IntoFolder(dir.to_path_buf())
        );
        let missing = dir.join("new.heic");
        assert_eq!(Dest::new(&missing, 1).unwrap(), Dest::File(missing.clone()));
        assert!(matches!(
            Dest::new(&missing, 2),
            Err(Error::DestNotFolder(_))
        ));
        std::fs::write(&missing, b"x").unwrap();
        assert!(matches!(
            Dest::new(&missing, 2),
            Err(Error::DestNotFolder(_))
        ));
        let fs = dcim();
        let node = resolve(&fs, &paths(&[A1])[0]).unwrap();
        assert_eq!(
            Dest::new(dir, 1).unwrap().target(&node).unwrap(),
            dir.join("IMG_0001.HEIC")
        );
        assert_eq!(
            Dest::new(&missing, 1).unwrap().target(&node).unwrap(),
            missing
        );
    }

    #[test]
    fn invalid_utf16_device_name_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fs = dcim();
        // Node 4 is 202601_a/IMG_0001.HEIC.
        let node = fs.node_mut(4);
        node.raw_file_name = Some(vec![0x0049, 0xDC00]);
        node.original_file_name = None;
        node.name = Some("IMG_0001.HEIC".into());
        let mut out = Vec::new();
        let failures = run(
            &fs,
            &paths(&[A1]),
            tmp.path(),
            CpOptions::default(),
            &mut out,
        )
        .unwrap();
        assert_eq!(failures, 1);
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn relative_dest_is_resolved() {
        // `normalize_local` is covered in paths.rs; here DEST "." must be a folder.
        let dest = Dest::new(&normalize_local(Path::new(".")).unwrap(), 2).unwrap();
        assert!(matches!(dest, Dest::IntoFolder(p) if p.is_absolute()));
    }

    #[test]
    fn copies_into_folder_with_device_name() {
        let tmp = tempfile::tempdir().unwrap();
        let (out, failures) = cp(&[A1, A2], tmp.path(), CpOptions::default()).unwrap();
        assert_eq!(failures, 0);
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"heic-a"
        );
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0002.MOV"))
                .unwrap()
                .len(),
            2048
        );
        assert!(out.ends_with("[done] copied=2 failed=0\n"), "{out}");
    }

    #[test]
    fn single_source_to_new_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("b.heic");
        cp(&[B1], &target, CpOptions::default()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"heic-b");
    }

    #[test]
    fn never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"old").unwrap();
        let (_, failures) = cp(&[A1], tmp.path(), CpOptions::default()).unwrap();
        assert_eq!(failures, 1);
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"old"
        );
    }

    #[test]
    fn same_name_from_two_folders_fails_the_second() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, failures) = cp(&[A1, B1], tmp.path(), CpOptions::default()).unwrap();
        assert_eq!(failures, 1);
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"heic-a"
        );
    }

    #[test]
    fn folder_source_needs_recursive() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = CpOptions {
            recursive: true,
            ..Default::default()
        };
        let (_, failures) = cp(&["/Internal Storage/DCIM/202601_a/"], tmp.path(), opts).unwrap();
        assert_eq!(failures, 1);
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = CpOptions {
            dry_run: true,
            ..Default::default()
        };
        let (out, failures) = cp(&[A1], tmp.path(), opts).unwrap();
        assert_eq!(failures, 0);
        assert!(out.starts_with("[plan] "), "{out}");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn size_mismatch_removes_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fs = dcim();
        // Node 4 is 202601_a/IMG_0001.HEIC (6 bytes).
        fs.node_mut(4).size = Some(7);
        let mut out = Vec::new();
        let failures = run(
            &fs,
            &paths(&[A1]),
            tmp.path(),
            CpOptions::default(),
            &mut out,
        )
        .unwrap();
        assert_eq!(failures, 1);
        assert!(!tmp.path().join("IMG_0001.HEIC").exists());
    }

    #[test]
    fn many_sources_need_folder_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let err = cp(&[A1, B1], &tmp.path().join("missing"), CpOptions::default()).unwrap_err();
        assert!(matches!(err, Error::DestNotFolder(_)));
    }
}
