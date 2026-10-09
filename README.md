# win-iphone-dcim

`win-iphone-dcim` is a read-only Windows CLI. It copies photos and videos from
an iPhone over Windows Portable Devices (WPD). It keeps the folder structure
that File Explorer shows under `Internal Storage/DCIM`. It does not convert
images, videos or metadata. It never writes to or deletes from the iPhone.

## Status

Phase 1 (full backup MVP) is complete. Phase 2 (incremental copy and
reliability) is in progress. See [SPEC.md](SPEC.md) for the full plan.

Works now:

- `devices`: list the WPD devices.
- `ls`: list folder contents.
- `tree`: show folders and files as a tree.
- `cp`: copy files, and folders with `-r`, with the `cp`/`rsync` trailing
  slash rules. Each file goes to a `.part` file first and gets its final name
  only after a size check. Progress bar, `-p`/`-a` timestamps, `-f`
  overwrite, `--dry-run`, and an error summary.
- Incremental copy with a JSONL manifest in `DEST/.win-iphone-dcim/`. A
  second `cp` of the same tree skips the verified files.
- Retries with backoff for transient errors (`--retries`).
- `--verify local-hash`: a BLAKE3 hash of each new copy.
- `verify`: check DEST against the manifest.

Planned:

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
win-iphone-dcim.exe cp   [-d <INDEX>] [-r] [-n | -f] [-p] [-a] [--dry-run] [--verify size|local-hash] SRC... DEST
win-iphone-dcim.exe verify [--hash] DEST
```

Global flags:

- `-d, --device <INDEX>` selects a device by the index from `devices`. You can
  omit it when exactly one device is connected.
- `--log-format text|json` sets the log format. The default is `text`. Logs go
  to stderr. Results go to stdout. Set `RUST_LOG=debug` for more detail.
- `--retries <N>` sets the number of additional attempts for a file after a
  transient error. The default is 3. See [Retries](#retries).
- `--diagnostic` also logs the raw device ID. Without it, logs show only the
  first 8 hex characters of its BLAKE3 hash.

Commands:

- `devices` lists the WPD devices with index, name, manufacturer and description.
- `ls` lists one level. The default path is `/`. `-l` adds type, size,
  modification date and object ID. `-R` recurses. `--json` prints one JSON
  object per line (JSONL).
- `tree` prints a branch view. The default path is `/`. `-L <DEPTH>` limits
  the depth. `--json` prints JSONL.
- `cp` copies device files and folders to a local path. It never writes to
  the device.
  - If DEST is an existing folder, each SRC goes into it with its original
    name. With one SRC, a DEST that does not exist is the new file name, or
    for a folder SRC the new folder that gets the contents.
  - With two or more SRC, DEST must be an existing folder.
  - A folder SRC needs `-r`.
  - The trailing slash rule of `rsync`:
    `cp -r "/Internal Storage/DCIM" D:\Backup` creates `D:\Backup\DCIM\...`.
    `cp -r "/Internal Storage/DCIM/" D:\Backup` copies the contents of `DCIM`
    into `D:\Backup\...`.
  - The tool lists one device folder at a time and copies one file at a time.
  - Each file goes to `<name>.<random>.part` in the target folder. The tool
    checks the byte count against the device size, then renames the file
    with a no-clobber rename. If the device gives no size, the line shows
    `size-unavailable`. On a failure the `.part` file is removed. A `.part`
    file from an earlier run is never treated as a complete file. The tool
    logs it and leaves it in place.
  - Every copied file gets a record in the manifest. An existing target
    file follows the [incremental rules](#manifest-and-incremental-copy):
    a verified file is skipped (`[skip] <path>  verified`). Any other
    existing file is skipped with a warning
    (`[skip] <path>  conflict: ... (use --force to replace)`). A skip is not a
    failure.
  - `-f, --force` replaces an existing target file that is not verified. The
    new data goes to a `.part` file first. The target is replaced atomically
    only after the size check. The tool logs `[overwrite] <path>`.
  - `-n, --no-clobber` skips existing target files that are not verified
    silently. It conflicts with `-f`.
  - `-p` sets the local modified time (and the created time on Windows)
    from the device dates after the copy. `-a` is `-r -p`. Timestamps never
    change a skip or copy decision.
  - Device names that Windows cannot hold (reserved names such as `CON`,
    `<>:"/\|?*`, control characters, a trailing dot or space) and names in
    one folder that differ only by case are errors. The tool never renames a
    file.
  - `--dry-run` prints the decision for every file (`[plan]`, `[skip]`,
    `[error]`) and writes nothing, not even the manifest.
  - `--verify size` is the default. `--verify local-hash` hashes each new
    copy with BLAKE3 while the bytes go to disk (no second read) and stores
    the hash. Before a skip it hashes the local file again and skips only if
    the hash matches.
  - On a terminal, stderr shows an overall line (files, bytes, elapsed,
    average speed) and a bar for the current file (bytes, percent, MiB/s,
    ETA). The totals grow while folders are listed. Without a terminal, or
    with `--log-format json`, progress goes to the log as events.
  - At the end, `cp` prints
    `[done] copied=N skipped=N exists=N failed=N` with the total bytes and
    the average speed, then the failures grouped by category.
