//! The GUI (SPEC.md section 10, Phase 4): browse the device tree, check
//! folders and files, copy them with the copy engine, and open a file with
//! its default application from a local cache.
//!
//! Threads: the UI thread draws the window and never touches a `DeviceFs`.
//! The device thread in `device` owns the `DeviceFs` and answers requests
//! over channels.

pub mod cache;
pub mod chunks;
pub mod copyview;
pub mod device;
pub mod filedesc;
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

/// Send logs to stderr. The GUI has no console, so redirect it to see
/// them: `set RUST_LOG=win_iphone_dcim=debug` and
/// `win-iphone-dcim-gui.exe 2> gui.log`.
#[cfg(windows)]
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
}
