//! The device thread of the GUI.
//!
//! One thread owns the `DeviceFs` and every object that it holds (the
//! worker process pipes, or COM objects without isolation). The UI thread
//! sends a `Request` and gets `Reply` values back. It never calls the
//! device itself and never waits for the device thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::time::{Duration, Instant};

use super::cache;
use super::chunks::{self, Chunk, ChunkWriter, Pending};
use super::filedesc::{self, FileItem, Listing};
use super::selection::{Entry, SelectedFs, Selection};
use crate::backup::engine::{self, CopyOptions, CopySummary, Note, OnExists, ProgressSink};
use crate::backup::manifest::device_key;
use crate::backup::transfer::transfer;
use crate::device_fs::{self, CachedFs, DeviceFs};
use crate::devpath::DevicePath;
use crate::error::{Error, Result};
use crate::model::{DeviceInfo, Node, join_device_path};
use crate::paths::to_verbatim;
use crate::supervisor::{self, WorkerCommand};

/// The fastest rate of progress replies.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Copy log lines that wait for the next progress reply, at most.
const NOTE_BATCH: usize = 256;

/// How the device thread finds and opens devices.
pub trait Connector: Send {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>>;
    fn open(&self, index: usize) -> Result<Box<dyn DeviceFs>>;
}

/// Devices in a worker process: the CLI program next to the GUI program.
pub struct WorkerConnector {
    pub worker: WorkerCommand,
    pub timeout: Duration,
}

impl Connector for WorkerConnector {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        supervisor::list_devices(&self.worker, self.timeout)
    }

    fn open(&self, index: usize) -> Result<Box<dyn DeviceFs>> {
        device_fs::open_remote(self.worker.clone(), Some(index), self.timeout)
    }
}

pub enum Request {
    ListDevices,
    Open {
        index: usize,
    },
    /// List the folder at a device path.
    List {
        path: String,
    },
    /// Copy into the existing folder `dest`.
    Copy {
        what: CopySet,
        dest: PathBuf,
        force: bool,
        /// Write the manifest of the copy root, like `cp --manifest`.
        manifest: bool,
        /// Files and bytes of the set, if the UI knows them. `None` makes
        /// the device thread walk the set first.
        totals: Option<(u64, u64)>,
    },
    /// Download a file to the cache for opening.
    Download {
        path: String,
    },
    /// Delete the cache folder of the open device.
    ClearCache,
    /// Set the soft size limit of the cache, in bytes, for the next
    /// download.
    SetCacheMax(u64),
    /// Walk `paths` and their folders for an Explorer paste. The listing
    /// goes into `slot`, because the UI thread may be inside a drag loop
    /// and read no replies.
    Enumerate {
        paths: Vec<String>,
        slot: Arc<Pending<ListingResult>>,
    },
    /// Read a file into a chunk channel for an Explorer paste stream.
    OpenRead {
        path: String,
        /// 1-based number of the file in its paste, and the file count.
        number: usize,
        files: usize,
        tx: SyncSender<Chunk>,
    },
}

/// The listing of a paste, or why the walk failed.
pub type ListingResult = std::result::Result<Arc<Listing>, String>;

/// What a copy copies.
pub enum CopySet {
    /// The checked objects, like `cp -r -p <copy root> DEST` limited to them.
    Checked(Selection),
    /// These device paths, like `cp -r -p <paths>... DEST`.
    Paths(Vec<String>),
}

/// An open device.
#[derive(Debug, Clone)]
pub struct Opened {
    pub root: Entry,
    /// The cache folder of this device. `None` if no cache folder is known.
    pub cache_dir: Option<PathBuf>,
}

/// Progress of a running copy.
#[derive(Debug, Clone, Default)]
pub struct CopyProgress {
    pub files_found: u64,
    pub files_done: u64,
    pub bytes_found: u64,
    pub bytes_done: u64,
    /// Bytes read from the device in this run, for the speed.
    pub bytes_transferred: u64,
    pub current: Option<CurrentFile>,
}

#[derive(Debug, Clone)]
pub struct CurrentFile {
    pub source: String,
    pub size: Option<u64>,
    pub bytes: u64,
    pub started: Instant,
}

