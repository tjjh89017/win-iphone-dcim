//! `cp`: copy device files and folders to a local path (SPEC.md section 5).

use std::collections::BTreeMap;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::time::Instant;

use crate::backup::planner::{CopyItem, PlanItem, PlanOptions, Planner};
use crate::backup::transfer::{leftover_parts, set_file_times, transfer};
use crate::device_fs::DeviceFs;
use crate::devpath::DevicePath;
use crate::error::{Error, Result, stdout_err};
use crate::model::{FailureKind, Node, SyncDecision, human_size, human_speed};
use crate::paths::normalize_local;
use crate::progress::{Progress, ProgressMode};

/// What to do when a file is already at the target path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnExists {
    /// Keep the file, log a warning, count it as skipped.
    #[default]
    SkipWarn,
    /// `-n`: keep the file and count it as skipped, with no warning.
    SkipQuiet,
    /// `-f`: replace the file through a `.part` file and an atomic rename.
    Overwrite,
}

#[derive(Debug, Clone, Copy)]
pub struct CpOptions {
    pub recursive: bool,
    /// `-p`: set the local file times from the device after the commit.
    pub preserve: bool,
    pub dry_run: bool,
    pub on_exists: OnExists,
    pub progress: ProgressMode,
}

impl Default for CpOptions {
    fn default() -> Self {
        Self {
            recursive: false,
            preserve: false,
            dry_run: false,
            on_exists: OnExists::SkipWarn,
            progress: ProgressMode::Off,
        }
    }
}

struct Failure {
    source: String,
    message: String,
}

#[derive(Default)]
struct Summary {
    copied: usize,
    skipped: usize,
    exists: usize,
    bytes: u64,
    failures: BTreeMap<FailureKind, Vec<Failure>>,
}

impl Summary {
    fn failed(&self) -> usize {
        self.failures.values().map(Vec::len).sum()
    }
}

struct Run<'a> {
    fs: &'a dyn DeviceFs,
    opts: CpOptions,
    out: &'a mut dyn Write,
    progress: Progress,
    summary: Summary,
}

/// Copy `sources` to `dest`. Return the number of failed items.
///
/// A device error that stops all later items (device gone, access denied)
/// ends the run after the summary and is returned as `Err`.
pub fn run(
    fs: &dyn DeviceFs,
    sources: &[DevicePath],
    dest: &Path,
    opts: CpOptions,
    out: &mut dyn Write,
) -> Result<usize> {
    // Absolute and, on Windows, verbatim (`\\?\D:\...`, `\\?\UNC\...`):
    // long paths and UNC shares work without the LongPathsEnabled setting.
    let dest = normalize_local(dest)?;
    let mut planner = Planner::new(
        fs,
        sources,
        &dest,
        PlanOptions {
            recursive: opts.recursive,
        },
    )?;
    let start = Instant::now();
    let mut run = Run {
        fs,
        opts,
        out,
        progress: Progress::new(if opts.dry_run {
            ProgressMode::Off
        } else {
            opts.progress
        }),
        summary: Summary::default(),
    };
    let mut fatal = None;
    while let Some(item) = planner.next() {
        let result = match item {
            PlanItem::Dir { source, target } => match run.enter_dir(&target) {
                Ok(()) => Ok(()),
                Err(e) => {
                    planner.skip_dir();
                    run.fail(&source, e)
                }
            },
            PlanItem::Copy(item) => run.copy(item),
            PlanItem::Error { source, error } => run.fail(&source, error),
        };
        if let Err(e) = result {
            fatal = Some(e);
            break;
        }
    }
    run.progress.scan_done();
    run.progress.finish();
    run.print_summary(start.elapsed())?;
    match fatal {
        Some(e) => Err(e),
        None => Ok(run.summary.failed()),
    }
}

