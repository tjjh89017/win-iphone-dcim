//! win-iphone-dcim-gui: the window to browse, open and copy device files.
//! It starts `win-iphone-dcim.exe` from its own folder as the device worker.

// No console window.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    win_iphone_dcim::gui::main()
}
