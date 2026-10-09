# AGENTS.md

## Project

A Rust CLI and GUI for Windows 10/11 x64 that copies photos and videos from an iPhone over WPD. See SPEC.md.

- `src/lib.rs` holds the modules. `src/main.rs` is the CLI. `src/bin/gui.rs` is the GUI (`win-iphone-dcim-gui`).

## Build rules

- Never run cargo on the host.
- Use `scripts/dev.sh` for local builds and checks. It runs cargo inside Docker.
- `scripts/dev.sh` with no args cross-builds the release binary for `x86_64-pc-windows-msvc`.
- GitHub Actions on `windows-latest` is the authoritative build.
- This development machine runs Linux and has no iPhone attached.
- Tests that need WPD or a device are manual. Run them on a Windows machine with an iPhone.

## Test strategy

- Portable modules (`cli`, `model`, `devpath`, `device_fs`, `paths`, `error`, `progress`, `ipc`, `supervisor`, `cmd/*`, `backup/*`, `gui/selection`, `gui/device`, `gui/cache`, `gui/config`, `gui/filedesc`, `gui/chunks`, `gui/nav`, `gui/menu`) do not import `windows`, except the `cfg(windows)` file API calls in `backup/transfer.rs` and the `cfg(windows)` `SetHandleInformation` call in `supervisor`. Their unit tests run on Linux in Docker with `scripts/dev.sh test`.
- Unit tests live next to their module in `<module>/tests.rs`, declared with `#[cfg(test)] mod tests;`.
- Command logic runs against the `DeviceFs` trait. Tests use the in-memory fake in `device_fs::fake`.
- The fake is compiled only in unit tests and with the `fake-device` cargo feature. Release builds do not enable the feature. Gate fake-only code with `#[cfg(any(test, feature = "fake-device"))]`.
- Run all tests, end-to-end included, with `scripts/dev.sh test --features fake-device`. Without the feature, cargo skips `tests/e2e.rs`.
- Run clippy with and without the feature: `scripts/dev.sh clippy --all-targets -- -D warnings` and `scripts/dev.sh clippy --all-targets --features fake-device -- -D warnings`.
- All WPD code is under `#[cfg(windows)]` in `src/wpd/`. On other platforms `wpd` returns an "unsupported platform" error.
- Check the Windows code with `scripts/dev.sh xwin check --target x86_64-pc-windows-msvc` and `scripts/dev.sh xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`.
- Device tests are manual. Run them on Windows with an iPhone attached.

## Worker and IPC

- The worker process (`win-iphone-dcim worker`) owns all WPD COM objects. The parent talks to it through `ipc`.
- The worker's stdout carries the protocol only. Never print anything else to it.
- Logs go to stderr, in the parent and in the worker.
- Never pass COM pointers between processes. Send device paths and object ids instead.
- End-to-end tests in `tests/e2e.rs` run the built program with `WIN_IPHONE_DCIM_FAKE_FS=1`. The worker then serves the in-memory fake device. The program must be built with the `fake-device` feature. Without it, the variable has no effect.

## GUI

- The UI thread never touches a `DeviceFs`. The device thread in `gui/device.rs` owns it and answers requests over channels.
- The UI thread never waits for the device thread. It reads replies with `try_recv`.
- `gui/app.rs` and `gui/shell.rs` are Windows only (`#[cfg(windows)]`). eframe, egui_extras and rfd are Windows-only dependencies.
- Test the GUI logic (selection, cache paths, device thread) on Linux with the fake device. egui drawing has no unit tests.
- The GUI starts `win-iphone-dcim.exe` from its own folder as the worker.
- The GUI UI thread is an OLE STA apartment (`OleInitialize`). The OLE clipboard, `DoDragDrop` and the `IDataObject` live on it. Never call `CoInitializeEx` with MTA on the UI thread.
- The device thread never touches OLE or COM. Paste streams (`IStream`) live in the process MTA and get their bytes from the device thread through the bounded channel in `gui/chunks`.
- The file list sorts with `gui/selection::sort_rows`, which reuses the `cmd/sort` name and value order.
- `gui/filedesc` and `gui/chunks` are portable and have unit tests. `gui/dataobject` and `gui/dnd` are Windows only and are checked by xwin clippy; test the Explorer interaction by hand on Windows.

## Code

- Use Rust stable.
- `cargo fmt` must pass.
- `cargo clippy -D warnings` must pass.

## Git

- Do not make merge commits.
- Use rebase or cherry-pick.
