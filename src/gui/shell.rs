//! Open a local file with its default Windows application.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::{SE_ERR_NOASSOC, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

/// The hint for a file type without an application.
pub const NO_ASSOCIATION_HINT: &str = "no application is associated with this file type. \
     For HEIC photos and HEVC videos, install the \"HEIF Image Extensions\" and \
     \"HEVC Video Extensions\" from the Microsoft Store";

/// `ShellExecuteW` with the `open` verb. Call it on a thread that may block:
/// the shell can take a moment to start the application.
pub fn open(path: &Path) -> Result<(), String> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: COM is initialized and uninitialized on this thread only.
    // `wide` is NUL-terminated and outlives the call.
    let code = unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let result = ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if init.is_ok() {
            CoUninitialize();
        }
        result.0 as isize
    };
    // A value above 32 means success.
    match code {
        c if c > 32 => Ok(()),
        c if c == SE_ERR_NOASSOC as isize => Err(NO_ASSOCIATION_HINT.into()),
        c => Err(format!("ShellExecuteW failed with code {c}")),
    }
}

/// Show `path` selected in a File Explorer window.
pub fn reveal(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot start explorer.exe: {e}"))
}