impl Run<'_> {
    fn println(&mut self, line: String) -> Result<()> {
        let out = &mut *self.out;
        self.progress
            .suspend(|| writeln!(out, "{line}"))
            .map_err(stdout_err)
    }

    /// Record a per-item failure. Return the error if it is fatal.
    fn fail(&mut self, source: &str, error: Error) -> Result<()> {
        if error.is_fatal() {
            return Err(error);
        }
        if self.opts.dry_run {
            self.println(format!("[error] {source}  {error}"))?;
        } else {
            tracing::error!("{source}: {error}");
        }
        self.summary
            .failures
            .entry(error.kind())
            .or_default()
            .push(Failure {
                source: source.to_owned(),
                message: error.to_string(),
            });
        Ok(())
    }

    /// Create the local folder, or reuse it if it exists.
    fn enter_dir(&mut self, target: &Path) -> Result<()> {
        match std::fs::metadata(target) {
            Ok(m) if m.is_dir() => {
                for part in leftover_parts(target) {
                    tracing::warn!(
                        "leftover partial file {} is not a complete file; it is left in place",
                        part.display()
                    );
                }
                Ok(())
            }
            Ok(_) => Err(Error::NotAFolderLocal(target.to_path_buf())),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if self.opts.dry_run {
                    self.println(format!("[plan] mkdir {}", target.display()))
                } else {
                    std::fs::create_dir(target).map_err(|source| Error::Io {
                        context: format!("create folder {}", target.display()),
                        source,
                    })
                }
            }
            Err(source) => Err(Error::Io {
                context: format!("inspect {}", target.display()),
                source,
            }),
        }
    }

    fn decide(&self, target: &Path) -> Result<SyncDecision> {
        match std::fs::symlink_metadata(target) {
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(SyncDecision::Copy),
            Err(source) => Err(Error::Io {
                context: format!("inspect {}", target.display()),
                source,
            }),
            Ok(m) if m.is_dir() => Err(Error::TargetIsFolder(target.to_path_buf())),
            Ok(_) => Ok(match self.opts.on_exists {
                OnExists::SkipWarn => SyncDecision::SkipExists { warn: true },
                OnExists::SkipQuiet => SyncDecision::SkipExists { warn: false },
                OnExists::Overwrite => SyncDecision::Overwrite,
            }),
        }
    }

    fn skip_exists(&mut self, target: &Path, warn: bool, size: Option<u64>) {
        if warn {
            tracing::warn!(
                "[skip] {}  exists (use --force to overwrite)",
                target.display()
            );
        }
        self.summary.skipped += 1;
        self.summary.exists += 1;
        self.progress.settled(size);
    }

    fn copy(&mut self, item: CopyItem) -> Result<()> {
        let CopyItem {
            source,
            node,
            target,
        } = item;
        self.progress.found(node.size);
        let decision = match self.decide(&target) {
            Ok(d) => d,
            Err(e) => {
                self.progress.settled(node.size);
                return self.fail(&source, e);
            }
        };
        let replace = match decision {
            SyncDecision::SkipExists { warn } => {
                self.skip_exists(&target, warn, node.size);
                return Ok(());
            }
            SyncDecision::Overwrite => true,
            SyncDecision::Copy => false,
        };
        let size = node
            .size
            .map(human_size)
            .unwrap_or_else(|| "size unknown".into());
        if self.opts.dry_run {
            let note = if replace { ", overwrite" } else { "" };
            self.summary.copied += 1;
            return self.println(format!(
                "[plan] {source} -> {}  ({size}{note})",
                target.display()
            ));
        }
        self.progress.file_start(&source, node.size);
        let progress = &mut self.progress;
        let result = transfer(self.fs, &node, &target, replace, &mut |n| progress.bytes(n));
        self.progress.file_end(result.is_ok());
        let report = match result {
            Ok(r) => r,
            // A file appeared at the target after the check.
            Err(Error::OutputExists(_)) if self.opts.on_exists != OnExists::Overwrite => {
                self.summary.skipped += 1;
                self.summary.exists += 1;
                if self.opts.on_exists == OnExists::SkipWarn {
                    tracing::warn!(
                        "[skip] {}  exists (use --force to overwrite)",
                        target.display()
                    );
                }
                return Ok(());
            }
            Err(e) => return self.fail(&source, e),
        };
        if report.replaced {
            tracing::warn!("[overwrite] {}", target.display());
        }
        if self.opts.preserve {
            preserve_times(&target, &node);
        }
        self.summary.copied += 1;
        self.summary.bytes += report.bytes;
        self.println(format!(
            "[copy] {source} -> {}  {} / {size}  {}",
            target.display(),
            human_size(report.bytes),
            report.verification
        ))
    }

    fn print_summary(&mut self, elapsed: std::time::Duration) -> Result<()> {
        let s = &self.summary;
        let verb = if self.opts.dry_run {
            "planned"
        } else {
            "copied"
        };
        let mut line = format!(
            "[done] {verb}={} skipped={} exists={} failed={}",
            s.copied,
            s.skipped,
            s.exists,
            s.failed()
        );
        if !self.opts.dry_run {
            line.push_str(&format!(
                "  total={} elapsed={:.1}s avg={}",
                human_size(s.bytes),
                elapsed.as_secs_f64(),
                human_speed(s.bytes, elapsed)
            ));
        }
        let mut lines = vec![line];
        for (kind, list) in &s.failures {
            lines.push(format!("[failed] {kind}: {}", list.len()));
            for f in list {
                lines.push(format!("  {}: {}", f.source, f.message));
            }
        }
        for l in lines {
            self.println(l)?;
        }
        Ok(())
    }
}

