//! Copy engine: copies device files and folders to a local path with the
//! `cp` rules.
//!
//! With `manifest`, a copy writes the JSONL manifest of the copy root and
//! applies the incremental rules. Without it, a local
//! file of the same size is kept. A transient failure is retried
//! with backoff. The engine reports progress to a
//! `ProgressSink`: the CLI draws terminal bars, the GUI forwards events to
//! its window.

use std::collections::{BTreeMap, HashMap};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
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

/// What one copy line or log line is about. The GUI shows these in its log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    /// A copied file, or a planned copy in a dry run.
    Copy,
    /// A skipped file.
    Skip,
    /// A new attempt after a transient failure.
    Retry,
    /// A warning, for example an overwrite or a leftover `.part` file.
    Warn,
    /// A failed item.
    Error,
    /// A summary line at the end of the run.
    Summary,
}

/// Counts of a finished run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopySummary {
    pub copied: usize,
    pub skipped: usize,
    pub exists: usize,
    pub failed: usize,
    pub bytes: u64,
    /// True if the run stopped early because `ProgressSink::cancelled` said so.
    pub cancelled: bool,
}

/// Receives the progress of a run. Every method has an empty default.
pub trait ProgressSink {
    /// The run starts, after the sources are checked.
    fn begin(&mut self) {}
    /// The planner found one more file. Totals grow while folders are listed.
    fn found(&mut self, _size: Option<u64>) {}
    /// A planned file is finished without a transfer (skipped or failed).
    fn settled(&mut self, _size: Option<u64>) {}
    /// A transfer of `source` starts.
    fn file_start(&mut self, _source: &str, _size: Option<u64>) {}
    /// `n` more bytes of the current file are on disk.
    fn bytes(&mut self, _n: u64) {}
    /// The current file starts again from the first byte after a failure.
    fn restart_file(&mut self) {}
    /// The current file is finished. `ok` is false for a failed transfer.
    fn file_end(&mut self, _ok: bool) {}
    /// The planner has no more folders to list. The totals are final.
    fn scan_done(&mut self) {}
    /// No more progress follows. Remove any progress display.
    fn finish(&mut self) {}
    /// A result line or a log line for the user.
    fn note(&mut self, _note: Note, _text: &str) {}
    /// The counts of the run, after the last line.
    fn summary(&mut self, _summary: &CopySummary) {}
    /// Run `f` with the progress display hidden, to print a line.
    fn suspend(&mut self, f: &mut dyn FnMut()) {
        f()
    }
    /// True if the user asked to stop. The engine stops before the next item.
    fn cancelled(&self) -> bool {
        false
    }
}

/// A sink that ignores everything.
pub struct NoProgress;

impl ProgressSink for NoProgress {}

#[derive(Debug, Clone, Copy)]
pub struct CopyOptions {
    pub recursive: bool,
    /// `-p`: set the local file times from the device after the commit.
    pub preserve: bool,
    pub dry_run: bool,
    pub on_exists: OnExists,
    /// `--verify local-hash`: hash new copies while they stream, and check
    /// the stored hash before a skip.
    pub local_hash: bool,
    /// Additional attempts after a transient failure.
    pub retries: u32,
    /// Waits between attempts. Tests replace it.
    pub sleep: fn(Duration),
    /// `--diagnostic`: log the raw device ID.
    pub diagnostic: bool,
    /// `--manifest`: read and write `DEST/.win-iphone-dcim/manifest.jsonl`.
    pub manifest: bool,
}

