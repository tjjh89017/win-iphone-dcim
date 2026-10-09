//! `cp`: copy device files and folders to a local path (SPEC.md section 5).
//!
//! Every copy writes the JSONL manifest of the copy root and applies the
//! incremental rules of SPEC.md section 7. A transient failure is retried
//! with backoff (section 8).

use std::collections::BTreeMap;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::backup::manifest::{
    Manifest, Record, device_key, device_log_key, hash_file, relative_path, to_hex,
};
use crate::backup::planner::{CopyItem, PlanItem, PlanOptions, Planner};
use crate::backup::transfer::{leftover_parts, set_file_times, transfer};
use crate::device_fs::DeviceFs;
use crate::devpath::DevicePath;
use crate::error::{Error, Result, stdout_err};
use crate::model::{
    ExistingState, FailureKind, Node, SyncDecision, TransferReport, human_size, human_speed,
};
use crate::paths::normalize_local;
use crate::progress::{Progress, ProgressMode};

/// What to do when a file is already at the target path and it is not verified.
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

/// Default number of additional attempts after a transient failure.
pub const DEFAULT_RETRIES: u32 = 3;

/// Wait before retry `attempt` (1-based): 1 s, 3 s, 10 s, then 10 s.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(match attempt {
        0 | 1 => 1,
        2 => 3,
        _ => 10,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct CpOptions {
    pub recursive: bool,
    /// `-p`: set the local file times from the device after the commit.
    pub preserve: bool,
    pub dry_run: bool,
    pub on_exists: OnExists,
    pub progress: ProgressMode,
    /// `--verify local-hash`: hash new copies while they stream, and check
    /// the stored hash before a skip.
    pub local_hash: bool,
    /// Additional attempts after a transient failure.
    pub retries: u32,
    /// Waits between attempts. Tests replace it.
    pub sleep: fn(Duration),
    /// `--diagnostic`: log the raw device ID.
    pub diagnostic: bool,
}