/// `-p`: set the local times from the device. Timestamps are metadata only.
/// A failure is logged and does not fail the copy.
fn preserve_times(target: &Path, node: &Node) {
    if node.modified.is_none() {
        tracing::info!(
            "{}: the device gives no modified date; the copy time stays",
            target.display()
        );
    }
    if let Err(e) = set_file_times(target, node.modified, node.created) {
        tracing::warn!("{}: cannot set the file times: {e}", target.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::transfer::local_to_system_time;
    use crate::device_fs::fake::{FakeFs, dcim};
    use crate::model::LocalTime;

    fn paths(list: &[&str]) -> Vec<DevicePath> {
        list.iter().map(|p| DevicePath::parse(p).unwrap()).collect()
    }

    fn cp_fs(fs: &FakeFs, list: &[&str], dest: &Path, opts: CpOptions) -> Result<(String, usize)> {
        let mut out = Vec::new();
        let failures = run(fs, &paths(list), dest, opts, &mut out)?;
        Ok((String::from_utf8(out).unwrap(), failures))
    }

    fn cp(list: &[&str], dest: &Path, opts: CpOptions) -> Result<(String, usize)> {
        cp_fs(&dcim(), list, dest, opts)
    }

    fn rec() -> CpOptions {
        CpOptions {
            recursive: true,
            ..Default::default()
        }
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    const DCIM: &str = "/Internal Storage/DCIM";
    const A1: &str = "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC";
    const A2: &str = "/Internal Storage/DCIM/202601_a/IMG_0002.MOV";
    const B1: &str = "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC";

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
        assert!(
            out.contains("[done] copied=2 skipped=0 exists=0 failed=0  total="),
            "{out}"
        );
        assert!(out.contains("size-ok"), "{out}");
    }

    #[test]
    fn single_source_to_new_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("b.heic");
        cp(&[B1], &target, CpOptions::default()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"heic-b");
        assert_eq!(files_in(tmp.path()), ["b.heic"]);
    }

    #[test]
    fn recursive_copy_creates_named_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let (out, failures) = cp(&[DCIM], tmp.path(), rec()).unwrap();
        assert_eq!(failures, 0, "{out}");
        let d = tmp.path().join("DCIM");
        assert_eq!(
            std::fs::read(d.join("202601_a/IMG_0001.HEIC")).unwrap(),
            b"heic-a"
        );
        assert_eq!(
            std::fs::read(d.join("202601_b/IMG_0001.HEIC")).unwrap(),
            b"heic-b"
        );
        assert_eq!(
            d.join("202601_a/IMG_0002.MOV").metadata().unwrap().len(),
            2048
        );
        assert!(out.contains("copied=3 skipped=0"), "{out}");
    }

    #[test]
    fn trailing_slash_copies_contents() {
        let tmp = tempfile::tempdir().unwrap();
        cp(&["/Internal Storage/DCIM/"], tmp.path(), rec()).unwrap();
        assert_eq!(files_in(tmp.path()), ["202601_a", "202601_b"]);
        assert_eq!(
            std::fs::read(tmp.path().join("202601_b/IMG_0001.HEIC")).unwrap(),
            b"heic-b"
        );
    }

    #[test]
    fn missing_dest_gets_folder_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let new = tmp.path().join("New");
        cp(&["/Internal Storage/DCIM/202601_a"], &new, rec()).unwrap();
        assert_eq!(files_in(&new), ["IMG_0001.HEIC", "IMG_0002.MOV"]);
    }

    #[test]
    fn folder_source_needs_recursive() {
        let tmp = tempfile::tempdir().unwrap();
        let (out, failures) = cp(&[DCIM], tmp.path(), CpOptions::default()).unwrap();
        assert_eq!(failures, 1);
        assert!(out.contains("[failed] usage: 1"), "{out}");
        assert!(out.contains("use -r"), "{out}");
        assert!(files_in(tmp.path()).is_empty());
    }

    #[test]
    fn many_sources_need_folder_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let err = cp(&[A1, B1], &tmp.path().join("missing"), CpOptions::default()).unwrap_err();
        assert!(matches!(err, Error::DestNotFolder(_)));
    }

    #[test]
    fn existing_target_is_skipped_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"old").unwrap();
        let (out, failures) = cp(&[A1, A2], tmp.path(), CpOptions::default()).unwrap();
        assert_eq!(failures, 0);
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"old"
        );
        assert!(
            out.contains("copied=1 skipped=1 exists=1 failed=0"),
            "{out}"
        );
    }

    #[test]
    fn no_clobber_skips_silently() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"old").unwrap();
        let opts = CpOptions {
            on_exists: OnExists::SkipQuiet,
            ..Default::default()
        };
        let (out, failures) = cp(&[A1], tmp.path(), opts).unwrap();
        assert_eq!(failures, 0);
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"old"
        );
        assert!(out.contains("copied=0 skipped=1 exists=1"), "{out}");
    }

    #[test]
    fn force_replaces_with_new_content() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"old content").unwrap();
        let opts = CpOptions {
            on_exists: OnExists::Overwrite,
            ..Default::default()
        };
        let (out, failures) = cp(&[A1], tmp.path(), opts).unwrap();
        assert_eq!(failures, 0);
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"heic-a"
        );
        assert_eq!(files_in(tmp.path()), ["IMG_0001.HEIC"]);
        assert!(out.contains("copied=1 skipped=0 exists=0"), "{out}");
    }

    #[test]
    fn force_keeps_old_file_when_transfer_fails() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"old").unwrap();
        let mut fs = dcim();
        // Node 4 is 202601_a/IMG_0001.HEIC.
        fs.fail_read(4, 3, false);
        let opts = CpOptions {
            on_exists: OnExists::Overwrite,
            ..Default::default()
        };
        let (_, failures) = cp_fs(&fs, &[A1], tmp.path(), opts).unwrap();
        assert_eq!(failures, 1);
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"old"
        );
        assert_eq!(files_in(tmp.path()), ["IMG_0001.HEIC"]);
    }

    #[test]
    fn same_name_from_two_folders_skips_the_second() {
        let tmp = tempfile::tempdir().unwrap();
        let (out, failures) = cp(&[A1, B1], tmp.path(), CpOptions::default()).unwrap();
        assert_eq!(failures, 0);
        assert!(out.contains("exists=1"), "{out}");
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"heic-a"
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fs = dcim();
        let dcim_dir = 2;
        fs.file(dcim_dir, "CON", b"x");
        let opts = CpOptions {
            dry_run: true,
            ..rec()
        };
        let (out, failures) = cp_fs(&fs, &[DCIM], tmp.path(), opts).unwrap();
        assert_eq!(failures, 1);
        assert!(out.contains("[plan] mkdir "), "{out}");
        assert!(
            out.contains("[plan] /Internal Storage/DCIM/202601_a/IMG_0001.HEIC -> "),
            "{out}"
        );
        assert!(
            out.contains("[error] /Internal Storage/DCIM/CON  "),
            "{out}"
        );
        assert!(
            out.contains("[done] planned=3 skipped=0 exists=0 failed=1"),
            "{out}"
        );
        assert!(files_in(tmp.path()).is_empty());
    }

    #[test]
    fn size_mismatch_deletes_part_and_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fs = dcim();
        fs.node_mut(4).size = Some(7);
        let (out, failures) = cp_fs(&fs, &[A1], tmp.path(), CpOptions::default()).unwrap();
        assert_eq!(failures, 1);
        assert!(out.contains("[failed] size mismatch: 1"), "{out}");
        assert!(files_in(tmp.path()).is_empty());
    }

    #[test]
    fn read_failure_leaves_no_file_and_continues() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fs = dcim();
        fs.fail_read(5, 100, false);
        let (out, failures) = cp_fs(&fs, &[DCIM], tmp.path(), rec()).unwrap();
        assert_eq!(failures, 1);
        assert!(out.contains("[failed] device: 1"), "{out}");
        let a = tmp.path().join("DCIM/202601_a");
        assert_eq!(files_in(&a), ["IMG_0001.HEIC"]);
        assert!(tmp.path().join("DCIM/202601_b/IMG_0001.HEIC").exists());
    }

    #[test]
    fn fatal_error_stops_after_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fs = dcim();
        fs.fail_read(4, 2, true);
        let mut out = Vec::new();
        let err = run(&fs, &paths(&[DCIM]), tmp.path(), rec(), &mut out).unwrap_err();
        assert!(err.is_fatal());
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("[done] copied=0"), "{out}");
        assert_eq!(
            files_in(&tmp.path().join("DCIM/202601_a")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn leftover_part_is_not_treated_as_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("202601_a");
        std::fs::create_dir(&dir).unwrap();
        // A crash in an earlier run left this file.
        let leftover = dir.join("IMG_0001.HEIC.0123456789abcdef.part");
        std::fs::write(&leftover, b"hei").unwrap();
        let (out, failures) = cp(&["/Internal Storage/DCIM/202601_a"], tmp.path(), rec()).unwrap();
        assert_eq!(failures, 0);
        assert!(out.contains("copied=2"), "{out}");
        assert_eq!(std::fs::read(dir.join("IMG_0001.HEIC")).unwrap(), b"heic-a");
        assert_eq!(std::fs::read(&leftover).unwrap(), b"hei");
    }

    #[test]
    fn case_collision_copies_neither() {
        let mut fs = FakeFs::new();
        let d = fs.folder(0, "DCIM");
        fs.file(d, "IMG_0001.HEIC", b"1");
        fs.file(d, "img_0001.heic", b"2");
        fs.file(d, "IMG_0002.HEIC", b"3");
        let tmp = tempfile::tempdir().unwrap();
        let (out, failures) = cp_fs(&fs, &["/DCIM/"], tmp.path(), rec()).unwrap();
        assert_eq!(failures, 2);
        assert!(out.contains("[failed] collision: 2"), "{out}");
        assert_eq!(files_in(tmp.path()), ["IMG_0002.HEIC"]);
    }

    #[test]
    fn cjk_and_emoji_names_round_trip() {
        let mut fs = FakeFs::new();
        let d = fs.folder(0, "旅行 🗾");
        fs.file(d, "照片🎉.HEIC", b"x");
        let tmp = tempfile::tempdir().unwrap();
        cp_fs(&fs, &["/旅行 🗾"], tmp.path(), rec()).unwrap();
        assert_eq!(files_in(tmp.path()), ["旅行 🗾"]);
        assert_eq!(files_in(&tmp.path().join("旅行 🗾")), ["照片🎉.HEIC"]);
    }

    #[test]
    fn long_target_path_is_copied() {
        let mut fs = FakeFs::new();
        let mut parent = 0;
        for i in 0..4 {
            parent = fs.folder(parent, &format!("{i}{}", "x".repeat(90)));
        }
        fs.file(parent, &format!("{}.MOV", "y".repeat(80)), b"long");
        let tmp = tempfile::tempdir().unwrap();
        let (out, failures) = cp_fs(&fs, &["/"], tmp.path(), rec()).unwrap();
        assert_eq!(failures, 0, "{out}");
        let mut path = tmp.path().to_path_buf();
        for i in 0..4 {
            path.push(format!("{i}{}", "x".repeat(90)));
        }
        path.push(format!("{}.MOV", "y".repeat(80)));
        assert!(path.as_os_str().len() > 300);
        assert_eq!(
            std::fs::read(crate::paths::to_verbatim(&path)).unwrap(),
            b"long"
        );
    }

    #[test]
    fn archive_sets_modified_time() {
        let mut fs = dcim();
        let t = LocalTime::parse("2023-03-04 05:06:07");
        fs.node_mut(4).modified = Some(t);
        fs.node_mut(4).created = Some(t);
        let tmp = tempfile::tempdir().unwrap();
        let opts = CpOptions {
            preserve: true,
            ..rec()
        };
        cp_fs(&fs, &["/Internal Storage/DCIM/202601_a/"], tmp.path(), opts).unwrap();
        let m = std::fs::metadata(tmp.path().join("IMG_0001.HEIC"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(m, local_to_system_time(t).unwrap());
        // No device date: the copy time stays.
        let m2 = std::fs::metadata(tmp.path().join("IMG_0002.MOV"))
            .unwrap()
            .modified()
            .unwrap();
        assert!(m2 > local_to_system_time(t).unwrap());
    }

    #[test]
    fn existing_dest_folder_as_file_fails_the_subtree() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("DCIM"), b"file").unwrap();
        let (out, failures) = cp(&[DCIM], tmp.path(), rec()).unwrap();
        assert_eq!(failures, 1, "{out}");
        assert!(out.contains("[failed] target exists: 1"), "{out}");
    }
}