pub enum Reply {
    Devices(std::result::Result<Vec<DeviceInfo>, String>),
    Opened(std::result::Result<Opened, String>),
    Listed {
        path: String,
        result: std::result::Result<Vec<Entry>, String>,
    },
    /// The pre-scan of a copy found this much so far.
    CopyScan {
        files: u64,
        bytes: u64,
    },
    CopyProgress(CopyProgress),
    /// Log lines of the copy, in order, batched like the progress.
    CopyNotes(Vec<(Note, String)>),
    CopyDone(std::result::Result<CopySummary, String>),
    DownloadProgress {
        path: String,
        bytes: u64,
        size: Option<u64>,
    },
    /// The file is complete at `local`. Only now can it be opened.
    Downloaded {
        path: String,
        local: PathBuf,
        reused: bool,
    },
    DownloadFailed {
        path: String,
        error: String,
    },
    CacheCleared(std::result::Result<PathBuf, String>),
    /// The result of opening a file with its default application.
    ShellOpened {
        local: PathBuf,
        result: std::result::Result<(), String>,
    },
    /// A line for the log about an Explorer paste.
    PasteNote(String),
    /// The walk of an `Enumerate` request has found `files` files so far.
    EnumerateProgress {
        files: usize,
    },
    /// Explorer reads file `number` of `files`.
    PasteProgress {
        number: usize,
        files: usize,
        path: String,
        /// Bytes of this file read so far.
        bytes: u64,
        /// The size of this file, if the device reports it.
        size: Option<u64>,
    },
    /// A paste stream ended: the bytes read, or the error.
    PasteFileDone {
        number: usize,
        files: usize,
        path: String,
        result: std::result::Result<u64, String>,
    },
}

/// The UI side of the device thread.
pub struct DeviceHandle {
    tx: Sender<Request>,
    pub rx: Receiver<Reply>,
    /// For replies from other helper threads, for example the shell open.
    pub reply_tx: Sender<Reply>,
    /// Set to stop a copy after the current file.
    pub cancel: Arc<AtomicBool>,
    /// True after the device thread woke the UI, until the UI takes the
    /// replies. The device thread wakes the UI once per take.
    woken: Arc<AtomicBool>,
}

