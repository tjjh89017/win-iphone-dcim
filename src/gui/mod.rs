//! The GUI: browse the device tree, check
//! folders and files, copy them with the copy engine, and open a file with
//! its default application from a local cache.
//!
//! Threads: the UI thread draws the window and never touches a `DeviceFs`.
//! The device thread in `device` owns the `DeviceFs` and answers requests
//! over channels.

pub mod cache;
pub mod chunks;
pub mod config;
pub mod copyview;
pub mod device;
pub mod filedesc;
pub mod menu;
pub mod nav;
pub mod selection;

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod dataobject;
#[cfg(windows)]
mod dnd;
#[cfg(windows)]
mod shell;

use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

use tracing_subscriber::EnvFilter;

/// The entry point of `win-iphone-dcim-gui`.
pub fn main() -> ExitCode {
    #[cfg(windows)]
    {
        init_logging(log_file_choice());
        app::run()
    }
    #[cfg(not(windows))]
    {
        eprintln!("win-iphone-dcim-gui runs on Windows only");
        ExitCode::from(crate::error::exit::INTERNAL as u8)
    }
}

/// A file path. When set, the GUI and its worker append their logs to it.
pub const LOG_FILE_ENV: &str = "WIN_IPHONE_DCIM_LOG_FILE";
/// The GUI argument that names the log file. It beats `LOG_FILE_ENV`.
pub const LOG_FILE_ARG: &str = "--log-file";
/// The filter when a log file is given and `RUST_LOG` is not set.
pub const LOG_FILE_FILTER: &str = "win_iphone_dcim=debug";
/// The filter without a log file, and when `RUST_LOG` does not parse.
pub const DEFAULT_FILTER: &str = "info";

static LOGGING_ERROR: OnceLock<String> = OnceLock::new();

/// Why the log file could not be used, if it could not. The status bar
/// shows it once.
pub fn logging_error() -> Option<String> {
    LOGGING_ERROR.get().cloned()
}

#[cfg(windows)]
fn set_logging_error(message: String) {
    let _ = LOGGING_ERROR.set(message);
}

/// The value of `--log-file <path>` or `--log-file=<path>` in `args`
/// (without the program name). The last one wins. Other arguments are
/// ignored. A relative path resolves against `cwd`.
pub fn log_file_arg(
    args: impl IntoIterator<Item = OsString>,
    cwd: &Path,
) -> Result<Option<PathBuf>, String> {
    let prefix = format!("{LOG_FILE_ARG}=");
    let mut found = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let value = if arg == LOG_FILE_ARG {
            match args.next() {
                Some(v) => v,
                None => return Err(format!("{LOG_FILE_ARG} needs a file name")),
            }
        } else if let Some(v) = arg.to_str().and_then(|a| a.strip_prefix(&prefix)) {
            OsString::from(v)
        } else {
            continue;
        };
        if value.is_empty() {
            return Err(format!("{LOG_FILE_ARG} needs a file name"));
        }
        found = Some(cwd.join(value));
    }
    Ok(found)
}

/// The log file to use: the argument, then `LOG_FILE_ENV` (relative to
/// `cwd`), then the `log_file` config key. `None` means no log file.
pub fn pick_log_file(
    arg: Option<PathBuf>,
    env: Option<OsString>,
    config: Option<PathBuf>,
    cwd: &Path,
) -> Option<PathBuf> {
    let env = env
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(|v| cwd.join(v));
    arg.or(env).or(config)
}

/// Open `path` to append to it. Create the file if it is missing.
pub fn open_log_file(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// The filter for `RUST_LOG` (`rust_log`). Without it, a log file gives
/// `LOG_FILE_FILTER` and no file gives `DEFAULT_FILTER`. A value that does
/// not parse gives `DEFAULT_FILTER` and a warning to log.
pub fn log_filter(rust_log: Option<&str>, has_file: bool) -> (EnvFilter, Option<String>) {
    match rust_log.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => match EnvFilter::builder().parse(value) {
            Ok(filter) => (filter, None),
            Err(e) => (
                EnvFilter::new(DEFAULT_FILTER),
                Some(format!(
                    "RUST_LOG={value:?} is not valid ({e}); using {DEFAULT_FILTER}"
                )),
            ),
        },
        None if has_file => (EnvFilter::new(LOG_FILE_FILTER), None),
        None => (EnvFilter::new(DEFAULT_FILTER), None),
    }
}

/// The first log lines. An empty log file means no subscriber was installed.
pub fn log_started(file: Option<&Path>, warning: Option<&str>) {
    let file = file.map_or_else(|| "none".to_owned(), |p| p.display().to_string());
    tracing::info!("log started: {}, file {file}", env!("CARGO_PKG_VERSION"));
    if let Some(w) = warning {
        tracing::warn!("{w}");
    }
}

/// The log file named by `--log-file`, `LOG_FILE_ENV` or the config file.
#[cfg(windows)]
fn log_file_choice() -> Option<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let arg = log_file_arg(std::env::args_os().skip(1), &cwd).unwrap_or_else(|e| {
        set_logging_error(e);
        None
    });
    let config = config::Config::load(config::exe_dir().as_deref()).log_file;
    pick_log_file(arg, std::env::var_os(LOG_FILE_ENV), config, &cwd)
}

/// Send logs to stderr. The GUI has no console, and a GUI program gets no
/// stderr from cmd's `2> file`, so `path` (the log file) becomes the process
/// stderr. The worker inherits it: `Stdio::inherit` duplicates the current
/// `GetStdHandle` value as an inheritable handle at spawn time.
/// Without a log file or a valid stderr, no subscriber is installed.
/// Call this first, before any other thread starts.
#[cfg(windows)]
fn init_logging(path: Option<PathBuf>) {
    use std::os::windows::io::{AsRawHandle, IntoRawHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};

    let mut file = None;
    if let Some(path) = path {
        match open_log_file(&path) {
            Ok(f) => {
                // SAFETY: the handle is open and stays open for the life of
                // the process, because the File gives up ownership of it.
                match unsafe { SetStdHandle(STD_ERROR_HANDLE, HANDLE(f.into_raw_handle())) } {
                    Ok(()) => file = Some(path),
                    Err(e) => set_logging_error(format!("log file {}: {e}", path.display())),
                }
            }
            Err(e) => set_logging_error(format!("log file {}: {e}", path.display())),
        }
    }
    if HANDLE(std::io::stderr().as_raw_handle()).is_invalid() {
        return;
    }
    let rust_log = std::env::var("RUST_LOG").ok();
    let (filter, warning) = log_filter(rust_log.as_deref(), file.is_some());
    if file.is_some() && rust_log.is_none() {
        // SAFETY: no other thread runs yet. The worker inherits the level.
        unsafe { std::env::set_var("RUST_LOG", LOG_FILE_FILTER) };
    }
    let installed = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init()
        .is_ok();
    if installed {
        log_started(file.as_deref(), warning.as_deref());
    }
}

#[cfg(test)]
mod tests;
