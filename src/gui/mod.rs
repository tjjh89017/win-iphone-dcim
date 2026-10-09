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

use std::process::ExitCode;

/// The entry point of `win-iphone-dcim-gui`.
pub fn main() -> ExitCode {
    #[cfg(windows)]
    {
        init_logging();
        app::run()
    }
    #[cfg(not(windows))]
    {
        eprintln!("win-iphone-dcim-gui runs on Windows only");
        ExitCode::from(crate::error::exit::INTERNAL as u8)
    }
}

/// A file path. When set, the GUI and its worker append their logs to it.
#[cfg(windows)]
const LOG_FILE_ENV: &str = "WIN_IPHONE_DCIM_LOG_FILE";

/// Send logs to stderr. The GUI has no console, and cmd does not pass
/// `2> file` to a GUI program, so set `WIN_IPHONE_DCIM_LOG_FILE` to see
/// them. The file becomes the process stderr, and the worker inherits it.
/// Without a valid stderr, no subscriber is installed and logs cost nothing.
#[cfg(windows)]
fn init_logging() {
    use std::os::windows::io::{AsRawHandle, IntoRawHandle};
    use tracing_subscriber::EnvFilter;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};

    if let Some(path) = std::env::var_os(LOG_FILE_ENV) {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path);
        if let Ok(file) = file {
            // SAFETY: the handle is open and stays open for the life of the
            // process, because the File gives up ownership of it.
            let _ = unsafe { SetStdHandle(STD_ERROR_HANDLE, HANDLE(file.into_raw_handle())) };
        }
    }
    if HANDLE(std::io::stderr().as_raw_handle()).is_invalid() {
        return;
    }
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
}