- `verify` checks the files in DEST against the manifest. See
  [verify](#verify).

Device paths:

- A device path starts at the device root `/`, for example
  `/Internal Storage/DCIM/202601_a`.
- Each component matches the original file name first, then the object name.
- Quote a path that contains spaces.
- Use PowerShell or cmd. Git Bash (MSYS) rewrites arguments that start with `/`.

Examples:

```powershell
win-iphone-dcim.exe devices
win-iphone-dcim.exe tree -L 3
win-iphone-dcim.exe ls -l "/Internal Storage/DCIM/202601_a"
win-iphone-dcim.exe ls -R --json "/Internal Storage/DCIM"
win-iphone-dcim.exe cp --dry-run "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC" D:\Test\
win-iphone-dcim.exe cp "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC" D:\Test\
win-iphone-dcim.exe cp -a "/Internal Storage/DCIM" D:\iPhoneBackup
win-iphone-dcim.exe cp -r --dry-run "/Internal Storage/DCIM/" D:\iPhoneBackup\DCIM
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

## Manifest and incremental copy

`cp` writes `DEST/.win-iphone-dcim/manifest.jsonl`. DEST is the copy root that
you give to `cp`: the DEST folder, or the parent folder when DEST is a new
file name. The tool creates the folder on the first write.

Each committed file appends one JSON line:

```json
{"v":1,"device":"3f2a9c01d4e5b677","path":"DCIM/202601_a/IMG_0001.HEIC","source":"/Internal Storage/DCIM/202601_a/IMG_0001.HEIC","size":3879731,"modified":"2026-01-03 10:20:30","verification":"local-hash","hash_alg":"blake3","hash":"…","committed_at":"2026-10-09T08:00:00Z"}
```

- `device` is a hash of the device ID. The raw device ID is never stored.
- `path` is the path under DEST with `/` separators.
- `verification` is `size` (the byte count matches the device size),
  `local-hash` (the size matches and a hash is stored), or `size-unavailable`
  (the device gave no size, so the copy is not verified).
- The hash proves only that the local file did not change. It does not prove
  that the local file equals the original on the iPhone.
- Each line is flushed and synced. If the tool stops while it writes a line,
  the next run skips that incomplete line, logs it, and continues. A later
  record for the same path replaces an earlier one.

Before a run, the tool compares each record with the local file. A missing
file or a different size makes the record stale. Then, for each file:

| Situation | Default | With `-f` |
| --- | --- | --- |
| No local file | Copy, then record | Same |
| Record, local file and device size match | Skip as `verified` | Same |
| Same, with `--verify local-hash` and a stored hash | Skip only if the hash matches, otherwise conflict | Same |
| Local file with a different size, or a stale record, or a record from another device or source | Conflict: warn, skip, count in `exists` | Replace, then record |
| No record, local file with the same size | `unverified-existing`: warn, skip, count in `exists`, no record | Replace, then record |

Timestamps never change a decision. The tool never adopts an existing file
without a record. Use `-f` to copy it again.

## Retries

A failed file is retried on its own. Only transient errors are retried: the
device is busy or unavailable, an I/O call timed out, a network (SMB) write
failed, or the device worker was restarted. Not found, unsafe names, case
collisions, a full disk and access denied fail at once.

- `--retries <N>` additional attempts. The default is 3.
- The wait before each retry is 1 s, 3 s, 10 s, then 10 s.
- The `.part` file of the failed attempt is removed before the next attempt.
  Each attempt starts again from the first byte.
- Each retry prints `[retry k/N] <path>  <reason>`.
- If the device is still unavailable after the last retry, `cp` prints the
  summary and stops with exit code 3.

## verify

```powershell
win-iphone-dcim.exe verify D:\iPhoneBackup
win-iphone-dcim.exe verify --hash D:\iPhoneBackup
```

`verify` reads the manifest in DEST and checks that each recorded file exists
with the recorded size. `--hash` also recomputes the BLAKE3 hash where the
manifest has one. Files under DEST without a record are `unrecorded`. The
`.win-iphone-dcim` folder and `*.part` files are ignored. `verify` never
opens the device.

It prints one line per file (`[ok]`, `[missing]`, `[size-mismatch]`,
`[hash-mismatch]`, `[unrecorded]`) and a summary:

```text
[verify] ok=142 missing=0 size-mismatch=0 hash-mismatch=0 unrecorded=3
```

The exit code is 0 if all files are ok, 1 otherwise.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | Some files or paths failed (not found, unsafe name, case collision, size mismatch, I/O error), or `verify` found a problem. A skipped existing file is not a failure |
| 2 | Command-line error (bad arguments, more than one device and no `-d`, DEST is not a folder) |
| 3 | Device not found, access denied, or the device cannot be opened |
| 4 | Internal error (unexpected WPD error, device worker failure, unsupported platform) |

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
