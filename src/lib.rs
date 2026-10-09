//! win-iphone-dcim: read-only iPhone DCIM access over Windows Portable Devices.
//!
//! The library holds the device access, the copy engine and the GUI. The
//! binaries `win-iphone-dcim` (CLI and worker) and `win-iphone-dcim-gui`
//! are thin entry points.

// Off Windows the WPD backend is a stub, so the portable helpers that only
// it calls (HRESULT mapping, device selection, date and buffer helpers) look
// unused. They are still compiled and unit-tested there.
#![cfg_attr(not(windows), allow(dead_code))]

pub mod backup;
pub mod cli;
pub mod cmd;
pub mod device_fs;
pub mod devpath;
pub mod error;
pub mod gui;
pub mod ipc;
pub mod model;
pub mod paths;
pub mod progress;
pub mod supervisor;
pub mod wpd;
