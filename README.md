# win-iphone-dcim

`win-iphone-dcim` is a read-only Windows CLI. It copies photos and videos from
an iPhone over Windows Portable Devices (WPD). It keeps the folder structure
that File Explorer shows under `Internal Storage/DCIM`. It does not convert
images, videos or metadata. It never writes to or deletes from the iPhone.

## Status

The project is in Phase 0 (proof of concept). See [SPEC.md](SPEC.md) for the
full plan.

Works now:

- `devices`: list the WPD devices.
- `ls`: list folder contents.
- `tree`: show folders and files as a tree.
- `cp`: copy single device files.

Planned:

- `cp -r`: copy folders recursively.
- Incremental copy with a JSONL manifest in `DEST/.win-iphone-dcim/`.
- Worker process isolation, so a blocked WPD call cannot hang the tool.
- A GUI (Phase 4, optional).

## Download

Open the [main workflow runs](https://github.com/tjjh89017/win-iphone-dcim/actions/workflows/main.yaml)
and select a green run. Every run on `main` uploads two artifacts:

- `win-iphone-dcim-windows-x64`
- `win-iphone-dcim-windows-arm64`

You must log in to GitHub to download artifacts. The ARM64 binary is built but
not tested on hardware.

With the GitHub CLI:

```sh
gh run download <run-id> -n win-iphone-dcim-windows-x64
```

## Requirements

- Windows 10 or 11.
- An iPhone connected by USB.
- The iPhone is unlocked, and you tapped "Trust This Computer".

Recommended: on the iPhone, set Settings > Photos > Transfer to Mac or PC to
**Keep Originals**. If iCloud Photos "Optimize iPhone Storage" is on, some
originals are not on the device. The tool cannot copy those files.

## Usage

```powershell
win-iphone-dcim.exe devices
win-iphone-dcim.exe ls   [-d <INDEX>] [-l] [-R] [--json] [PATH...]
win-iphone-dcim.exe tree [-d <INDEX>] [-L <DEPTH>] [--json] [PATH]
win-iphone-dcim.exe cp   [-d <INDEX>] [--dry-run] SRC... DEST
```

Global flags:

- `-d, --device <INDEX>` selects a device by the index from `devices`. You can
  omit it when exactly one device is connected.
- `--log-format text|json` sets the log format. The default is `text`. Logs go
  to stderr. Results go to stdout. Set `RUST_LOG=debug` for more detail.

Commands:

- `devices` lists the WPD devices with index, name, manufacturer and description.
- `ls` lists one level. The default path is `/`. `-l` adds type, size,
  modification date and object ID. `-R` recurses. `--json` prints one JSON
  object per line (JSONL).
- `tree` prints a branch view. The default path is `/`. `-L <DEPTH>` limits
  the depth. `--json` prints JSONL.
- `cp` copies device files to a local path. If DEST is an existing folder,
  each file goes into it with its original name. With one SRC, a DEST that
  does not exist is the new file name. With two or more SRC, DEST must be an
  existing folder. `cp` never overwrites a local file. It compares the byte
  count with the size that the device reports. `--dry-run` prints the copy
  plan only.

Device paths:

- A device path starts at the device root `/`, for example
  `/Internal Storage/DCIM/202601_a`.
- Each component matches the original file name first, then the object name.
- Quote a path that contains spaces.
- Use PowerShell or cmd. Git Bash (MSYS) rewrites arguments that start with `/`.
- Planned for `cp -r`: the trailing slash rule of `rsync`.
  `cp -r "/Internal Storage/DCIM" D:\Backup` will create `D:\Backup\DCIM\...`.
  `cp -r "/Internal Storage/DCIM/" D:\Backup` will copy the contents of `DCIM`
  into `D:\Backup\...`. The parser already keeps the trailing slash.

Examples:

```powershell
win-iphone-dcim.exe devices
win-iphone-dcim.exe tree -L 3
win-iphone-dcim.exe ls -l "/Internal Storage/DCIM/202601_a"
win-iphone-dcim.exe ls -R --json "/Internal Storage/DCIM"
win-iphone-dcim.exe cp --dry-run "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC" D:\Test\
win-iphone-dcim.exe cp "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC" D:\Test\
```

Example `tree` output:

```text
/
└── Internal Storage
    └── DCIM
        ├── 202601_a
        │   ├── IMG_0001.HEIC  3.7 MiB
        │   └── IMG_0002.MOV   48.1 MiB
        └── 202601_b
            └── IMG_0003.DNG   21.0 MiB
```

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Some files or paths failed (not found, size mismatch, local file exists, I/O error) |
| 2 | Command-line error (bad arguments, more than one device and no `-d`, DEST is not a folder) |
| 3 | Device not found, access denied, or the device cannot be opened |
| 4 | Internal error (unexpected WPD error, unsupported platform) |

## Paths

Local paths stay `PathBuf`/`OsString`, and device file names are converted from
their UTF-16 form without loss; a device name that is not valid Unicode is
refused. `cp` makes DEST absolute and, on Windows, adds the verbatim prefix
(`\\?\D:\...` or `\\?\UNC\server\share\...`), so paths longer than 260
characters and UNC shares such as `\\server\share\Backup` (for example a Samba
share) work without the LongPathsEnabled registry setting.

## Build

Do not run cargo on the host for this repo. Use the Docker wrapper
`scripts/dev.sh`. It runs cargo with cargo-xwin inside the dev container.

```sh
scripts/dev.sh                     # x64 release build
scripts/dev.sh xwin build --release --target aarch64-pc-windows-msvc   # ARM64 release build
scripts/dev.sh test                # portable unit tests (Linux container)
scripts/dev.sh xwin check --target x86_64-pc-windows-msvc
scripts/dev.sh xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings
```

The binaries go to `target/<target>/release/win-iphone-dcim.exe`. The `target`
directory is a Docker volume. GitHub Actions on `windows-latest` is the
authoritative build. Tests that need WPD or a real iPhone are manual. Run them
on Windows with an iPhone attached.

## Design notes

- The tool calls the WPD COM API directly. It does not use the Windows Shell
  namespace or the Explorer copy APIs.
- COM objects never cross threads. Each COM apartment owns its own handles.
- All WPD code is in `src/wpd/` under `#[cfg(windows)]`. On other platforms,
  WPD calls return an "unsupported platform" error.
- The commands use the `DeviceFs` trait. Unit tests use the in-memory fake in
  `device_fs::fake`, so CI does not need an iPhone.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
