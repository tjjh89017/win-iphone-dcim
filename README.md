# win-iphone-dcim

Read-only Windows CLI that reads the iPhone DCIM tree over Windows Portable
Devices (WPD). It never writes to or deletes from the iPhone. Phase 0 is a
feasibility PoC; see `SPEC.md`.

Requirements: Windows 10/11 x64, an unlocked iPhone on USB, and "Trust" tapped
on the device.

## Usage

```powershell
win-iphone-dcim.exe devices
win-iphone-dcim.exe ls   [-d <index>] [-l] [-R] [--json] [PATH...]
win-iphone-dcim.exe tree [-d <index>] [-L <depth>] [--json] [PATH]
win-iphone-dcim.exe cp   [-d <index>] [--dry-run] SRC... DEST
```

- `devices` lists the WPD devices with index, name, manufacturer and description.
- `-d <index>` selects a device. It is optional when exactly one device is connected.
- Device paths start at the device root `/`, for example
  `"/Internal Storage/DCIM/202601_a"`. Each component matches the original file
  name first, then the object name.
- `ls` lists one level. `-l` adds type, size, modification date and object ID.
  `-R` recurses. `--json` prints one JSON object per line.
- `tree` prints a branch view. `-L` limits the depth. `--json` prints JSONL.
- `cp` copies single device files (folders and `-r` come in Phase 1). If DEST is
  an existing folder, each file goes into it with its original name. With one
  SRC, a DEST that does not exist is the new file name. With two or more SRC,
  DEST must be an existing folder. `cp` never overwrites a local file and
  compares the byte count with the size that the device reports.
- `--log-format text|json` sets the log format. Logs go to stderr. Set
  `RUST_LOG=debug` for more detail.

Examples:

```powershell
win-iphone-dcim.exe tree -L 3
win-iphone-dcim.exe ls -l "/Internal Storage/DCIM/202601_a"
win-iphone-dcim.exe cp "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC" D:\Test\
```

Use PowerShell or cmd. Git Bash (MSYS) rewrites arguments that start with `/`.

Exit codes: 0 success, 1 some files or paths failed, 2 command-line error,
3 device not found or cannot be opened, 4 internal error.

## Paths

Local paths stay `PathBuf`/`OsString`, and device file names are converted from
their UTF-16 form without loss; a device name that is not valid Unicode is
refused. `cp` makes DEST absolute and, on Windows, adds the verbatim prefix
(`\\?\D:\...` or `\\?\UNC\server\share\...`), so paths longer than 260
characters and UNC shares such as `\\server\share\Backup` work without the
LongPathsEnabled registry setting.

## Build

Do not run cargo on the host. Use the Docker wrapper:

```sh
scripts/dev.sh                     # release build: target/x86_64-pc-windows-msvc/release/win-iphone-dcim.exe
scripts/dev.sh test                # portable unit tests (Linux container)
scripts/dev.sh xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings
```

The `target` directory is a Docker volume. GitHub Actions on `windows-latest`
is the authoritative build.

## License

Licensed under the Apache License, Version 2.0. See LICENSE.