impl DeviceHandle {
    /// Start the device thread. `cache_max` is the soft size limit of the
    /// cache. `wake` runs after each reply, so the UI can repaint.
    pub fn spawn(
        connector: Box<dyn Connector>,
        cache_base: Option<PathBuf>,
        cache_max: u64,
        wake: Box<dyn Fn() + Send>,
    ) -> Self {
        let (tx, requests) = mpsc::channel();
        let (reply_tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let woken = Arc::new(AtomicBool::new(false));
        let out = Out {
            tx: reply_tx.clone(),
            wake,
            woken: woken.clone(),
        };
        let thread_cancel = cancel.clone();
        // The thread ends when the handle drops the request channel. Nobody
        // joins it, so a hung device call cannot block the window. The
        // `DeviceFs` is created on this thread and never leaves it.
        std::thread::Builder::new()
            .name("device".into())
            .spawn(move || {
                let mut thread = DeviceThread {
                    connector,
                    cache_base,
                    cache_max,
                    fs: None,
                    nodes: HashMap::new(),
                    device_key: None,
                    out,
                    cancel: thread_cancel,
                };
                while let Ok(request) = requests.recv() {
                    thread.handle(request);
                }
            })
            .expect("start the device thread");
        Self {
            tx,
            rx,
            reply_tx,
            cancel,
            woken,
        }
    }

    /// Up to `max` waiting replies, oldest first. Never blocks. The rest
    /// stay for the next call. The next reply after this call wakes the
    /// UI again.
    pub fn take_replies(&self, max: usize) -> Vec<Reply> {
        self.woken.store(false, Ordering::SeqCst);
        self.rx.try_iter().take(max).collect()
    }

    /// A sender for requests from other threads, for example the paste
    /// streams that Explorer reads.
    pub fn sender(&self) -> Sender<Request> {
        self.tx.clone()
    }

    pub fn send(&self, request: Request) {
        // A send fails only if the device thread is gone; the UI then gets
        // no reply and shows its last state.
        let _ = self.tx.send(request);
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        // A running copy stops after the current file.
        self.cancel.store(true, Ordering::SeqCst);
    }
}

struct Out {
    tx: Sender<Reply>,
    wake: Box<dyn Fn() + Send>,
    /// See `DeviceHandle::woken`.
    woken: Arc<AtomicBool>,
}

impl Out {
    /// Send `reply` and wake the UI, unless a wake is pending. A burst of
    /// replies then posts one repaint request, not one per reply.
    fn send(&self, reply: Reply) {
        let _ = self.tx.send(reply);
        if !self.woken.swap(true, Ordering::SeqCst) {
            (self.wake)();
        }
    }
}

struct DeviceThread {
    connector: Box<dyn Connector>,
    cache_base: Option<PathBuf>,
    /// Soft size limit of the cache, in bytes.
    cache_max: u64,
    fs: Option<Box<dyn DeviceFs>>,
    /// Nodes from earlier listings, by device path.
    nodes: HashMap<String, Node>,
    /// Hashed device ID of the open device.
    device_key: Option<String>,
    out: Out,
    cancel: Arc<AtomicBool>,
}

fn text(e: Error) -> String {
    e.to_string()
}

impl DeviceThread {
    fn handle(&mut self, request: Request) {
        match request {
            Request::ListDevices => {
                let result = self.connector.list_devices().map_err(text);
                self.out.send(Reply::Devices(result));
            }
            Request::Open { index } => {
                let result = self.open(index).map_err(text);
                self.out.send(Reply::Opened(result));
            }
            Request::List { path } => {
                let result = self.list(&path).map_err(text);
                self.out.send(Reply::Listed { path, result });
            }
            Request::Copy {
                what,
                dest,
                force,
                manifest,
                totals,
            } => {
                let started = Instant::now();
                let result = self
                    .copy(&what, &dest, force, manifest, totals, started)
                    .map_err(text);
                tracing::debug!("copy: done in {} ms", started.elapsed().as_millis());
                self.out.send(Reply::CopyDone(result));
            }
            Request::Download { path } => match self.download(&path) {
                Ok((local, reused)) => self.out.send(Reply::Downloaded {
                    path,
                    local,
                    reused,
                }),
                Err(e) => self.out.send(Reply::DownloadFailed {
                    path,
                    error: e.to_string(),
                }),
            },
            Request::ClearCache => {
                let result = self
                    .device_cache()
                    .and_then(|dir| cache::clear(&dir).map(|()| dir))
                    .map_err(text);
                self.out.send(Reply::CacheCleared(result));
            }
            Request::SetCacheMax(bytes) => self.cache_max = bytes,
            Request::Enumerate { paths, slot } => {
                let result = self.enumerate(&paths).map_err(text).map(|items| {
                    let listing = Listing::new(&items, filedesc::device_filetime);
                    for skip in &listing.skipped {
                        tracing::warn!("paste: {skip}");
                        self.out.send(Reply::PasteNote(format!("[skip] {skip}")));
                    }
                    Arc::new(listing)
                });
                slot.set(result);
                (self.out.wake)();
            }
            Request::OpenRead {
                path,
                number,
                files,
                tx,
            } => {
                let result = self.read_chunks(&path, number, files, &tx).map_err(text);
                if let Err(e) = &result {
                    tracing::warn!("paste: {path}: {e}");
                    chunks::send_error(&tx, e.clone());
                }
                self.out.send(Reply::PasteFileDone {
                    number,
                    files,
                    path,
                    result,
                });
            }
        }
    }

    /// Every object at and below `paths`, folders before their contents.
    /// Relative paths start at the common parent of `paths`.
    fn enumerate(&mut self, paths: &[String]) -> Result<Vec<FileItem>> {
        let base = filedesc::common_parent(paths);
        let mut items = Vec::new();
        let mut found = Vec::new();
        let out = &self.out;
        let mut files = 0;
        let mut last = Instant::now();
        let mut progress = |item: &FileItem| {
            if !item.is_folder {
                files += 1;
            }
            if last.elapsed() >= PROGRESS_INTERVAL {
                last = Instant::now();
                out.send(Reply::EnumerateProgress { files });
            }
        };
        for path in paths {
            let node = self.node(path)?;
            let rel = filedesc::relative(&base, path).ok_or_else(|| Error::PathNotFound {
                path: path.clone(),
                component: path.clone(),
            })?;
            walk(
                self.fs()?,
                &node,
                path.clone(),
                rel,
                &mut items,
                &mut found,
                &mut progress,
            )?;
        }
        // The streams resolve these paths again; keep the nodes.
        for (path, node) in found {
            self.nodes.entry(path).or_insert(node);
        }
        Ok(items)
    }