impl Default for CopyOptions {
    fn default() -> Self {
        Self {
            recursive: false,
            preserve: false,
            dry_run: false,
            on_exists: OnExists::SkipWarn,
            local_hash: false,
            retries: DEFAULT_RETRIES,
            sleep: std::thread::sleep,
            diagnostic: false,
            manifest: false,
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

    fn counts(&self, cancelled: bool) -> CopySummary {
        CopySummary {
            copied: self.copied,
            skipped: self.skipped,
            exists: self.exists,
            failed: self.failed(),
            bytes: self.bytes,
            cancelled,
        }
    }
}

struct Run<'a> {
    fs: &'a dyn DeviceFs,
    opts: CopyOptions,
    out: &'a mut dyn Write,
    progress: &'a mut dyn ProgressSink,
    summary: Summary,
    /// Device key for the manifest (hash of the raw device ID).
    device: Option<String>,
    /// The manifest of the copy root. Loaded when the root is known.
    manifest: Option<Manifest>,
    /// Without a manifest: the targets this run wrote, with their sources.
    written: HashMap<PathBuf, String>,
}

/// Copy `sources` to `dest`. Result lines go to `out` and to
/// `progress.note`. Return the counts.
///
/// A device error that stops all later items (device gone, access denied)
/// ends the run after the summary and is returned as `Err`.
pub fn run(
    fs: &dyn DeviceFs,
    sources: &[DevicePath],
    dest: &Path,
    opts: CopyOptions,
    progress: &mut dyn ProgressSink,
    out: &mut dyn Write,
) -> Result<CopySummary> {
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
    progress.begin();
    let mut run = Run {
        fs,
        opts,
        out,
        progress,
        summary: Summary::default(),
        device,
        manifest: None,
        written: HashMap::new(),
    };
    if opts.manifest && dest.is_dir() {
        run.open_manifest(&dest)?;
    }
    let mut fatal = None;
    let mut cancelled = false;
    loop {
        if run.progress.cancelled() {
            cancelled = true;
            run.warn("cancelled; the remaining items are not copied".into());
            break;
        }
        let Some(item) = planner.next() else {
            break;
        };
        if opts.manifest && run.manifest.is_none() {
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
    let counts = run.summary.counts(cancelled);
    run.progress.summary(&counts);
    match fatal {
        Some(e) => Err(e),
        None => Ok(counts),
    }
}

impl Run<'_> {
    fn println(&mut self, note: Note, line: String) -> Result<()> {
        self.progress.note(note, &line);
        let out = &mut *self.out;
        let mut result = Ok(());
        self.progress
            .suspend(&mut || result = writeln!(out, "{line}"));
        result.map_err(stdout_err)
    }

    /// Log a warning to stderr and to the sink.
    fn warn(&mut self, line: String) {
        tracing::warn!("{line}");
        self.progress.note(Note::Warn, &line);
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
            self.println(Note::Error, format!("[error] {source}  {error}"))?;
        } else {
            tracing::error!("{source}: {error}");
            self.progress
                .note(Note::Error, &format!("{source}: {error}"));
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
                    self.warn(format!(
                        "leftover partial file {} is not a complete file; it is left in place",
                        part.display()
                    ));
                }
                Ok(())
            }
            Ok(_) => Err(Error::NotAFolderLocal(target.to_path_buf())),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if self.opts.dry_run {
                    self.println(Note::Copy, format!("[plan] mkdir {}", target.display()))
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

    /// Apply the incremental rules to one file.
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
        if !self.opts.manifest {
            let state = match (self.written.get(target), node.size) {
                (Some(other), _) => {
                    ExistingState::Conflict(format!("copied in this run from {other}"))
                }
                (None, Some(size)) if size == local => match self.opts.on_exists {
                    OnExists::Overwrite => ExistingState::UnverifiedExisting,
                    _ => return Ok(SyncDecision::SkipSameSize),
                },
                (None, Some(size)) => {
                    ExistingState::Conflict(format!("local size {local}, device size {size}"))
                }
                (None, None) => ExistingState::Conflict("the device gives no size".into()),
            };
            return Ok(self.on_exists(state));
        }
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
        Ok(self.on_exists(state))
    }

    /// The decision for an existing target file that is not kept as is.
    fn on_exists(&self, state: ExistingState) -> SyncDecision {
        match self.opts.on_exists {
            OnExists::SkipWarn => SyncDecision::SkipExists { state, warn: true },
            OnExists::SkipQuiet => SyncDecision::SkipExists { state, warn: false },
            OnExists::Overwrite => SyncDecision::Overwrite { state },
        }
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
            self.println(Note::Skip, line)
        } else {
            tracing::warn!("{line}");
            self.progress.note(Note::Skip, &line);
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
                return self.println(Note::Skip, format!("[skip] {source}  verified"));
            }
            SyncDecision::SkipSameSize => {
                self.summary.skipped += 1;
                self.progress.settled(node.size);
                return self.println(
                    Note::Skip,
                    format!("[skip] {}  exists, same size", target.display()),
                );
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
            return self.println(
                Note::Copy,
                format!("[plan] {source} -> {}  ({size}{note})", target.display()),
            );
        }
        let replace = overwrite.is_some();
        let report = match self.transfer_with_retries(&source, &node, &target, replace)? {
            Ok(r) => r,
            // A file appeared at the target after the check.
            Err(Error::OutputExists(_)) if self.opts.on_exists != OnExists::Overwrite => {
                self.summary.skipped += 1;
                self.summary.exists += 1;
                if self.opts.on_exists == OnExists::SkipWarn {
                    let line = format!(
                        "[skip] {}  exists (use --force to replace)",
                        target.display()
                    );
                    tracing::warn!("{line}");
                    self.progress.note(Note::Skip, &line);
                }
                return Ok(());
            }
            Err(e) => return self.fail(&source, e),
        };
        if let Some(state) = &overwrite {
            self.warn(format!("[overwrite] {}  {state}", target.display()));
        }
        if self.opts.preserve {
            preserve_times(&target, &node);
        }
        if let Err(e) = self.record(&source, &node, &target, &report) {
            return self.fail(&source, e);
        }
        self.summary.copied += 1;
        self.summary.bytes += report.bytes;
        self.println(
            Note::Copy,
            format!(
                "[copy] {source} -> {}  {} / {size}  {}",
                target.display(),
                human_size(report.bytes),
                report.verification
            ),
        )
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
        let fs = self.fs;
        let result = loop {
            let progress = &mut *self.progress;
            let result = transfer(fs, node, target, replace, self.opts.local_hash, &mut |n| {
                progress.bytes(n)
            });
            match result {
                Err(e) if e.is_transient() && attempt < self.opts.retries => {
                    attempt += 1;
                    let line = format!("[retry {attempt}/{}] {source}  {e}", self.opts.retries);
                    self.println(Note::Retry, line)?;
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
        if !self.opts.manifest {
            self.written.insert(target.to_path_buf(), source.to_owned());
            return Ok(());
        }
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
        let mut lines = vec![(Note::Summary, line)];
        for (kind, list) in &s.failures {
            lines.push((Note::Summary, format!("[failed] {kind}: {}", list.len())));
            for f in list {
                lines.push((Note::Error, format!("  {}: {}", f.source, f.message)));
            }
        }
        for (note, l) in lines {
            self.println(note, l)?;
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
mod tests;
