# AGENTS.md

## Project

A Rust CLI for Windows 10/11 x64 that copies photos and videos from an iPhone over WPD. See SPEC.md.

## Build rules

- Never run cargo on the host.
- Use `scripts/dev.sh` for local builds and checks. It runs cargo inside Docker.
- `scripts/dev.sh` with no args cross-builds the release binary for `x86_64-pc-windows-msvc`.
- GitHub Actions on `windows-latest` is the authoritative build.
- This development machine runs Linux and has no iPhone attached.
- Tests that need WPD or a device are manual. Run them on a Windows machine with an iPhone.

## Test strategy

- Portable modules (`cli`, `model`, `devpath`, `device_fs`, `paths`, `error`, `cmd/*`) do not import `windows`. Their unit tests run on Linux in Docker with `scripts/dev.sh test`.
- Command logic runs against the `DeviceFs` trait. Tests use the in-memory fake in `device_fs::fake`.
- All WPD code is under `#[cfg(windows)]` in `src/wpd/`. On other platforms `wpd` returns an "unsupported platform" error.
- Check the Windows code with `scripts/dev.sh xwin check --target x86_64-pc-windows-msvc` and `scripts/dev.sh xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`.
- Device tests are manual. Run them on Windows with an iPhone attached.

## Code

- Use Rust stable.
- `cargo fmt` must pass.
- `cargo clippy -D warnings` must pass.

## Git

- Do not make merge commits.
- Use rebase or cherry-pick.