    /// Stream `path` into `tx` and end it with `Chunk::End`.
    fn read_chunks(
        &self,
        path: &str,
        number: usize,
        files: usize,
        tx: &SyncSender<Chunk>,
    ) -> Result<u64> {
        let node = self.node(path)?;
        if node.is_folder {
            return Err(Error::NotAFolder(path.to_owned()));
        }
        let size = node.size;
        let mut bytes = 0;
        let mut last = Instant::now();
        let out = &self.out;
        let mut writer = ChunkWriter::new(tx.clone(), |n| {
            bytes += n;
            if last.elapsed() >= PROGRESS_INTERVAL {
                last = Instant::now();
                out.send(Reply::PasteProgress {
                    number,
                    files,
                    path: path.to_owned(),
                    bytes,
                    size,
                });
            }
        });
        let n = self.fs()?.read_to(&node, &mut writer)?;
        writer.finish().map_err(|source| Error::Io {
            context: format!("send {path} to File Explorer"),
            source,
        })?;
        Ok(n)
    }

    fn open(&mut self, index: usize) -> Result<Opened> {
        self.fs = None;
        self.nodes.clear();
        let fs = self.connector.open(index)?;
        let root = fs.root();
        self.device_key = fs.device_id().map(|raw| device_key(&raw));
        self.nodes.insert("/".into(), root.clone());
        self.fs = Some(fs);
        Ok(Opened {
            root: Entry::new(None, &root),
            cache_dir: self.device_cache().ok(),
        })
    }

    fn fs(&self) -> Result<&dyn DeviceFs> {
        self.fs.as_deref().ok_or(Error::NoDevice)
    }

    /// The node at `path`: from an earlier listing, else resolved again.
    fn node(&self, path: &str) -> Result<Node> {
        if let Some(node) = self.nodes.get(path) {
            return Ok(node.clone());
        }
        let parsed = DevicePath::parse(path).map_err(|_| Error::PathNotFound {
            path: path.to_owned(),
            component: path.to_owned(),
        })?;
        device_fs::resolve(self.fs()?, &parsed)
    }

    fn list(&mut self, path: &str) -> Result<Vec<Entry>> {
        let dir = self.node(path)?;
        let children = self.fs()?.list(&dir)?;
        let mut entries = Vec::with_capacity(children.len());
        for child in children {
            let entry = Entry::new(Some(path), &child);
            self.nodes.entry(entry.path.clone()).or_insert(child);
            entries.push(entry);
        }
        Ok(entries)
    }