impl Default for CpOptions {
    fn default() -> Self {
        Self {
            recursive: false,
            preserve: false,
            dry_run: false,
            on_exists: OnExists::SkipWarn,
            progress: ProgressMode::Off,
            local_hash: false,
            retries: DEFAULT_RETRIES,
            sleep: std::thread::sleep,
            diagnostic: false,
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
    /// Device key for the manifest (hash of the raw device ID).
    device: Option<String>,
    /// The manifest of the copy root. Loaded when the root is known.
    manifest: Option<Manifest>,
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
    let device = fs.device_id().map(|raw| {
        let key = device_key(&raw);
        if opts.diagnostic {
            tracing::info!("device {}  raw id {raw}", device_log_key(&key));
        } else {
            tracing::info!("device {}", device_log_key(&key));
        }
        key
    });
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
        device,
        manifest: None,
    };
    if dest.is_dir() {
        run.open_manifest(&dest)?;
    }
    let mut fatal = None;
    while let Some(item) = planner.next() {
        if run.manifest.is_none() {
            // DEST did not exist: it is the new folder, or the new file name
            // of a single file SRC.
            match &item {
                PlanItem::Dir { .. } => run.open_manifest(&dest)?,
                PlanItem::Copy(_) => run.open_manifest(dest.parent().unwrap_or(&dest))?,
                PlanItem::Error { .. } => {}
            }
        }
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

    /// Load the manifest of the copy root and compare it with the local files.
    fn open_manifest(&mut self, root: &Path) -> Result<()> {
        let mut manifest = Manifest::load(root)?;
        let counts = manifest.reconcile();
        if !manifest.is_empty() {
            tracing::info!(
                "manifest {}: {} record(s), {} stale",
                Manifest::path_for(root).display(),
                manifest.len(),
                counts.stale
            );
        }
        self.manifest = Some(manifest);
        Ok(())
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

    /// The manifest path of `target`, if it is below the copy root.
    fn relative(&self, target: &Path) -> Option<String> {
        relative_path(self.manifest.as_ref()?.root(), target)
    }

    /// Apply the incremental rules of SPEC.md section 7 to one file.
    fn decide(&self, source: &str, node: &Node, target: &Path) -> Result<SyncDecision> {
        let meta = match std::fs::symlink_metadata(target) {
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(SyncDecision::Copy),
            Err(source) => {
                return Err(Error::Io {
                    context: format!("inspect {}", target.display()),
                    source,
                });
            }
            Ok(m) if m.is_dir() => return Err(Error::TargetIsFolder(target.to_path_buf())),
            Ok(m) => m,
        };
        let local = meta.len();
        let entry = self
            .relative(target)
            .and_then(|rel| self.manifest.as_ref()?.get(&rel));
        let state = match (entry, node.size) {
            (Some(e), _) if e.stale.is_some() => ExistingState::Conflict(format!(
                "manifest record is stale ({})",
                e.stale.as_deref().unwrap_or_default()
            )),
            (Some(e), _) if differ(&e.record.device, &self.device) => {
                ExistingState::Conflict("manifest record is from another device".into())
            }
            (Some(e), _) if e.record.source.as_deref().is_some_and(|s| s != source) => {
                ExistingState::Conflict(format!(
                    "manifest record is for another source ({})",
                    e.record.source.as_deref().unwrap_or_default()
                ))
            }
            (Some(_), None) => ExistingState::SizeUnavailable,
            (_, Some(size)) if size != local => {
                ExistingState::Conflict(format!("local size {local}, device size {size}"))
            }
            (Some(e), Some(_)) => match e.record.blake3().filter(|_| self.opts.local_hash) {
                None => return Ok(SyncDecision::SkipVerified),
                Some(stored) => {
                    let actual = hash_file(target).map_err(|source| Error::Io {
                        context: format!("hash {}", target.display()),
                        source,
                    })?;
                    if to_hex(&actual).eq_ignore_ascii_case(stored) {
                        return Ok(SyncDecision::SkipVerified);
                    }
                    ExistingState::Conflict("local hash differs from the manifest".into())
                }
            },
            (None, Some(_)) => ExistingState::UnverifiedExisting,
            (None, None) => {
                ExistingState::Conflict("no manifest record and the device gives no size".into())
            }
        };
        Ok(match self.opts.on_exists {
            OnExists::SkipWarn => SyncDecision::SkipExists { state, warn: true },
            OnExists::SkipQuiet => SyncDecision::SkipExists { state, warn: false },
            OnExists::Overwrite => SyncDecision::Overwrite { state },
        })
    }

    fn skip_exists(&mut self, target: &Path, state: &ExistingState, warn: bool) -> Result<()> {
        self.summary.skipped += 1;
        self.summary.exists += 1;
        let line = format!(
            "[skip] {}  {state} (use --force to replace)",
            target.display()
        );
        if !warn {
            Ok(())
        } else if self.opts.dry_run {
            self.println(line)
        } else {
            tracing::warn!("{line}");
            Ok(())
        }
    }

    fn copy(&mut self, item: CopyItem) -> Result<()> {
        let CopyItem {
            source,
            node,
            target,
        } = item;
        self.progress.found(node.size);
        let decision = match self.decide(&source, &node, &target) {
            Ok(d) => d,
            Err(e) => {
                self.progress.settled(node.size);
                return self.fail(&source, e);
            }
        };
        let overwrite = match decision {
            SyncDecision::SkipVerified => {
                self.summary.skipped += 1;
                self.progress.settled(node.size);
                return self.println(format!("[skip] {source}  verified"));
            }
            SyncDecision::SkipExists { state, warn } => {
                self.progress.settled(node.size);
                return self.skip_exists(&target, &state, warn);
            }
            SyncDecision::Overwrite { state } => Some(state),
            SyncDecision::Copy => None,
        };
        let size = node
            .size
            .map(human_size)
            .unwrap_or_else(|| "size unknown".into());
        if self.opts.dry_run {
            let note = match &overwrite {
                Some(state) => format!(", overwrite: {state}"),
                None => String::new(),
            };
            self.summary.copied += 1;
            return self.println(format!(
                "[plan] {source} -> {}  ({size}{note})",
                target.display()
            ));
        }
        let replace = overwrite.is_some();
        let report = match self.transfer_with_retries(&source, &node, &target, replace)? {
            Ok(r) => r,
            // A file appeared at the target after the check.
            Err(Error::OutputExists(_)) if self.opts.on_exists != OnExists::Overwrite => {
                self.summary.skipped += 1;
                self.summary.exists += 1;
                if self.opts.on_exists == OnExists::SkipWarn {
                    tracing::warn!(
                        "[skip] {}  exists (use --force to replace)",
                        target.display()
                    );
                }
                return Ok(());
            }
            Err(e) => return self.fail(&source, e),
        };
        if let Some(state) = &overwrite {
            tracing::warn!("[overwrite] {}  {state}", target.display());
        }
        if self.opts.preserve {
            preserve_times(&target, &node);
        }
        if let Err(e) = self.record(&source, &node, &target, &report) {
            return self.fail(&source, e);
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

    /// Transfer one file. Retry a transient failure up to `retries` times
    /// with backoff. A failed attempt removes its `.part` file before the
    /// next one. The outer `Err` is a failed write to stdout.
    fn transfer_with_retries(
        &mut self,
        source: &str,
        node: &Node,
        target: &Path,
        replace: bool,
    ) -> Result<Result<TransferReport>> {
        self.progress.file_start(source, node.size);
        let mut attempt = 0;
        let result = loop {
            let progress = &mut self.progress;
            let result = transfer(
                self.fs,
                node,
                target,
                replace,
                self.opts.local_hash,
                &mut |n| progress.bytes(n),
            );
            match result {
                Err(e) if e.is_transient() && attempt < self.opts.retries => {
                    attempt += 1;
                    self.println(format!(
                        "[retry {attempt}/{}] {source}  {e}",
                        self.opts.retries
                    ))?;
                    (self.opts.sleep)(backoff(attempt));
                    self.progress.restart_file();
                }
                other => break other,
            }
        };
        self.progress.file_end(result.is_ok());
        Ok(result)
    }

    /// Append the manifest record of a committed file.
    fn record(
        &mut self,
        source: &str,
        node: &Node,
        target: &Path,
        report: &TransferReport,
    ) -> Result<()> {
        let Some(rel) = self.relative(target) else {
            tracing::warn!(
                "{}: not below the copy root; no manifest record",
                target.display()
            );
            return Ok(());
        };
        let record = Record::committed(
            self.device.clone(),
            rel,
            source.to_owned(),
            report.bytes,
            node.modified,
            node.created,
            report.verification,
            report.hash,
        );
        match self.manifest.as_mut() {
            Some(m) => m.append(record),
            None => Ok(()),
        }
    }

    fn print_summary(&mut self, elapsed: Duration) -> Result<()> {
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

/// True if both device keys are known and they differ.
fn differ(a: &Option<String>, b: &Option<String>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a != b)
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

    fn opts() -> CpOptions {
        CpOptions {
            sleep: |_| {},
            ..CpOptions::default()
        }
    }

    fn rec() -> CpOptions {
        CpOptions {
            recursive: true,
            ..opts()
        }
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != crate::backup::manifest::DIR_NAME)
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
        let (out, failures) = cp(&[A1, A2], tmp.path(), opts()).unwrap();
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
        cp(&[B1], &target, opts()).unwrap();
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
        let (out, failures) = cp(&[DCIM], tmp.path(), opts()).unwrap();
        assert_eq!(failures, 1);
        assert!(out.contains("[failed] usage: 1"), "{out}");
        assert!(out.contains("use -r"), "{out}");
        assert!(files_in(tmp.path()).is_empty());
    }

    #[test]
    fn many_sources_need_folder_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let err = cp(&[A1, B1], &tmp.path().join("missing"), opts()).unwrap_err();
        assert!(matches!(err, Error::DestNotFolder(_)));
    }

    #[test]
    fn existing_target_is_skipped_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"old").unwrap();
        let (out, failures) = cp(&[A1, A2], tmp.path(), opts()).unwrap();
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
            ..opts()
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
            ..opts()
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
            ..opts()
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
        let (out, failures) = cp(&[A1, B1], tmp.path(), opts()).unwrap();
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
        let (out, failures) = cp_fs(&fs, &[A1], tmp.path(), opts()).unwrap();
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

    // Phase 2: manifest, incremental rules, retries.

    use crate::backup::manifest::{Manifest, device_key};
    use std::cell::{Cell, RefCell};

    const RAW_ID: &str = r"\\?\usb#vid_05ac&pid_12a8&mi_00#00008030001a2b3c4d5e6f70#{6ac27878-a6fa-4155-ba85-f98f491d4f33}";

    /// A fake device that fails the first `fail_first` reads with `error`.
    struct Flaky {
        inner: FakeFs,
        reads: Cell<usize>,
        fail_first: usize,
        error: fn() -> Error,
        id: Option<String>,
    }

    impl Flaky {
        fn new(fail_first: usize, error: fn() -> Error) -> Self {
            Self {
                inner: dcim(),
                reads: Cell::new(0),
                fail_first,
                error,
                id: Some(RAW_ID.into()),
            }
        }
    }

    impl DeviceFs for Flaky {
        fn root(&self) -> Node {
            self.inner.root()
        }

        fn list(&self, dir: &Node) -> Result<Vec<Node>> {
            self.inner.list(dir)
        }

        fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64> {
            let n = self.reads.get();
            self.reads.set(n + 1);
            if n < self.fail_first {
                out.write_all(b"pa").unwrap();
                return Err((self.error)());
            }
            self.inner.read_to(file, out)
        }

        fn device_id(&self) -> Option<String> {
            self.id.clone()
        }
    }

    fn worker_restarted() -> Error {
        Error::WorkerRestarted {
            context: "read".into(),
            reason: "watchdog timeout".into(),
        }
    }

    fn cp_dyn(fs: &dyn DeviceFs, list: &[&str], dest: &Path, opts: CpOptions) -> (String, usize) {
        let mut out = Vec::new();
        let failures = run(fs, &paths(list), dest, opts, &mut out).unwrap();
        (String::from_utf8(out).unwrap(), failures)
    }

    fn manifest_text(root: &Path) -> String {
        std::fs::read_to_string(Manifest::path_for(root)).unwrap_or_default()
    }

    #[test]
    fn second_run_skips_verified_files() {
        let tmp = tempfile::tempdir().unwrap();
        let fs = Flaky::new(0, worker_restarted);
        let (out, _) = cp_dyn(&fs, &[DCIM], tmp.path(), rec());
        assert!(out.contains("copied=3 skipped=0"), "{out}");
        assert_eq!(manifest_text(tmp.path()).lines().count(), 3);
        let (out, failures) = cp_dyn(&fs, &[DCIM], tmp.path(), rec());
        assert_eq!(failures, 0);
        assert!(
            out.contains("[skip] /Internal Storage/DCIM/202601_a/IMG_0001.HEIC  verified"),
            "{out}"
        );
        assert!(
            out.contains("copied=0 skipped=3 exists=0 failed=0"),
            "{out}"
        );
        assert_eq!(fs.reads.get(), 3);
        assert_eq!(files_in(tmp.path()), ["DCIM"]);
        assert!(tmp.path().join(".win-iphone-dcim/manifest.jsonl").is_file());
    }

    #[test]
    fn manifest_records_path_under_dest_and_hides_device_id() {
        let tmp = tempfile::tempdir().unwrap();
        let fs = Flaky::new(0, worker_restarted);
        cp_dyn(&fs, &[DCIM], tmp.path(), rec());
        let text = manifest_text(tmp.path());
        assert!(!text.contains(RAW_ID));
        assert!(!text.contains("00008030001a2b3c4d5e6f70"));
        assert!(!text.contains("vid_05ac"));
        assert!(text.contains(&device_key(RAW_ID)));
        let m = Manifest::load(tmp.path()).unwrap();
        let e = m.get("DCIM/202601_b/IMG_0001.HEIC").unwrap();
        assert_eq!(e.record.size, 6);
        assert_eq!(
            e.record.source.as_deref(),
            Some("/Internal Storage/DCIM/202601_b/IMG_0001.HEIC")
        );
    }

    #[test]
    fn single_file_to_new_name_records_in_parent() {
        let tmp = tempfile::tempdir().unwrap();
        cp(&[B1], &tmp.path().join("b.heic"), opts()).unwrap();
        let m = Manifest::load(tmp.path()).unwrap();
        assert!(m.get("b.heic").is_some());
        let (out, _) = cp(&[B1], &tmp.path().join("b.heic"), opts()).unwrap();
        assert!(out.contains("verified"), "{out}");
    }

    #[test]
    fn truncated_last_manifest_line_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        cp(&[DCIM], tmp.path(), rec()).unwrap();
        let path = Manifest::path_for(tmp.path());
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(br#"{"v":1,"path":"DCIM/202601_a/IMG_0002.M"#)
            .unwrap();
        drop(f);
        let (out, failures) = cp(&[DCIM], tmp.path(), rec()).unwrap();
        assert_eq!(failures, 0);
        assert!(out.contains("copied=0 skipped=3 exists=0"), "{out}");
        // A new record after the broken line is still readable.
        std::fs::remove_file(tmp.path().join("DCIM/202601_b/IMG_0001.HEIC")).unwrap();
        let (out, _) = cp(&[DCIM], tmp.path(), rec()).unwrap();
        assert!(out.contains("copied=1 skipped=2"), "{out}");
        let m = Manifest::load(tmp.path()).unwrap();
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn stale_record_is_a_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        cp(&[A1], tmp.path(), opts()).unwrap();
        // The record says 99 bytes; the local file and the device say 6.
        let path = Manifest::path_for(tmp.path());
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"size\":6", "\"size\":99")).unwrap();
        let (out, failures) = cp(&[A1], tmp.path(), opts()).unwrap();
        assert_eq!(failures, 0);
        assert!(out.contains("copied=0 skipped=1 exists=1"), "{out}");
        let force = CpOptions {
            on_exists: OnExists::Overwrite,
            ..opts()
        };
        let (out, _) = cp(&[A1], tmp.path(), force).unwrap();
        assert!(out.contains("copied=1"), "{out}");
        let m = Manifest::load(tmp.path()).unwrap();
        assert_eq!(m.get("IMG_0001.HEIC").unwrap().record.size, 6);
        let (out, _) = cp(&[A1], tmp.path(), opts()).unwrap();
        assert!(out.contains("verified"), "{out}");
    }

    #[test]
    fn unverified_existing_is_skipped_and_not_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        // Same size as the device file, but no manifest record.
        std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"HEIC-A").unwrap();
        let (out, failures) = cp(&[A1], tmp.path(), opts()).unwrap();
        assert_eq!(failures, 0);
        assert!(out.contains("copied=0 skipped=1 exists=1"), "{out}");
        assert!(!out.contains("verified"), "{out}");
        assert!(manifest_text(tmp.path()).is_empty());
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"HEIC-A"
        );
        // A second run still does not claim it.
        let (out, _) = cp(&[A1], tmp.path(), opts()).unwrap();
        assert!(out.contains("exists=1"), "{out}");
        // -f replaces it and records it.
        let force = CpOptions {
            on_exists: OnExists::Overwrite,
            ..opts()
        };
        cp(&[A1], tmp.path(), force).unwrap();
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"heic-a"
        );
        assert!(
            Manifest::load(tmp.path())
                .unwrap()
                .get("IMG_0001.HEIC")
                .is_some()
        );
    }

    #[test]
    fn local_hash_is_stored_and_a_mismatch_is_a_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let hashed = CpOptions {
            local_hash: true,
            ..opts()
        };
        let (out, _) = cp(&[A1], tmp.path(), hashed).unwrap();
        assert!(out.contains("local-hash"), "{out}");
        let m = Manifest::load(tmp.path()).unwrap();
        let r = &m.get("IMG_0001.HEIC").unwrap().record;
        assert_eq!(r.hash_alg.as_deref(), Some("blake3"));
        assert_eq!(
            r.hash.as_deref(),
            Some(blake3::hash(b"heic-a").to_hex().as_str())
        );
        let (out, _) = cp(&[A1], tmp.path(), hashed).unwrap();
        assert!(out.contains("verified"), "{out}");
        // Same size, other bytes.
        std::fs::write(tmp.path().join("IMG_0001.HEIC"), b"heic-X").unwrap();
        let (out, failures) = cp(&[A1], tmp.path(), hashed).unwrap();
        assert_eq!(failures, 0);
        assert!(out.contains("copied=0 skipped=1 exists=1"), "{out}");
        // Size mode does not read the file, so it skips.
        let (out, _) = cp(&[A1], tmp.path(), opts()).unwrap();
        assert!(out.contains("verified"), "{out}");
    }

    #[test]
    fn dry_run_prints_decisions_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        cp(&[DCIM], tmp.path(), rec()).unwrap();
        std::fs::write(tmp.path().join("DCIM/202601_b/IMG_0001.HEIC"), b"x").unwrap();
        std::fs::remove_file(tmp.path().join("DCIM/202601_a/IMG_0002.MOV")).unwrap();
        let before = manifest_text(tmp.path());
        let dry = CpOptions {
            dry_run: true,
            ..rec()
        };
        let (out, _) = cp(&[DCIM], tmp.path(), dry).unwrap();
        assert!(
            out.contains("[skip] /Internal Storage/DCIM/202601_a/IMG_0001.HEIC  verified"),
            "{out}"
        );
        let conflict = out.lines().find(|l| l.contains("  conflict:")).unwrap();
        assert!(conflict.contains("202601_b"), "{out}");
        assert!(
            out.contains("[plan] /Internal Storage/DCIM/202601_a/IMG_0002.MOV -> "),
            "{out}"
        );
        assert!(out.contains("planned=1 skipped=2 exists=1"), "{out}");
        assert_eq!(manifest_text(tmp.path()), before);
        assert!(!tmp.path().join("DCIM/202601_a/IMG_0002.MOV").exists());
    }

    thread_local! {
        static SLEEPS: RefCell<Vec<Duration>> = const { RefCell::new(Vec::new()) };
    }

    fn record_sleep(d: Duration) {
        SLEEPS.with(|s| s.borrow_mut().push(d));
    }

    #[test]
    fn transient_error_is_retried_then_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let fs = Flaky::new(usize::MAX, worker_restarted);
        let opts = CpOptions {
            retries: 3,
            sleep: record_sleep,
            ..opts()
        };
        SLEEPS.with(|s| s.borrow_mut().clear());
        let (out, failures) = cp_dyn(&fs, &[A1], tmp.path(), opts);
        assert_eq!(failures, 1);
        assert_eq!(fs.reads.get(), 4);
        for k in 1..=3 {
            assert!(
                out.contains(&format!("[retry {k}/3] {A1}  read: the device worker")),
                "{out}"
            );
        }
        assert!(!out.contains("[retry 4/3]"), "{out}");
        assert!(out.contains("[failed] worker: 1"), "{out}");
        let secs: Vec<u64> = SLEEPS.with(|s| s.borrow().iter().map(Duration::as_secs).collect());
        assert_eq!(secs, [1, 3, 10]);
        // No partial file is left.
        assert!(files_in(tmp.path()).is_empty());
        assert_eq!(backoff(4), Duration::from_secs(10));
    }

    #[test]
    fn transient_error_then_success_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let fs = Flaky::new(2, worker_restarted);
        let (out, failures) = cp_dyn(&fs, &[A1], tmp.path(), opts());
        assert_eq!(failures, 0);
        assert_eq!(fs.reads.get(), 3);
        assert!(out.contains("[retry 2/3]"), "{out}");
        assert!(out.contains("copied=1"), "{out}");
        assert_eq!(files_in(tmp.path()), ["IMG_0001.HEIC"]);
        assert_eq!(
            std::fs::read(tmp.path().join("IMG_0001.HEIC")).unwrap(),
            b"heic-a"
        );
    }

    #[test]
    fn permanent_error_is_not_retried() {
        let tmp = tempfile::tempdir().unwrap();
        let fs = Flaky::new(usize::MAX, || Error::Io {
            context: "write".into(),
            source: std::io::Error::from(ErrorKind::StorageFull),
        });
        let (out, failures) = cp_dyn(&fs, &[A1], tmp.path(), opts());
        assert_eq!(failures, 1);
        assert_eq!(fs.reads.get(), 1);
        assert!(!out.contains("[retry"), "{out}");
        assert!(out.contains("[failed] io: 1"), "{out}");
    }

    #[test]
    fn zero_retries_means_one_attempt() {
        let tmp = tempfile::tempdir().unwrap();
        let fs = Flaky::new(usize::MAX, worker_restarted);
        let opts = CpOptions {
            retries: 0,
            ..opts()
        };
        let (_, failures) = cp_dyn(&fs, &[A1], tmp.path(), opts);
        assert_eq!(failures, 1);
        assert_eq!(fs.reads.get(), 1);
    }
}
