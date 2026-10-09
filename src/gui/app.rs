//! The egui window. It runs on the UI thread and talks to the device
//! thread only through `DeviceHandle`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Button, Checkbox, Label, Layout, ProgressBar, RichText, Sense};
use egui_extras::{Column, TableBuilder};

use windows::Win32::System::Com::IDataObject;

use super::cache;
use super::chunks::Pending;
use super::copyview::CopyTracker;
use super::dataobject::{self, DataObject, PasteShared};
use super::device::{CopySet, DeviceHandle, ListingResult, Reply, Request, WorkerConnector};
use super::dnd;
use super::nav::{NavHistory, breadcrumbs};
use super::selection::{Check, Entry, ListSelection, Tree, band_rows, sort_rows};
use super::shell;
use crate::backup::engine::Note;
use crate::cmd::sort::SortKey;
use crate::model::{DeviceInfo, LocalTime, human_size};
use crate::speed::{SpeedMeter, format_eta, format_speed};
use crate::supervisor::{DEFAULT_TIMEOUT, WorkerCommand};

/// Lines kept in the log list.
const LOG_LIMIT: usize = 5000;
const SIZE_WIDTH: f32 = 90.0;
const DATE_WIDTH: f32 = 150.0;
/// Windows fonts for names that the default fonts cannot draw (CJK).
const FALLBACK_FONTS: [&str; 3] = ["msyh.ttc", "YuGothM.ttc", "malgun.ttf"];
const EXPLORER_COPY: &str = "Copy (paste in Explorer)";
/// Pointer travel before a drag from a selected row starts `DoDragDrop`.
const DRAG_DISTANCE: f32 = 6.0;
/// Largest scroll step per frame while a rubber band is past an edge.
/// The storage key of the "Clear cache on exit" setting.
const CLEAR_CACHE_KEY: &str = "clear_cache_on_exit";

/// Delete the cache of all devices. Errors are only logged.
fn clear_cache_folder() {
    if let Ok(base) = cache::base_dir() {
        let left = cache::clear_all(&base);
        tracing::debug!("cache cleared, {left} entries left");
    }
}

const BAND_SCROLL_MAX: f32 = 30.0;

pub fn run() -> ExitCode {
    // OLE clipboard and drag and drop need an STA on this thread. winit
    // also calls OleInitialize for its drop target; the calls nest.
    let ole = dataobject::ole_init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("win-iphone-dcim {}", crate::VERSION))
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([640.0, 400.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let result = eframe::run_native(
        "win-iphone-dcim",
        options,
        Box::new(|cc| {
            add_fallback_fonts(&cc.egui_ctx);
            // A file that a viewer still holds cannot be deleted at exit,
            // so the cache is cleared at the start as well.
            let clear_on_exit = cc
                .storage
                .and_then(|s| s.get_string(CLEAR_CACHE_KEY))
                .is_none_or(|v| v != "false");
            if clear_on_exit {
                clear_cache_folder();
            }
            let mut app = App::new(&cc.egui_ctx);
            app.clear_cache_on_exit = clear_on_exit;
            Ok(Box::new(app))
        }),
    );
    if ole {
        dataobject::ole_uninit();
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("GUI: {e}");
            ExitCode::from(crate::error::exit::INTERNAL as u8)
        }
    }
}