    fn copy(
        &self,
        what: &CopySet,
        dest: &Path,
        force: bool,
        manifest: bool,
        totals: Option<(u64, u64)>,
        started: Instant,
    ) -> Result<CopySummary> {
        // One memo for the scan and the copy: each folder is listed once.
        let cached = CachedFs::new(self.fs()?);
        let fs: &dyn DeviceFs = &cached;
        let (paths, selection) = match what {
            CopySet::Checked(selection) => match selection.copy_root() {
                Some(root) => (vec![root], Some(selection)),
                None => return Ok(CopySummary::default()),
            },
            CopySet::Paths(paths) => (paths.clone(), None),
        };
        let sources = paths
            .iter()
            .map(|p| {
                DevicePath::parse(p).map_err(|_| Error::PathNotFound {
                    path: p.clone(),
                    component: p.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let opts = CopyOptions {
            recursive: true,
            preserve: true,
            on_exists: if force {
                OnExists::Overwrite
            } else {
                OnExists::SkipWarn
            },
            manifest,
            ..CopyOptions::default()
        };
        let mut out = std::io::sink();
        let (files, bytes) = match totals {
            Some(t) => t,
            None => {
                // Walk the set once for the totals, so the overall progress
                // is exact.
                self.out.send(Reply::CopyScan { files: 0, bytes: 0 });
                let mut scan = ScanSink {
                    out: &self.out,
                    cancel: &self.cancel,
                    files: 0,
                    bytes: 0,
                    last: Instant::now(),
                };
                let plan = CopyOptions {
                    dry_run: true,
                    ..opts
                };
                let scanned = match selection {
                    Some(selection) => {
                        let view = SelectedFs::new(fs, selection);
                        engine::run(&view, &sources, dest, plan, &mut scan, &mut out)
                    }
                    None => engine::run(fs, &sources, dest, plan, &mut scan, &mut out),
                }?;
                if scanned.cancelled || self.cancel.load(Ordering::SeqCst) {
                    return Ok(CopySummary {
                        cancelled: true,
                        ..CopySummary::default()
                    });
                }
                self.out.send(Reply::CopyScan {
                    files: scan.files,
                    bytes: scan.bytes,
                });
                (scan.files, scan.bytes)
            }
        };
        tracing::debug!(
            "copy: {} source(s), engine starts {} ms after the request",
            sources.len(),
            started.elapsed().as_millis()
        );
        let mut sink = ChannelSink::new(&self.out, &self.cancel, files, bytes, started);
        match selection {
            Some(selection) => {
                let view = SelectedFs::new(fs, selection);
                engine::run(&view, &sources, dest, opts, &mut sink, &mut out)
            }
            None => engine::run(fs, &sources, dest, opts, &mut sink, &mut out),
        }
    }

    fn cache_base(&self) -> Result<PathBuf> {
        self.cache_base.clone().map_or_else(cache::base_dir, Ok)
    }

    fn device_cache(&self) -> Result<PathBuf> {
        Ok(cache::device_dir(
            &self.cache_base()?,
            self.device_key.as_deref(),
        ))
    }

    /// Download `path` to the cache, or reuse a complete cached copy.
    fn download(&self, path: &str) -> Result<(PathBuf, bool)> {
        let node = self.node(path)?;
        if node.is_folder {
            return Err(Error::NotAFolder(path.to_owned()));
        }
        let base = self.cache_base()?;
        let local = cache::file_path(&cache::device_dir(&base, self.device_key.as_deref()), path)?;
        if cache::is_fresh(&local, node.size) {
            cache::touch(&local);
            return Ok((local, true));
        }
        if let Some(size) = node.size {
            let room = cache::make_room(&base, size, self.cache_max);
            if room.deleted > 0 {
                tracing::debug!(
                    "cache: deleted {} files ({} bytes), {} bytes used",
                    room.deleted,
                    room.freed,
                    room.used
                );
            }
        }
        let target = to_verbatim(&local);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                context: format!("create the cache folder {}", parent.display()),
                source,
            })?;
        }
        let mut bytes = 0;
        let mut last = Instant::now();
        let out = &self.out;
        let size = node.size;
        transfer(self.fs()?, &node, &target, true, false, &mut |n| {
            bytes += n;
            if last.elapsed() >= PROGRESS_INTERVAL {
                last = Instant::now();
                out.send(Reply::DownloadProgress {
                    path: path.to_owned(),
                    bytes,
                    size,
                });
            }
        })?;
        Ok((local, false))
    }
}

/// Push `node` at `path` and, for a folder, everything below it.
fn walk(
    fs: &dyn DeviceFs,
    node: &Node,
    path: String,
    rel: String,
    items: &mut Vec<FileItem>,
    found: &mut Vec<(String, Node)>,
    progress: &mut dyn FnMut(&FileItem),
) -> Result<()> {
    let item = FileItem {
        path: path.clone(),
        rel: rel.clone(),
        is_folder: node.is_folder,
        size: node.size,
        modified: node.modified,
        created: node.created,
    };
    progress(&item);
    items.push(item);
    if node.is_folder {
        for child in fs.list(node)? {
            let name = child.display_name();
            let child_path = join_device_path(&path, &name);
            walk(
                fs,
                &child,
                child_path.clone(),
                format!("{rel}\\{name}"),
                items,
                found,
                progress,
            )?;
            found.push((child_path, child));
        }
    }
    Ok(())
}

/// Counts the files that a dry run plans and reports them as `CopyScan`.
struct ScanSink<'a> {
    out: &'a Out,
    cancel: &'a AtomicBool,
    files: u64,
    bytes: u64,
    last: Instant,
}

impl ProgressSink for ScanSink<'_> {
    fn found(&mut self, size: Option<u64>) {
        self.files += 1;
        self.bytes += size.unwrap_or(0);
        if self.last.elapsed() >= PROGRESS_INTERVAL {
            self.last = Instant::now();
            self.out.send(Reply::CopyScan {
                files: self.files,
                bytes: self.bytes,
            });
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// Engine progress as replies, at most one progress reply per interval.
///
/// Per-file events only update the state. A skip or a small file takes
/// microseconds, so one reply per file would flood the UI. The state goes
/// out with the next event after the interval, and always at the end.
/// Log lines wait for the same reply, or until `NOTE_BATCH` of them wait.
struct ChannelSink<'a> {
    out: &'a Out,
    cancel: &'a AtomicBool,
    progress: CopyProgress,
    /// Files and bytes that the planner of this run found so far.
    planned: (u64, u64),
    last: Instant,
    notes: Vec<(Note, String)>,
    /// When the device thread got the copy request. `None` after the
    /// first file is logged.
    started: Option<Instant>,
}

impl<'a> ChannelSink<'a> {
    /// The totals come from the pre-scan or the UI tree. They grow if the
    /// planner finds more.
    fn new(out: &'a Out, cancel: &'a AtomicBool, files: u64, bytes: u64, started: Instant) -> Self {
        Self {
            out,
            cancel,
            progress: CopyProgress {
                files_found: files,
                bytes_found: bytes,
                ..CopyProgress::default()
            },
            planned: (0, 0),
            last: Instant::now(),
            notes: Vec::new(),
            started: Some(started),
        }
    }

    fn push(&mut self, force: bool) {
        if force || self.last.elapsed() >= PROGRESS_INTERVAL {
            self.last = Instant::now();
            self.flush_notes();
            self.out.send(Reply::CopyProgress(self.progress.clone()));
        }
    }

    fn flush_notes(&mut self) {
        if !self.notes.is_empty() {
            self.out
                .send(Reply::CopyNotes(std::mem::take(&mut self.notes)));
        }
    }

    /// Log the time from the request to the first file, once.
    fn first_file(&mut self, what: &str) {
        if let Some(started) = self.started.take() {
            tracing::debug!(
                "copy: first file {what} {} ms after the request",
                started.elapsed().as_millis()
            );
        }
    }
}

impl ProgressSink for ChannelSink<'_> {
    fn begin(&mut self) {
        self.push(true);
    }

    fn found(&mut self, size: Option<u64>) {
        self.planned.0 += 1;
        self.planned.1 += size.unwrap_or(0);
        let p = &mut self.progress;
        p.files_found = p.files_found.max(self.planned.0);
        p.bytes_found = p.bytes_found.max(self.planned.1);
    }

    fn settled(&mut self, size: Option<u64>) {
        self.first_file("settled");
        self.progress.files_done += 1;
        self.progress.bytes_done += size.unwrap_or(0);
        self.push(false);
    }

    fn file_start(&mut self, source: &str, size: Option<u64>) {
        self.first_file("starts");
        self.progress.current = Some(CurrentFile {
            source: source.to_owned(),
            size,
            bytes: 0,
            started: Instant::now(),
        });
        self.push(false);
    }

    fn bytes(&mut self, n: u64) {
        self.progress.bytes_transferred += n;
        if let Some(c) = self.progress.current.as_mut() {
            c.bytes += n;
        }
        self.push(false);
    }

    fn restart_file(&mut self) {
        if let Some(c) = self.progress.current.as_mut() {
            c.bytes = 0;
            c.started = Instant::now();
        }
        self.push(true);
    }

    fn file_end(&mut self, _ok: bool) {
        if let Some(c) = self.progress.current.take() {
            self.progress.files_done += 1;
            self.progress.bytes_done += c.size.unwrap_or(c.bytes);
        }
        self.push(false);
    }

    fn finish(&mut self) {
        self.push(true);
    }

    fn note(&mut self, note: Note, text: &str) {
        self.notes.push((note, text.to_owned()));
        if self.notes.len() >= NOTE_BATCH {
            self.flush_notes();
        } else {
            self.push(false);
        }
    }

    fn summary(&mut self, _summary: &CopySummary) {
        self.flush_notes();
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

impl Drop for ChannelSink<'_> {
    /// A run that ends with an error sends its last lines too.
    fn drop(&mut self) {
        self.flush_notes();
    }
}

#[cfg(test)]
mod tests;
