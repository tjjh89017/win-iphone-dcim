//! The GUI (SPEC.md section 10, Phase 4): browse the device tree, check
//! folders and files, copy them with the copy engine, and open a file with
//! its default application from a local cache.
//!
//! Threads: the UI thread draws the window and never touches a `DeviceFs`.
//! The device thread in `device` owns the `DeviceFs` and answers requests
//! over channels.

pub mod cache;
pub mod chunks;
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
        app::run()
    }
    #[cfg(not(windows))]
    {
        eprintln!("win-iphone-dcim-gui runs on Windows only");
        ExitCode::from(crate::error::exit::INTERNAL as u8)
    }
}