fn add_fallback_fonts(ctx: &egui::Context) {
    let Some(dir) = std::env::var_os("WINDIR").map(|w| PathBuf::from(w).join("Fonts")) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    for name in FALLBACK_FONTS {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            continue;
        };
        fonts.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push(name.to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

/// What a click or a menu item asks for. Applied after drawing.
enum Action {
    Toggle(String),
    SetChecked(Vec<String>, bool),
    Load(String),
    /// List a folder again, also if it is loaded.
    Reload(String),
    ShowFolder(String),
    /// A click on row `index` of the file list.
    Click {
        index: usize,
        ctrl: bool,
        shift: bool,
    },
    ContextClick(usize),
    SelectAll,
    DeselectAll,
    /// A click on a file in the tree: show its folder and select it.
    Reveal(String),
    Open(String),
    OpenCacheFolder(String),
    CopyTo(Vec<String>),
    Properties(String),
    ExpandAll(String),
    CollapseAll(String),
    /// A rubber-band drag starts at `x` (screen) and `y` (list content).
    BandStart {
        x: f32,
        y: f32,
        add: bool,
    },
    /// The band now covers these rows of the file list.
    BandUpdate(std::ops::Range<usize>),
    BandEnd,
    /// Put these paths on the clipboard for a paste in File Explorer.
    ExplorerCopy(Vec<String>),
    /// Drag these paths to File Explorer.
    DragOut(Vec<String>),
    Back,
    Forward,
    Up,
    /// A click on a column header.
    SortBy(SortKey),
}

/// The press point of a rubber-band drag: screen x, and y in list content
/// coordinates, so it stays on its row while the list scrolls.
#[derive(Clone, Copy)]
struct Band {
    x: f32,
    y: f32,
}

struct Download {
    path: String,
    bytes: u64,
    size: Option<u64>,
}

struct App {
    /// `None` if the worker program is missing.
    device: Option<DeviceHandle>,
    ctx: egui::Context,
    devices: Vec<DeviceInfo>,
    /// Index into `devices` of the open device.
    current: Option<usize>,
    tree: Option<Tree>,
    /// The cache folder of the open device.
    cache_dir: Option<PathBuf>,
    loading: HashSet<String>,
    /// Folders whose subfolders open when they are listed ("Expand all").
    expanding: HashSet<String>,
    /// The folder that the file list shows.
    folder: Option<String>,
    /// Back, Forward and Up of the file list.
    nav: NavHistory,
    /// The sort column of the file list, and true for descending.
    sort: (SortKey, bool),
    /// The highlighted rows of the file list.
    rows: ListSelection,
    /// The object in the properties window.
    properties: Option<String>,
    /// The rubber-band drag in the file list.
    band: Option<Band>,
    dest: Option<PathBuf>,
    force: bool,
    /// Delete the cache of all devices when the window closes.
    clear_cache_on_exit: bool,
    status: String,
    log: Vec<String>,
    copying: bool,
    copy: CopyTracker,
    /// The destination of the running copy, for the status line.
    copy_dest: PathBuf,
    /// The result line of the last cache download.
    download_final: Option<String>,
    download: Option<Download>,
    /// Shared by the data objects and streams of Explorer pastes.
    paste: Option<Arc<PasteShared>>,
    /// A clipboard copy that waits for its folder walk.
    clip_wait: Option<Arc<Pending<ListingResult>>>,
    /// The data object that this app put on the clipboard.
    clipboard: Option<IDataObject>,
    /// What Explorer reads now.
    paste_status: Option<String>,
    /// The paste stream that reads now: file number, files, bytes.
    paste_live: Option<(usize, usize, u64)>,
    /// Bytes of the files on the clipboard, 0 if unknown.
    paste_total: u64,
    paste_meter: SpeedMeter,
    download_meter: SpeedMeter,
    /// The user asked to close while Explorer reads.
    close_warning: bool,
    close_anyway: bool,
    /// `DoDragDrop` ate the button release; send egui one.
    release_pointer: bool,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let mut app = Self {
            device: None,
            ctx: ctx.clone(),
            devices: Vec::new(),
            current: None,
            tree: None,
            cache_dir: None,
            loading: HashSet::new(),
            expanding: HashSet::new(),
            folder: None,
            nav: NavHistory::default(),
            sort: (SortKey::Name, false),
            rows: ListSelection::default(),
            properties: None,
            band: None,
            dest: None,
            force: false,
            clear_cache_on_exit: true,
            status: String::new(),
            log: Vec::new(),
            copying: false,
            copy: CopyTracker::new(Instant::now()),
            copy_dest: PathBuf::new(),
            download_final: None,
            download: None,
            paste: None,
            clip_wait: None,
            clipboard: None,
            paste_status: None,
            paste_live: None,
            paste_total: 0,
            paste_meter: SpeedMeter::new(Instant::now()),
            download_meter: SpeedMeter::new(Instant::now()),
            close_warning: false,
            close_anyway: false,
            release_pointer: false,
        };
        match WorkerCommand::cli_next_to_current_exe() {
            Ok(worker) => {
                let wake_ctx = ctx.clone();
                let handle = DeviceHandle::spawn(
                    Box::new(WorkerConnector {
                        worker,
                        timeout: DEFAULT_TIMEOUT,
                    }),
                    None,
                    Box::new(move || wake_ctx.request_repaint()),
                );
                handle.send(Request::ListDevices);
                app.paste = Some(PasteShared::new(handle.sender()));
                app.device = Some(handle);
                app.status = "Looking for devices...".into();
            }
            Err(e) => {
                app.status = e.to_string();
                app.push_log(format!("[error] {e}"));
            }
        }
        app
    }

    fn send(&self, request: Request) {
        if let Some(d) = &self.device {
            d.send(request);
        }
    }

    fn push_log(&mut self, line: String) {
        if self.log.len() >= LOG_LIMIT {
            self.log.drain(..LOG_LIMIT / 10);
        }
        self.log.push(line);
    }

    fn error(&mut self, text: String) {
        self.status = text.clone();
        self.push_log(format!("[error] {text}"));
    }

    /// Handle every reply that is waiting. Never blocks.
    fn poll(&mut self) {
        let replies: Vec<Reply> = match &self.device {
            Some(d) => d.rx.try_iter().collect(),
            None => return,
        };
        for reply in replies {
            self.on_reply(reply);
        }
    }

    fn on_reply(&mut self, reply: Reply) {
        match reply {
            Reply::Devices(Ok(list)) => {
                self.status = format!("{} device(s) found", list.len());
                let keep = self
                    .current
                    .filter(|&i| i < list.len())
                    .or(if list.len() == 1 { Some(0) } else { None });
                if self.devices != list {
                    self.clear_own_clipboard("the device list changed");
                }
                self.devices = list;
                match keep {
                    Some(i) => self.open_device(i),
                    None => {
                        self.current = None;
                        self.tree = None;
                    }
                }
            }
            Reply::Devices(Err(e)) => {
                self.devices.clear();
                self.error(e);
            }
            Reply::Opened(Ok(opened)) => {
                self.status = "Device open".into();
                let path = opened.root.path.clone();
                self.tree = Some(Tree::new(opened.root));
                self.cache_dir = opened.cache_dir;
                self.loading.clear();
                self.expanding.clear();
                self.folder = Some(path.clone());
                self.nav.reset(&path);
                self.rows.clear();
                self.properties = None;
                self.load(&path);
            }
            Reply::Opened(Err(e)) => {
                self.tree = None;
                self.error(e);
            }
            Reply::Listed { path, result } => {
                self.loading.remove(&path);
                match result {
                    Ok(mut entries) => {
                        sort_entries(&mut entries);
                        if let Some(tree) = self.tree.as_mut() {
                            tree.set_children(&path, entries);
                        }
                        if self.expanding.remove(&path) {
                            self.expand_all(&path);
                        }
                    }
                    Err(e) => {
                        self.expanding.remove(&path);
                        self.error(format!("{path}: {e}"));
                    }
                }
            }
            Reply::CopyProgress(p) => {
                if self.copy.scanning {
                    self.status = format!("Copying to {}", self.copy_dest.display());
                }
                self.copy.update(p, Instant::now());
            }
            Reply::CopyScan { files, bytes } => {
                self.copy.scan(files, bytes);
                self.status = format!("Scanning... {files} files, {}", human_size(bytes));
            }
            Reply::CopyNote(note, text) => {
                // Copied files are in the counts; the log lists the rest.
                if note != Note::Copy {
                    self.push_log(text);
                }
            }
            Reply::CopyDone(result) => {
                self.copying = false;
                self.copy.scanning = false;
                match result {
                    Ok(s) => {
                        self.copy.finish(&s, Instant::now());
                        self.status = format!(
                            "Copy {}: copied {}, skipped {}, failed {}, {}",
                            if s.cancelled { "cancelled" } else { "done" },
                            s.copied,
                            s.skipped,
                            s.failed,
                            human_size(s.bytes)
                        );
                    }
                    Err(e) => self.error(format!("Copy failed: {e}")),
                }
            }
            Reply::DownloadProgress { path, bytes, size } => {
                self.download_final = None;
                let now = Instant::now();
                match &self.download {
                    Some(d) if d.path == path && bytes >= d.bytes => {
                        self.download_meter.add(bytes - d.bytes, now);
                    }
                    _ => {
                        self.download_meter.reset(now);
                        self.download_meter.add(bytes, now);
                    }
                }
                self.download = Some(Download { path, bytes, size });
            }
            Reply::Downloaded {
                path,
                local,
                reused,
            } => {
                let now = Instant::now();
                self.download_final = Some(match self.download.take() {
                    Some(d) => format!(
                        "Downloaded {}  {}  elapsed {}  avg {}",
                        file_name(&d.path),
                        human_size(d.bytes),
                        format_eta(self.download_meter.elapsed(now)),
                        format_speed(self.download_meter.average(now))
                    ),
                    None => format!("Cached {}", file_name(&path)),
                });
                self.status = format!(
                    "Opening {}{}",
                    file_name(&path),
                    if reused { " (cached)" } else { "" }
                );
                self.open_local(local);
            }
            Reply::DownloadFailed { path, error } => {
                self.download = None;
                self.error(format!("Cannot open {path}: {error}"));
            }
            Reply::CacheCleared(Ok(dir)) => {
                self.status = format!("Cache cleared: {}", dir.display());
            }
            Reply::CacheCleared(Err(e)) => self.error(e),
            Reply::ShellOpened { local, result } => match result {
                Ok(()) => self.status = format!("Opened {}", local.display()),
                Err(e) => self.error(format!("Cannot open {}: {e}", local.display())),
            },
            Reply::PasteNote(text) => self.push_log(text),
            Reply::PasteProgress {
                number,
                files,
                bytes,
                ..
            } => {
                let now = Instant::now();
                let delta = match self.paste_live {
                    Some((n, _, b)) if n == number && bytes >= b => bytes - b,
                    Some((n, ..)) if n != number && number != 1 => bytes,
                    Some(_) => {
                        // A new paste run starts at file 1 or re-reads a file.
                        if number == 1 {
                            self.paste_meter.reset(now);
                        }
                        bytes
                    }
                    None => {
                        self.paste_meter.reset(now);
                        bytes
                    }
                };
                self.paste_meter.add(delta, now);
                self.paste_live = Some((number, files, bytes));
            }
            Reply::PasteFileDone {
                number,
                files,
                path,
                result,
            } => match result {
                Ok(_) => {
                    self.paste_live = None;
                    self.paste_status = (number < files)
                        .then(|| format!("Explorer is reading {number} of {files}..."));
                    if number == files {
                        self.status = format!("Explorer read {files} file(s)");
                    }
                }
                Err(e) => {
                    self.paste_live = None;
                    self.paste_status = None;
                    self.error(format!("Explorer paste of {path} failed: {e}"));
                }
            },
        }
    }

    /// Start a clipboard copy of `paths`, or of the checked items when
    /// `paths` is empty. The data object goes on the clipboard when the
    /// device thread has walked the folders.
    fn explorer_copy(&mut self, mut paths: Vec<String>) {
        if paths.is_empty()
            && let Some(tree) = &self.tree
        {
            paths = tree.selection().top_paths();
        }
        if paths.is_empty() {
            tracing::info!("Explorer copy ignored: nothing selected or checked");
            self.status = "Select or check items to copy".into();
            return;
        }
        if self.paste.is_none() {
            tracing::info!(
                "Explorer copy ignored: {} path(s), worker missing",
                paths.len()
            );
            return;
        }
        tracing::info!("Explorer copy: listing {} path(s)", paths.len());
        let slot = Arc::new(Pending::default());
        self.status = format!("Preparing {} item(s) for File Explorer...", paths.len());
        self.send(Request::Enumerate {
            paths,
            slot: Arc::clone(&slot),
        });
        self.clip_wait = Some(slot);
    }

    /// Put the waiting clipboard copy on the clipboard once it is listed.
    fn finish_explorer_copy(&mut self) {
        let Some(result) = self.clip_wait.as_ref().and_then(|s| s.get()) else {
            return;
        };
        let (Some(slot), Some(shared)) = (self.clip_wait.take(), self.paste.clone()) else {
            return;
        };
        let listing = match result {
            Ok(l) => l,
            Err(e) => return self.error(format!("Cannot copy for Explorer: {e}")),
        };
        if listing.is_empty() {
            return self.error("Nothing to copy for Explorer".into());
        }
        let obj = DataObject::create(slot, shared);
        match dataobject::set_clipboard(&obj) {
            Ok(()) => {
                tracing::info!(
                    "Explorer copy: {} item(s), {} file(s) on the clipboard",
                    listing.len(),
                    listing.files
                );
                self.status = format!("{} item(s) copied. Paste in File Explorer.", listing.len());
                self.paste_total = listing
                    .entries
                    .iter()
                    .flatten()
                    .map(|f| f.size.unwrap_or(0))
                    .sum();
                self.clipboard = Some(obj);
            }
            Err(e) => {
                tracing::warn!("OleSetClipboard failed: {e}");
                self.error(format!("Cannot put the copy on the clipboard: {e}"));
            }
        }
    }

    /// Drag `paths` to File Explorer. Blocks in the OLE drag loop.
    fn drag_out(&mut self, paths: Vec<String>) {
        let Some(shared) = self.paste.clone() else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let slot = Arc::new(Pending::default());
        self.send(Request::Enumerate {
            paths,
            slot: Arc::clone(&slot),
        });
        let obj = DataObject::create(slot, shared);
        self.release_pointer = true;
        self.band = None;
        match dnd::drag(&obj) {
            Ok(true) => self.status = "Dropped. File Explorer copies the items.".into(),
            Ok(false) => self.status = "Drag cancelled".into(),
            Err(e) => self.error(format!("Drag and drop failed: {e}")),
        }
    }

    /// The status of the running Explorer paste, with speed and bytes left.
    fn paste_text(&self) -> Option<String> {
        let Some((number, files, _)) = self.paste_live else {
            return self.paste_status.clone();
        };
        let now = Instant::now();
        let mut text = format!(
            "Explorer is reading {number} of {files} \u{b7} {}",
            format_speed(self.paste_meter.current(now))
        );
        if self.paste_total > 0 {
            let left = self.paste_total.saturating_sub(self.paste_meter.total());
            text.push_str(&format!(" \u{b7} {} left", human_size(left)));
        }
        Some(text)
    }

    /// `12.3 MiB/s` for a running cache download.
    fn download_text(&self, d: &Download) -> String {
        let now = Instant::now();
        let mut text = format_speed(self.download_meter.current(now));
        if let Some(size) = d.size
            && let Some(eta) = self.download_meter.eta(size.saturating_sub(d.bytes), now)
        {
            text.push_str(&format!("  ETA {}", format_eta(eta)));
        }
        text
    }

    fn paste_busy(&self) -> bool {
        self.paste.as_ref().is_some_and(|p| p.busy())
    }

    /// Take this app's data object off the clipboard, so a later paste
    /// does not use device paths of another device.
    fn clear_own_clipboard(&mut self, why: &str) {
        if let Some(obj) = self.clipboard.take() {
            dataobject::clear_clipboard(&obj);
            tracing::info!("cleared the clipboard because {why}");
        }
    }

    fn open_device(&mut self, index: usize) {
        self.current = Some(index);
        self.tree = None;
        self.cache_dir = None;
        self.status = "Opening the device...".into();
        self.send(Request::Open { index });
    }

    fn load(&mut self, path: &str) {
        let loaded = self
            .tree
            .as_ref()
            .is_some_and(|t| t.children(path).is_some());
        if !loaded {
            self.reload(path);
        }
    }

    fn reload(&mut self, path: &str) {
        if self.loading.insert(path.to_owned()) {
            self.send(Request::List {
                path: path.to_owned(),
            });
        }
    }

    /// Open `path` and every folder below it. Folders that are not loaded
    /// yet open when their listing arrives.
    fn expand_all(&mut self, path: &str) {
        let Some(tree) = &self.tree else {
            return;
        };
        let folders = tree.loaded_folders(path);
        let unloaded: Vec<String> = folders
            .iter()
            .filter(|f| tree.children(f).is_none())
            .cloned()
            .collect();
        for f in &folders {
            set_open(&self.ctx, f, true);
        }
        for f in unloaded {
            self.expanding.insert(f.clone());
            self.load(&f);
        }
    }

    fn collapse_all(&mut self, path: &str) {
        let Some(tree) = &self.tree else {
            return;
        };
        for f in tree.loaded_folders(path) {
            self.expanding.remove(&f);
            set_open(&self.ctx, &f, false);
        }
    }

    /// The cached copy of a file, if it is complete.
    fn cached(&self, path: &str) -> Option<PathBuf> {
        let entry = self.tree.as_ref()?.entry(path)?;
        let local = cache::file_path(self.cache_dir.as_ref()?, path).ok()?;
        cache::is_fresh(&local, entry.size).then_some(local)
    }

    fn is_file(&self, path: &str) -> bool {
        self.tree
            .as_ref()
            .and_then(|t| t.entry(path))
            .is_some_and(|e| !e.is_folder)
    }

    /// Open a complete local file on a helper thread. `ShellExecuteW` can
    /// block while the shell starts the application.
    fn open_local(&self, local: PathBuf) {
        let Some(d) = &self.device else {
            return;
        };
        let tx = d.reply_tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = shell::open(&local);
            let _ = tx.send(Reply::ShellOpened { local, result });
            ctx.request_repaint();
        });
    }

    fn start_copy(&mut self, what: CopySet, dest: PathBuf) {
        let Some(d) = &self.device else {
            return;
        };
        // With the totals from the tree, the device thread skips its scan.
        let totals = self.tree.as_ref().and_then(|t| match &what {
            CopySet::Checked(selection) => t.totals(&selection.top_paths()),
            CopySet::Paths(paths) => t.totals(paths),
        });
        d.cancel.store(false, Ordering::SeqCst);
        d.send(Request::Copy {
            what,
            dest: dest.clone(),
            force: self.force,
            totals,
        });
        self.copying = true;
        self.copy.start(Instant::now());
        if totals.is_some() {
            self.status = format!("Copying to {}", dest.display());
        } else {
            self.copy.scan(0, 0);
            self.status = "Scanning...".into();
        }
        self.push_log(format!("[start] copy to {}", dest.display()));
        self.copy_dest = dest;
    }

    fn copy_checked(&mut self) {
        let (Some(tree), Some(dest)) = (&self.tree, &self.dest) else {
            return;
        };
        let selection = tree.selection();
        if !selection.is_empty() {
            let dest = dest.clone();
            self.start_copy(CopySet::Checked(selection), dest);
        }
    }

    /// The rows of the file list: the children of the shown folder.
    fn list_rows(&self) -> Vec<String> {
        let (Some(t), Some(f)) = (&self.tree, &self.folder) else {
            return Vec::new();
        };
        let mut entries: Vec<&Entry> = t
            .children(f)
            .unwrap_or_default()
            .iter()
            .filter_map(|p| t.entry(p))
            .collect();
        sort_rows(&mut entries, self.sort.0, self.sort.1);
        entries.into_iter().map(|e| e.path.clone()).collect()
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Toggle(path) => {
                    if let Some(t) = self.tree.as_mut() {
                        t.toggle(&path);
                    }
                }
                Action::SetChecked(paths, checked) => {
                    if let Some(t) = self.tree.as_mut() {
                        for p in paths {
                            t.set_checked(&p, checked);
                        }
                    }
                }
                Action::Load(path) => self.load(&path),
                Action::Reload(path) => self.reload(&path),
                Action::ShowFolder(path) => self.enter(path),
                Action::SortBy(key) => {
                    self.sort = if self.sort.0 == key {
                        (key, !self.sort.1)
                    } else {
                        (key, false)
                    };
                }
                Action::Back => {
                    if let Some(p) = self.nav.back().map(str::to_owned) {
                        self.show(p);
                    }
                }
                Action::Forward => {
                    if let Some(p) = self.nav.forward().map(str::to_owned) {
                        self.show(p);
                    }
                }
                Action::Up => {
                    if let Some(p) = self.nav.up().map(str::to_owned) {
                        self.show(p);
                    }
                }
                Action::Click { index, ctrl, shift } => {
                    let rows = self.list_rows();
                    self.rows.click(&rows, index, ctrl, shift);
                }
                Action::ContextClick(index) => {
                    let rows = self.list_rows();
                    self.rows.context_click(&rows, index);
                }
                Action::SelectAll => {
                    let rows = self.list_rows();
                    self.rows.select_all(&rows);
                }
                Action::DeselectAll => self.rows.clear(),
                Action::Reveal(path) => {
                    self.enter(parent_path(&path).to_owned());
                    let rows = self.list_rows();
                    if let Some(i) = rows.iter().position(|r| *r == path) {
                        self.rows.click(&rows, i, false, false);
                    }
                }
                Action::Open(path) => self.open(path),
                Action::OpenCacheFolder(path) => match self.cached(&path) {
                    Some(local) => {
                        if let Err(e) = shell::reveal(&local) {
                            self.error(e);
                        }
                    }
                    None => self.status = format!("{} is not in the cache", file_name(&path)),
                },
                Action::CopyTo(paths) => {
                    if !paths.is_empty()
                        && !self.copying
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        self.start_copy(CopySet::Paths(paths), dir);
                    }
                }
                Action::Properties(path) => {
                    if !self.is_file(&path) {
                        self.load(&path);
                    }
                    self.properties = Some(path);
                }
                Action::BandStart { x, y, add } => {
                    self.band = Some(Band { x, y });
                    self.rows.begin_band(add);
                }
                Action::BandUpdate(range) => {
                    if self.band.is_some() {
                        let rows = self.list_rows();
                        self.rows.update_band(&rows, range);
                    }
                }
                Action::BandEnd => {
                    self.band = None;
                    self.rows.end_band();
                }
                Action::ExpandAll(path) => self.expand_all(&path),
                Action::CollapseAll(path) => self.collapse_all(&path),
                Action::ExplorerCopy(paths) => self.explorer_copy(paths),
                Action::DragOut(paths) => self.drag_out(paths),
            }
        }
    }

    /// Show `path` in the file list and add it to the history.
    fn enter(&mut self, path: String) {
        self.nav.go(&path);
        self.show(path);
    }

    /// Show `path` in the file list. The history is already set.
    fn show(&mut self, path: String) {
        self.load(&path);
        if self.folder.as_deref() != Some(path.as_str()) {
            self.rows.clear();
            self.band = None;
        }
        self.folder = Some(path);
    }

    fn open(&mut self, path: String) {
        if !self.is_file(&path) {
            self.enter(path);
            return;
        }
        if self.download.as_ref().is_some_and(|d| d.path == path) {
            return;
        }
        self.status = format!("Downloading {}...", file_name(&path));
        self.download = Some(Download {
            path: path.clone(),
            bytes: 0,
            size: self
                .tree
                .as_ref()
                .and_then(|t| t.entry(&path))
                .and_then(|e| e.size),
        });
        self.send(Request::Download { path });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Device:");
            let name = self
                .current
                .and_then(|i| self.devices.get(i))
                .map(device_label)
                .unwrap_or_else(|| "(none)".into());
            let mut choice = None;
            egui::ComboBox::from_id_salt("device")
                .selected_text(name)
                .width(220.0)
                .show_ui(ui, |ui| {
                    for (i, d) in self.devices.iter().enumerate() {
                        if ui
                            .selectable_label(self.current == Some(i), device_label(d))
                            .clicked()
                        {
                            choice = Some(i);
                        }
                    }
                });
            if let Some(i) = choice
                && self.current != Some(i)
            {
                self.clear_own_clipboard("the device changed");
                self.open_device(i);
            }
            if ui.button("Refresh").clicked() {
                self.status = "Looking for devices...".into();
                self.send(Request::ListDevices);
            }
            ui.separator();
            if ui.button("Destination...").clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                self.dest = Some(dir);
            }
            let dest = self
                .dest
                .as_ref()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "(no destination)".into());
            ui.add(Label::new(dest).truncate());
            ui.checkbox(&mut self.force, "Overwrite existing (--force)");
            ui.checkbox(&mut self.clear_cache_on_exit, "Clear cache on exit");
            if self.copying {
                if ui.button("Cancel").clicked()
                    && let Some(d) = &self.device
                {
                    d.cancel.store(true, Ordering::SeqCst);
                    self.status = if self.copy.scanning {
                        "Cancel: the scan stops now".into()
                    } else {
                        "Cancel: the copy stops after the current file".into()
                    };
                }
            } else {
                let ready = self.dest.is_some()
                    && self
                        .tree
                        .as_ref()
                        .is_some_and(|t| !t.selection().is_empty());
                if ui
                    .add_enabled(ready, Button::new("Copy to folder"))
                    .on_hover_text("Copy the checked items into the destination")
                    .clicked()
                {
                    self.copy_checked();
                }
            }
            if ui
                .add_enabled(self.tree.is_some(), Button::new("Clear cache"))
                .clicked()
            {
                self.send(Request::ClearCache);
            }
            ui.separator();
            ui.label(&self.status);
            if let Some(d) = &self.download {
                ui.separator();
                ui.label(self.download_text(d));
            }
            if let Some(p) = self.paste_text() {
                ui.separator();
                ui.label(p);
            }
        });
        if self.close_warning {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "File Explorer is still reading files from this window. \
                     Closing it now makes the paste fail.",
                );
                if ui.button("Close anyway").clicked() {
                    self.close_anyway = true;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Keep open").clicked() {
                    self.close_warning = false;
                }
            });
        }
    }

    fn tree_panel(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(tree) = &self.tree else {
            ui.label("No device open.");
            return;
        };
        egui::ScrollArea::both()
            .id_salt("tree")
            .auto_shrink(false)
            .show(ui, |ui| self.tree_node(ui, tree, tree.root(), actions));
    }

    fn tree_node(&self, ui: &mut egui::Ui, tree: &Tree, path: &str, actions: &mut Vec<Action>) {
        let Some(entry) = tree.entry(path) else {
            return;
        };
        let check = tree.check(path);
        let one = vec![path.to_owned()];
        if !entry.is_folder {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent);
                check_box(ui, check, path, actions);
                let r = ui.selectable_label(self.rows.contains(path), &entry.name);
                if r.clicked() {
                    actions.push(Action::Reveal(path.to_owned()));
                }
                if r.double_clicked() {
                    actions.push(Action::Open(path.to_owned()));
                }
                r.context_menu(|ui| self.file_menu(ui, path, &one, actions));
            });
            return;
        }
        let state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            tree_id(path),
            path == tree.root(),
        );
        if state.is_open() && tree.children(path).is_none() {
            actions.push(Action::Load(path.to_owned()));
        }
        let shown = self.folder.as_deref() == Some(path);
        state
            .show_header(ui, |ui| {
                check_box(ui, check, path, actions);
                let r = ui.selectable_label(shown, RichText::new(&entry.name).strong());
                if r.clicked() {
                    actions.push(Action::ShowFolder(path.to_owned()));
                }
                r.context_menu(|ui| self.folder_menu(ui, path, &one, actions));
            })
            .body(|ui| match tree.children(path) {
                None => {
                    ui.weak("loading...");
                }
                Some([]) => {
                    ui.weak("(empty)");
                }
                Some(children) => {
                    for child in children {
                        self.tree_node(ui, tree, child, actions);
                    }
                }
            });
    }

    /// The menu of a file. `targets` are the objects that "Copy to..." and
    /// the check items act on.
    fn file_menu(
        &self,
        ui: &mut egui::Ui,
        path: &str,
        targets: &[String],
        actions: &mut Vec<Action>,
    ) {
        let single = targets.len() == 1;
        if ui.add_enabled(single, Button::new("Open")).clicked() {
            actions.push(Action::Open(path.to_owned()));
            ui.close();
        }
        if ui
            .add_enabled(
                self.cached(path).is_some(),
                Button::new("Open cache folder"),
            )
            .clicked()
        {
            actions.push(Action::OpenCacheFolder(path.to_owned()));
            ui.close();
        }
        self.copy_items(ui, targets, actions);
        ui.separator();
        if ui.button("Check").clicked() {
            actions.push(Action::SetChecked(targets.to_vec(), true));
            ui.close();
        }
        if ui.button("Uncheck").clicked() {
            actions.push(Action::SetChecked(targets.to_vec(), false));
            ui.close();
        }
        ui.separator();
        if ui.button("Properties").clicked() {
            actions.push(Action::Properties(path.to_owned()));
            ui.close();
        }
    }

    fn folder_menu(
        &self,
        ui: &mut egui::Ui,
        path: &str,
        targets: &[String],
        actions: &mut Vec<Action>,
    ) {
        if ui
            .add_enabled(targets.len() == 1, Button::new("Open"))
            .clicked()
        {
            actions.push(Action::ShowFolder(path.to_owned()));
            ui.close();
        }
        self.copy_items(ui, targets, actions);
        ui.separator();
        if ui.button("Check all beneath").clicked() {
            actions.push(Action::SetChecked(targets.to_vec(), true));
            ui.close();
        }
        if ui.button("Uncheck all beneath").clicked() {
            actions.push(Action::SetChecked(targets.to_vec(), false));
            ui.close();
        }
        ui.separator();
        if ui.button("Expand all").clicked() {
            actions.push(Action::ExpandAll(path.to_owned()));
            ui.close();
        }
        if ui.button("Collapse all").clicked() {
            actions.push(Action::CollapseAll(path.to_owned()));
            ui.close();
        }
        ui.separator();
        if ui.button("Properties").clicked() {
            actions.push(Action::Properties(path.to_owned()));
            ui.close();
        }
    }

    fn copy_items(&self, ui: &mut egui::Ui, targets: &[String], actions: &mut Vec<Action>) {
        if ui
            .add_enabled(!self.copying, Button::new("Copy to..."))
            .on_hover_text("Pick a folder, then copy like cp -r -p")
            .clicked()
        {
            actions.push(Action::CopyTo(targets.to_vec()));
            ui.close();
        }
        if ui
            .add_enabled(
                self.paste.is_some(),
                Button::new(EXPLORER_COPY).shortcut_text("Ctrl+C"),
            )
            .on_hover_text("Then paste in File Explorer. Explorer shows its own progress")
            .clicked()
        {
            actions.push(Action::ExplorerCopy(targets.to_vec()));
            ui.close();
        }
    }

    fn file_list(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let (Some(tree), Some(folder)) = (&self.tree, &self.folder) else {
            ui.label("Select a folder in the tree.");
            return;
        };
        // Registered first, so the rows drawn later get their own clicks.
        let background = ui.interact(
            ui.max_rect(),
            ui.id().with("files-bg"),
            Sense::click_and_drag(),
        );
        let press = ui.input(|i| i.pointer.press_origin());
        let primary_down = ui.input(|i| i.pointer.primary_down());
        if self.band.is_some() && !primary_down {
            actions.push(Action::BandEnd);
        }
        // A drag on empty space starts a rubber band.
        let mut band_start = press.filter(|_| background.drag_started());
        background.context_menu(|ui| {
            if ui.button("Refresh").clicked() {
                actions.push(Action::Reload(folder.clone()));
                ui.close();
            }
            if ui.button("Select all").clicked() {
                actions.push(Action::SelectAll);
                ui.close();
            }
            if ui.button("Deselect all").clicked() {
                actions.push(Action::DeselectAll);
                ui.close();
            }
        });
        self.nav_bar(ui, folder, actions);
        if tree.children(folder).is_none() {
            ui.weak("loading...");
            return;
        }
        let rows = self.list_rows();
        let width = ui.available_width();
        let name_width = (width - SIZE_WIDTH - DATE_WIDTH - 24.0).max(120.0);
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let targets = self.rows.paths(&rows);
        let copy_key = ui.input(|i| {
            i.events.iter().any(|e| matches!(e, egui::Event::Copy))
                || (i.modifiers.command && i.key_pressed(egui::Key::C))
        });
        // With no highlighted row, `explorer_copy` takes the checked items.
        if copy_key && !ui.ctx().egui_wants_keyboard_input() {
            actions.push(Action::ExplorerCopy(targets.clone()));
        }
        if !ui.ctx().egui_wants_keyboard_input() {
            let (all, none) = ui.input(|i| {
                let a = i.modifiers.command && i.key_pressed(egui::Key::A);
                (
                    a && !i.modifiers.shift,
                    (a && i.modifiers.shift) || i.key_pressed(egui::Key::Escape),
                )
            });
            if all {
                actions.push(Action::SelectAll);
            } else if none {
                actions.push(Action::DeselectAll);
            }
        }
        if let [one] = targets.as_slice()
            && ui.input(|i| i.key_pressed(egui::Key::Enter))
            && !ui.ctx().egui_wants_keyboard_input()
        {
            actions.push(Action::Open(one.clone()));
        }
        let pointer = ui.input(|i| i.pointer.latest_pos());
        let moved = press
            .zip(pointer)
            .is_some_and(|(a, b)| a.distance(b) > DRAG_DISTANCE);
        let modifiers = ui.input(|i| i.modifiers);
        let ctrl = modifiers.command || modifiers.ctrl;
        let pitch = row_height + ui.spacing().item_spacing.y;
        // A scroll offset that the rubber band asked for in the last frame.
        let scroll_id = ui.id().with("files-band-scroll");
        let scroll_to = ui.data_mut(|d| d.remove_temp::<f32>(scroll_id));
        // Screen y of the top of row 0, from the first drawn row.
        let mut content_top = None;
        // Column widths stay in egui memory for the session.
        let mut table = TableBuilder::new(ui)
            .id_salt("files")
            .resizable(true)
            .sense(Sense::click_and_drag())
            .auto_shrink(false)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(name_width).at_least(120.0).clip(true))
            .column(Column::initial(SIZE_WIDTH).at_least(50.0).clip(true))
            .column(Column::remainder().at_least(80.0).clip(true));
        if let Some(y) = scroll_to {
            table = table.vertical_scroll_offset(y);
        }
        let (sort_key, descending) = self.sort;
        let output = table
            .header(row_height, |mut header| {
                for (label, key) in [
                    ("Name", SortKey::Name),
                    ("Size", SortKey::Size),
                    ("Modified", SortKey::Time),
                ] {
                    header.col(|ui| {
                        let arrow = match (key == sort_key, descending) {
                            (false, _) => "",
                            (true, false) => " ▲",
                            (true, true) => " ▼",
                        };
                        let text = RichText::new(format!("{label}{arrow}")).strong();
                        if ui
                            .add(Button::new(text).frame(false))
                            .on_hover_text("Sort by this column; click again to reverse")
                            .clicked()
                        {
                            actions.push(Action::SortBy(key));
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(row_height, rows.len(), |mut row| {
                    let index = row.index();
                    let path = &rows[index];
                    let Some(e) = tree.entry(path) else {
                        return;
                    };
                    let selected = self.rows.contains(path);
                    row.set_selected(selected);
                    let name = if e.is_folder {
                        format!("{}/", e.name)
                    } else {
                        e.name.clone()
                    };
                    row.col(|ui| {
                        ui.add(Label::new(name).truncate().selectable(false));
                    });
                    row.col(|ui| {
                        let size = match (e.is_folder, e.size) {
                            (false, Some(s)) => human_size(s),
                            _ => String::new(),
                        };
                        ui.add(Label::new(size).selectable(false));
                    });
                    row.col(|ui| {
                        let date = e.modified.map(short_time).unwrap_or_default();
                        ui.add(Label::new(date).truncate().selectable(false));
                    });
                    let r = row.response();
                    content_top.get_or_insert(r.rect.top() - index as f32 * pitch);
                    // A drag from a selected row drags the selection to
                    // Explorer; from another row it starts a rubber band.
                    if r.drag_started() && !selected {
                        band_start = band_start.or(press);
                    }
                    if r.dragged() && selected && moved && self.band.is_none() {
                        actions.push(Action::DragOut(targets.clone()));
                    }
                    if r.clicked() {
                        actions.push(Action::Click {
                            index,
                            ctrl,
                            shift: modifiers.shift,
                        });
                    }
                    if r.secondary_clicked() {
                        actions.push(Action::ContextClick(index));
                    }
                    if r.double_clicked() {
                        actions.push(Action::Open(path.clone()));
                    }
                    // The menu acts on the selection if this row is in it.
                    let menu_targets = if selected {
                        targets.clone()
                    } else {
                        vec![path.clone()]
                    };
                    r.context_menu(|ui| {
                        if e.is_folder {
                            self.folder_menu(ui, path, &menu_targets, actions);
                        } else {
                            self.file_menu(ui, path, &menu_targets, actions);
                        }
                    });
                });
            });
        let view = output.inner_rect;
        let content_top = content_top.unwrap_or(view.top() - output.state.offset.y);
        if let Some(p) = band_start {
            actions.push(Action::BandStart {
                x: p.x,
                y: p.y - content_top,
                add: ctrl,
            });
        }
        if let Some(band) = self.band.filter(|_| primary_down)
            && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        {
            let step = draw_band(ui, band, p, content_top, view);
            if step != 0.0 {
                let y = (output.state.offset.y - step).max(0.0);
                ui.data_mut(|d| d.insert_temp(scroll_id, y));
            }
            let rows = band_rows(band.y, p.y - content_top, pitch, rows.len());
            actions.push(Action::BandUpdate(rows));
        }
    }

    /// Back, Forward, Up and the clickable path, with their shortcuts.
    fn nav_bar(&self, ui: &mut egui::Ui, folder: &str, actions: &mut Vec<Action>) {
        let typing = ui.ctx().egui_wants_keyboard_input();
        let (back, forward, up) = ui.input(|i| {
            let alt = i.modifiers.alt;
            (
                (alt && i.key_pressed(egui::Key::ArrowLeft))
                    || i.pointer.button_pressed(egui::PointerButton::Extra1),
                (alt && i.key_pressed(egui::Key::ArrowRight))
                    || i.pointer.button_pressed(egui::PointerButton::Extra2),
                (alt && i.key_pressed(egui::Key::ArrowUp))
                    || (!typing && i.key_pressed(egui::Key::Backspace)),
            )
        });
        ui.horizontal(|ui| {
            let b = ui
                .add_enabled(self.nav.can_back(), Button::new("←"))
                .on_hover_text("Back (Alt+Left)");
            if (b.clicked() || back) && self.nav.can_back() {
                actions.push(Action::Back);
            }
            let f = ui
                .add_enabled(self.nav.can_forward(), Button::new("→"))
                .on_hover_text("Forward (Alt+Right)");
            if (f.clicked() || forward) && self.nav.can_forward() {
                actions.push(Action::Forward);
            }
            let u = ui
                .add_enabled(self.nav.can_up(), Button::new("↑"))
                .on_hover_text("Up (Alt+Up, Backspace)");
            if (u.clicked() || up) && self.nav.can_up() {
                actions.push(Action::Up);
            }
            ui.separator();
            let crumbs = breadcrumbs(folder);
            let last = crumbs.len() - 1;
            for (i, (label, path)) in crumbs.into_iter().enumerate() {
                if i > 1 {
                    ui.label("›");
                }
                let text = if i == last {
                    RichText::new(label).strong()
                } else {
                    RichText::new(label)
                };
                if ui.add(Button::new(text).frame(false)).clicked() {
                    actions.push(Action::ShowFolder(path));
                }
                if i == 0 && last > 0 {
                    ui.label("›");
                }
            }
        });
    }

    fn properties_window(&mut self, ctx: &egui::Context) {
        let Some(path) = self.properties.clone() else {
            return;
        };
        let Some(entry) = self.tree.as_ref().and_then(|t| t.entry(&path)).cloned() else {
            self.properties = None;
            return;
        };
        let mut rows: Vec<(&str, String)> = vec![
            ("Name", entry.name.clone()),
            ("Device path", entry.path.clone()),
        ];
        let text = |t: Option<String>| t.unwrap_or_else(|| "-".into());
        if entry.is_folder {
            let tree = self.tree.as_ref();
            let counts = tree.and_then(|t| t.children(&path)).map(|c| {
                let folders = c
                    .iter()
                    .filter(|p| tree.and_then(|t| t.entry(p)).is_some_and(|e| e.is_folder))
                    .count();
                (folders, c.len() - folders)
            });
            let (folders, files) = match counts {
                Some((d, f)) => (d.to_string(), f.to_string()),
                None => ("loading...".into(), "loading...".into()),
            };
            rows.push(("Folders", folders));
            rows.push(("Files", files));
        } else {
            rows.push((
                "Size",
                text(entry.size.map(|s| format!("{} ({s} bytes)", human_size(s)))),
            ));
            rows.push(("Modified", text(entry.modified.map(|t| t.to_string()))));
            rows.push(("Created", text(entry.created.map(|t| t.to_string()))));
            rows.push(("Content type", text(entry.content_type.clone())));
            let cached = if self.cached(&path).is_some() {
                "yes"
            } else {
                "no"
            };
            rows.push(("Cached", cached.into()));
        }
        let mut open = true;
        egui::Window::new("Properties")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("properties").num_columns(2).show(ui, |ui| {
                    for (k, v) in rows {
                        ui.strong(k);
                        ui.label(v);
                        ui.end_row();
                    }
                });
            });
        if !open {
            self.properties = None;
        }
    }

    fn bottom_panel(&self, ui: &mut egui::Ui) {
        if self.download.is_none()
            && let Some(line) = &self.download_final
        {
            ui.add(ProgressBar::new(1.0).text(line));
        }
        if let Some(d) = &self.download {
            let fraction = d
                .size
                .filter(|&s| s > 0)
                .map_or(0.0, |s| d.bytes as f32 / s as f32);
            ui.add(ProgressBar::new(fraction).text(format!(
                "Downloading {}  {} / {}  {}",
                file_name(&d.path),
                human_size(d.bytes),
                d.size.map(human_size).unwrap_or_else(|| "?".into()),
                self.download_text(d)
            )));
        }
        let now = Instant::now();
        let (file_fraction, file_text) = self.copy.file_line(now);
        ui.add(ProgressBar::new(file_fraction).text(file_text));
        match self.copy.scan_text() {
            Some(text) => ui.add(ProgressBar::new(0.0).animate(true).text(text)),
            None => ui.add(
                ProgressBar::new(self.copy.fraction())
                    .text(self.copy.overall_text(self.copying, now)),
            ),
        };
        ui.separator();
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        egui::ScrollArea::vertical()
            .id_salt("log")
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show_rows(ui, row_height, self.log.len(), |ui, rows| {
                for line in &self.log[rows] {
                    ui.add(Label::new(RichText::new(line).monospace()).truncate());
                }
            });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        self.finish_explorer_copy();
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_anyway && self.paste_busy()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_warning = true;
        }
        if !self.paste_busy() {
            self.close_warning = false;
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if std::mem::take(&mut self.release_pointer) {
            let (pos, modifiers) =
                ctx.input(|i| (i.pointer.latest_pos().unwrap_or_default(), i.modifiers));
            raw_input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
            });
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string(CLEAR_CACHE_KEY, self.clear_cache_on_exit.to_string());
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.clear_cache_on_exit {
            clear_cache_folder();
        }
        // A data object left on the clipboard would point to a dead process.
        if let Some(obj) = self.clipboard.take() {
            dataobject::clear_clipboard(&obj);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        let mut actions = Vec::new();
        egui::Panel::bottom("bottom")
            .resizable(true)
            .default_size(180.0)
            .show(ui, |ui| self.bottom_panel(ui));
        egui::Panel::left("tree")
            .resizable(true)
            .default_size(340.0)
            .show(ui, |ui| self.tree_panel(ui, &mut actions));
        egui::CentralPanel::default().show(ui, |ui| self.file_list(ui, &mut actions));
        let ctx = ui.ctx().clone();
        self.properties_window(&ctx);
        self.apply(actions);
        if self.copying || self.download.is_some() || self.paste_busy() || self.clip_wait.is_some()
        {
            // Keep speed and ETA moving between progress replies.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

/// Paint the rubber band over the list. Returns the scroll step when the
/// pointer is past the top or bottom of `view`.
fn draw_band(
    ui: &egui::Ui,
    band: Band,
    pointer: egui::Pos2,
    content_top: f32,
    view: egui::Rect,
) -> f32 {
    let rect = egui::Rect::from_two_pos(egui::pos2(band.x, band.y + content_top), pointer);
    let color = ui.visuals().selection.bg_fill;
    let painter = ui.painter().with_clip_rect(view);
    painter.rect_filled(rect, 0.0, color.gamma_multiply(0.25));
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    ui.ctx().request_repaint();
    if pointer.y > view.bottom() {
        -(pointer.y - view.bottom()).min(BAND_SCROLL_MAX)
    } else if pointer.y < view.top() {
        (view.top() - pointer.y).min(BAND_SCROLL_MAX)
    } else {
        0.0
    }
}

/// `YYYY-MM-DD HH:MM` of a device time.
fn short_time(t: LocalTime) -> String {
    let (y, mo, d, h, mi, _) = t.civil();
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
}

/// The id of a folder's open state in the tree. It depends only on the path,
/// so "Expand all" can set it for folders that are not drawn.
fn tree_id(path: &str) -> egui::Id {
    egui::Id::new(("tree", path))
}

fn set_open(ctx: &egui::Context, path: &str, open: bool) {
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ctx, tree_id(path), false);
    state.set_open(open);
    state.store(ctx);
}

fn check_box(ui: &mut egui::Ui, check: Check, path: &str, actions: &mut Vec<Action>) {
    let mut on = check == Check::Checked;
    let r = ui.add(Checkbox::without_text(&mut on).indeterminate(check == Check::Partial));
    if r.clicked() {
        actions.push(Action::Toggle(path.to_owned()));
    }
}

/// Folders first, then names without case.
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.is_folder
            .cmp(&a.is_folder)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn device_label(d: &DeviceInfo) -> String {
    format!(
        "[{}] {}",
        d.index,
        d.friendly_name.as_deref().unwrap_or("<no name>")
    )
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parent_path(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/",
        Some((parent, _)) => parent,
    }
}
