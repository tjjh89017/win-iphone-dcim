# win-iphone-dcim

`win-iphone-dcim` is a read-only Windows CLI with an optional GUI. It copies photos and videos from
an iPhone over Windows Portable Devices (WPD). It keeps the folder structure
that File Explorer shows under `Internal Storage/DCIM`. It does not convert
images, videos or metadata. It never writes to or deletes from the iPhone.

## Status

Phase 1, 2 and 3 complete. Phase 4 (GUI) in progress: browsing, double-click
open, in-app copy, and Explorer paste and drag-and-drop work. See [SPEC.md](SPEC.md) for the
full plan. Release v0.1.0 is published.

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
- Worker process isolation with a watchdog (`--timeout`), so a blocked WPD
  call cannot hang the tool. See [Worker process](#worker-process).

- `win-iphone-dcim-gui.exe`: a window to browse the device, open a file with
  its default application, copy the checked folders and files, and paste or
  drag and drop them into File Explorer. See [GUI](#gui).

### Verification scope

Status on real hardware as of 2026-10-09: the GUI starts on Windows x64 and
lists a connected iPhone. The worker console window bug was found on real
hardware and fixed. Nothing else is verified on real hardware: no WPD copy,
no Explorer paste, no folder-name check (SPEC section 2), and no ARM64 test.
All automated tests use the in-memory fake device, on Linux and on
`windows-latest`. Use the [checklist](#testing-on-a-real-iphone) below for
the next real-device run.

## Download

Open the [Releases page](https://github.com/tjjh89017/win-iphone-dcim/releases/tag/v0.1.0)
and download the zip for your CPU, with `SHA256SUMS.txt` to check it:

- `win-iphone-dcim-windows-x64.zip`
- `win-iphone-dcim-windows-arm64.zip`
- `SHA256SUMS.txt`

Each zip holds `win-iphone-dcim.exe` (CLI), `win-iphone-dcim-gui.exe` (GUI),
`LICENSE` and `README.md`. Extract both programs into one folder. The GUI
needs the CLI next to it (see [GUI](#gui)).

For the latest `main` build, open the
[main workflow runs](https://github.com/tjjh89017/win-iphone-dcim/actions/workflows/main.yaml)
and select a green run. Every run on `main` uploads two artifacts:

- `win-iphone-dcim-windows-x64`
- `win-iphone-dcim-windows-arm64`

Each artifact holds the two exe files. You must log in to GitHub to download
artifacts. Artifacts expire after 7 days. The ARM64 binary is built but not
tested on hardware.

With the GitHub CLI:

```sh
gh run download <run-id> -n win-iphone-dcim-windows-x64
```

A pushed `v*` tag builds a draft GitHub release with the same zip files and
`SHA256SUMS.txt`.

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
- `--timeout <DURATION>` sets the worker watchdog. The default is `120s`. The
  value is whole seconds (`90`) or a number with the unit `s`, `m` or `h`
  (`90s`, `2m`). See [Worker process](#worker-process).
- `--no-isolate` runs the WPD calls in the main process instead of a worker
  process. A hung WPD call then hangs the tool. Use it only for debugging.

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
    current and average speed, ETA once the listing is done) and a bar for
    the current file (bytes, percent, current MiB/s over the last 3 s, ETA).
    The totals grow while folders are listed. Without a terminal, or
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

## GUI

Start `win-iphone-dcim-gui.exe`. No console window opens.

The GUI needs `win-iphone-dcim.exe` in the same folder: it starts that file
as its device worker, as the CLI does. If the file is missing, the GUI shows
an error and cannot open a device.

The window:

- Top bar: the device list, Refresh, the destination folder (Destination...),
  "Overwrite existing (--force)", "Copy to folder" (Cancel while a copy runs), Clear
  cache, and a status text. Refresh lists the devices again and reloads the
  tree. With one device, the GUI opens it at start.
- Left: the device tree from `/`. A folder is listed when you open it. Each
  folder and file has a checkbox. Checking a folder checks everything below
  it, also the parts that are not listed yet. A folder with only some checked
  items shows a partial mark.
- Right: the files of the selected folder in a table with the columns Name,
  Size and Modified (`YYYY-MM-DD HH:MM`, device time). Drag a column border
  to resize it; the widths stay while the window is open. Click a column
  header to sort by it, click again to reverse; ▲ or ▼ marks the sort
  column. The default is Name ascending. Name sorts folders and files
  together, like `ls`. Size and Modified put folders first (by name), then
  the files by size or date; files without a size or date go last. A folder
  has no size. Click selects a row. Ctrl-click adds or removes a row. Shift-click selects a
  range. Double-click opens a file, or opens a folder. Press the left button
  on empty space or on a row that is not selected, then drag: a selection
  rectangle selects every row it touches (rubber band). With Ctrl held when
  the drag starts, the rows add to the selection. The list scrolls when you
  drag past its top or bottom edge. A drag that starts on a selected row
  drags the selected items to File Explorer (see below). Ctrl+C copies the
  selected rows for a paste in File Explorer, or the checked items when no
  row is selected. A right-click on a row that is not selected selects only
  that row, like File Explorer; a right-click on a selected row keeps the
  selection.
- Above the file list: Back (←), Forward (→), Up (↑) and the path bar
  (`/ › Internal Storage › DCIM › 202601_a`). Click a path segment to go to
  that folder. Entering a folder (double-click, a click in the tree, Open,
  the path bar, Up) adds it to the history. Shortcuts: Alt+Left = Back,
  Alt+Right = Forward, Alt+Up or Backspace = Up, mouse side buttons = Back
  and Forward, Enter opens the one selected row, Ctrl+A selects all rows in the
  folder, Ctrl+Shift+A or Escape deselects all.
- Bottom: the progress of the current file (its current speed and ETA; it
  stays on the last file between files), the overall progress, and a log of
  skip, retry and error lines. A copy first scans the selected set (status
  "Scanning... N files, X"; Cancel stops it), so the overall bar is a true
  percent of the bytes, with files and bytes done/total, current and average
  speed, elapsed time and ETA. The final overall line stays until the next
  run. A double-click download and an Explorer paste show their speed and ETA
  in the same way.

The cache of downloaded files is deleted when the GUI closes and when it
starts (files that a viewer still holds are left). Untick "Clear cache on
exit" in the top bar to keep it; the setting is saved. The "Clear cache"
button deletes the cache of the open device at any time. Copy destinations
and manifests are never touched.

Right-click menus:

| Where | Items |
| --- | --- |
| A file (list or tree) | Open, Open cache folder (only when the file is cached; Explorer selects it), Copy to..., Copy (paste in Explorer), Check, Uncheck, Properties |
| A folder (list or tree) | Open, Copy to..., Copy (paste in Explorer), Check all beneath, Uncheck all beneath, Expand all, Collapse all, Properties |
| Empty space in the list | Refresh, Select all, Deselect all |

In the list, a menu on a selected row acts on all selected rows. Open is
enabled only for one item. Properties shows the name, the device path, and
for a file the size, dates, WPD content type and whether it is cached; for a
folder the number of direct subfolders and files.

"Copy to folder" (top bar) copies the checked items into the destination folder, like
`cp -r -p` with the default warn-and-skip rule (or `--force` with the
checkbox). The copy starts at the deepest folder that holds all checked
items and copies only the checked items below it. For example, checked
items in `202601_a` and `202601_b` go to `DEST\DCIM\202601_a` and
`DEST\DCIM\202601_b`. "Copy to..." asks for a folder and copies the
selected items, like `cp -r -p <items> DEST`. Both use the manifest and the
incremental rules of `cp`, so a second copy skips verified files. Cancel
stops after the current file.

Double-click on a file downloads it to the cache, then opens it with the
default Windows application. The file never opens before the download is
complete. The cache is
`%LOCALAPPDATA%\win-iphone-dcim\cache\<device-id-hash>\<device path>`, with
the original folder and file names. A cached file is reused while its size
matches the device size. Clear cache deletes the cache folder of the open
device. Changes in the viewer do not go back to the iPhone. HEIC photos and
HEVC videos need the "HEIF Image Extensions" and "HEVC Video Extensions" from
the Microsoft Store; the GUI shows this hint when Windows has no application
for the file type.

### Copy and paste or drag and drop to File Explorer

"Copy (paste in Explorer)" in a right-click menu, or Ctrl+C in the file
list, puts the selected items on the clipboard. With no selected row, Ctrl+C
takes the checked items. With neither, the status text shows "Select or
check items to copy". Folders include everything below them. The status text shows "N items copied. Paste in File Explorer."
when the folders are listed. Then paste in any File Explorer folder.

You can also drag selected rows from the file list and drop them on a File
Explorer window or the desktop. Only copy is offered; Explorer never moves
or deletes files on the iPhone.

File Explorer shows its own progress dialog and its own "Replace or Skip
Files" dialog. The GUI adds no dialog. The status text shows "Explorer is
reading N of M" while Explorer reads.

Limits of this mode:

- Explorer reads every file through this window. Keep the window open until
  the paste completes. If you close it during a paste, the GUI asks first
  ("Close anyway"); Explorer then reports an error for the remaining files.
- The speed is the same as the CLI and the in-app copy, not faster.
- Explorer decides replace or skip. There is no manifest, no verification
  and no incremental skip of verified files. Use "Copy to folder" or
  "Copy to..." for those.
- A path under the copied folder longer than 259 characters is left out,
  with a line in the log.
- A WPD hang stops the paste until the worker restarts (120 s without
  activity); Explorer then reports an error for that file.
- Device requests run one at a time: Explorer waits while an in-app copy
  runs.

Limits:

- Device requests run one at a time: while a copy runs, folder listings wait.
- Real-hardware status: see [Verification scope](#verification-scope). Only the GUI start and the device list are verified.

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

## Worker process

`ls`, `tree`, `cp` and `devices` run every WPD call in a child process: the
same executable, started with a hidden `worker` subcommand. If a device call
shows no activity for `--timeout`, or the worker crashes, the main process
kills the worker, fails the current file with "the device worker was
restarted", and starts a new worker for the next call; `cp` retries the file
like any other transient error. COM objects never leave the worker, and the
new worker finds files again by their device path.


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

## Testing on a real iPhone

Only the GUI start and the device list are verified on real hardware (see [Verification scope](#verification-scope)). Run this checklist on Windows with an
iPhone attached, unlocked and trusted. Use a new empty folder for DEST, for
example `D:\iPhoneTest`.

1. `devices` lists the iPhone.
2. `ls -l /` shows `Internal Storage`.
3. `tree "/Internal Storage/DCIM"` shows the same folders as File Explorer.
4. `cp` of one HEIC file to DEST: the copy opens and has the device size.
5. `cp -r` of one DCIM folder to DEST: all files are copied.
6. Run the same `cp -r` again: every file is `[skip] ... verified`.
7. `verify DEST` reports `missing=0 size-mismatch=0`.
8. Lock the iPhone and run `devices` and `ls /`: a clear error, no hang.
9. Unplug the cable during a `cp -r` of a folder with large MOV files: watch
   for `[retry k/N]` lines, plug the cable back in and unlock the phone. No
   `.part` file is left as a complete file. A second run copies the rest.

Record the iPhone model, the iOS version and the Windows version with the
results.

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
scripts/dev.sh test                # unit tests (Linux container)
scripts/dev.sh test --features fake-device   # unit and end-to-end tests
scripts/dev.sh xwin check --target x86_64-pc-windows-msvc
scripts/dev.sh xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings
```

The binaries go to `target/<target>/release/win-iphone-dcim.exe` and
`target/<target>/release/win-iphone-dcim-gui.exe`. The `target`
directory is a Docker volume. GitHub Actions on `windows-latest` is the
authoritative build. Tests that need WPD or a real iPhone are manual. Run them
on Windows with an iPhone attached.

## Design notes

- The tool calls the WPD COM API directly. It does not use the Windows Shell
  namespace or the Explorer copy APIs.
- COM objects never cross threads. Each COM apartment owns its own handles.
- The crate is a library (`src/lib.rs`) with two binaries: the CLI
  (`src/main.rs`) and the GUI (`src/bin/gui.rs`). The GUI uses eframe/egui
  with the glow renderer. Its UI thread never touches the device; a device
  thread owns the `DeviceFs` and answers requests over channels. The copy
  engine (`backup::engine`) reports progress through a `ProgressSink`: the CLI
  draws terminal bars, the GUI gets events.
- All WPD code is in `src/wpd/` under `#[cfg(windows)]`. On other platforms,
  WPD calls return an "unsupported platform" error.
- The commands use the `DeviceFs` trait. Unit tests use the in-memory fake in
  `device_fs::fake`, so CI does not need an iPhone. `tests/e2e.rs` runs the
  built executable with `WIN_IPHONE_DCIM_FAKE_FS=1`, so the worker serves the
  fake device. The fake is compiled into a binary only with the `fake-device`
  cargo feature, and `tests/e2e.rs` runs only with that feature. Release
  binaries do not enable it, so they do not contain the fake device, and the
  variable has no effect on them.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
