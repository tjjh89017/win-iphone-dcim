# iPhone DCIM Backup for Windows — Implementation Specification

Status: Draft v0.1  
Target: Windows 10/11 x64, Rust stable  
Primary backend: Windows Portable Devices (WPD) via Microsoft's `windows` (`windows-rs`) crate  
Language: English for identifiers/CLI and for this specification  
Style: ASD-STE100 Simplified Technical English  

## 1. Goals

Develop a **read-only** Windows CLI. The CLI copies photos and videos directly over WPD from an iPhone on a USB connection. The CLI must **fully preserve the relative DCIM folder structure that Windows File Explorer shows**. The user can also copy single folders or files with `cp`. Example:

```text
iPhone / Internal Storage / DCIM /
  202601_a / IMG_0001.HEIC
  202601_b / IMG_0002.MOV
  202602_a / IMG_0003.DNG
```

Output:

```text
D:\iPhoneBackup\DCIM\
  202601_a\IMG_0001.HEIC
  202601_b\IMG_0002.MOV
  202602_a\IMG_0003.DNG
```

The key point is to **preserve the logical folders that WPD exposes**. The tool does not rebuild albums. The tool does not regroup files by EXIF date. The tool does not guarantee that this structure is identical to the physical folders in the iOS device.

### Must achieve

- Call the WPD COM API directly. Do not use the Explorer UI.
- Keep the iPhone read-only. Do not change or delete objects on the iPhone.
- Save the downloaded bytes without changes. Do not convert images, videos, or metadata.
- Enumerate all DCIM subfolders recursively. Save folder names and file names without changes. Process HEIC/JPG/PNG/DNG/MOV/MP4/AAE and all other file types in the same way.
- Transfer one file at a time. Stream the data to the disk. Do not load a full file into RAM.
- Sync incrementally. On a new run, skip the files that are already verified.
- Retry a failed file individually. Show clear transfer progress and a failure report.
- Keep the main process in control if a WPD COM call blocks permanently. The main process must continue to report status. The main process must be able to terminate the worker.
- Support Windows x64 in the first version. Also build a Windows ARM64 binary, but do not test it on hardware. Keep an abstract interface for a future AFC backend.

### Non-goals (v1)

- Do not implement the USB/PTP/MTP protocols in this project.
- Do not use the Windows Shell Namespace / Explorer copy APIs.
- Do not write or delete iPhone files. Do not provide two-way sync.
- Do not recreate the album structure of the Photos app.
- Do not guarantee support for originals that iCloud did not download to the device yet.
- Do not read from the same iPhone with concurrent threads. Stability has priority.
- Do not claim portable, reliable offset resume on a WPD `IStream` during a transfer. In the first version, a file retry starts again from the first byte.
- Do not build the GUI before Phase 3 is complete. (Satisfied: Phase 3 is complete, and Phase 4 has started.)

## 2. Technical assumptions to verify first (Go / No-Go)

These items are **PoC acceptance criteria**. Do not treat them as known facts for all iPhone/iOS versions:

1. Windows can find an unlocked iPhone through WPD after the user taps "Trust".
2. The WPD object tree contains an `Internal Storage/DCIM` logical path. This path is similar to the path in Explorer.
3. WPD lists the actual folder names, such as `202601_a` and `202601_b`. WPD does not show only a flat virtual view.
4. The tool can get a read-only `IStream` for `WPD_RESOURCE_DEFAULT` for a file.
5. The tool can fully download large MOV, HEIC, DNG, and AAE files without unexpected conversion.
6. The tool can enumerate the iPhone again after the iPhone disconnects and reconnects. Then the copy can continue.

**If point 2 or 3 fails**, do not make your own year/month folder naming rule. Such a rule only fakes the original structure. Record the device tree information. Compare WPD with Explorer. If necessary, research an AFC backend. **If point 4 or 5 fails**, first examine the device settings and the WPD driver limits.

## 3. Technology choices

- Language: Rust stable, Edition 2024. If the project toolchain does not support Edition 2024, use Edition 2021 first.
- Windows bindings: `windows` crate (Microsoft `windows-rs`). Enable the necessary `Win32_*` features. Compile with the pinned crate version and use the result to verify the API paths.
- CLI: `clap` (derive).
- Error handling: `thiserror` (library) and `anyhow` (binary).
- Log: `tracing`, `tracing-subscriber`.
- Manifest: `serde`, `serde_json`. Use JSONL for v1 (recommended). SQLite can replace JSONL later.
- Hash: `sha2` or `blake3`. Use the hash only to verify the integrity of the **local backup files**. If the iPhone does not supply a trusted hash, do not claim end-to-end verification from the source.
- Async runtime: none yet. WPD COM is a synchronous, blocking API. Isolate WPD COM with a **child process**, not only with a Tokio timeout.
- Build tools: `cargo fmt`, `cargo clippy`, `cargo test`. Build and test on a GitHub Actions Windows runner.

### windows-rs features/interfaces (to confirm during implementation)

```text
Win32::Devices::PortableDevices
  IPortableDeviceManager
  IPortableDevice
  IPortableDeviceContent
  IPortableDeviceProperties
  IPortableDeviceResources
  IPortableDeviceValues
  IEnumPortableDeviceObjectIDs
Win32::System::Com
  CoInitializeEx / CoUninitialize
  CoCreateInstance
  IStream
```

Important WPD constants and property keys:

```text
WPD_DEVICE_OBJECT_ID
WPD_OBJECT_NAME
WPD_OBJECT_ORIGINAL_FILE_NAME
WPD_OBJECT_CONTENT_TYPE
WPD_OBJECT_SIZE
WPD_OBJECT_DATE_MODIFIED  (if provided by the device)
WPD_RESOURCE_DEFAULT
STGM_READ
```

The report of the actual device controls if a property is present and valid. Do not use `unwrap()` blindly.

## 4. Repo structure

```text
win-iphone-dcim/
├── Cargo.toml
├── Cargo.lock
├── Dockerfile
├── docker-compose.yml
├── AGENTS.md
├── README.md
├── SPEC.md
├── scripts/
├── .github/
├── src/
│   ├── lib.rs                 # library: declares the modules
│   ├── main.rs                # CLI entry point (and the hidden worker)
│   ├── bin/
│   │   └── gui.rs             # GUI entry point: win-iphone-dcim-gui
│   ├── cli.rs                 # CLI arguments
│   ├── model.rs              # Device, FileEntry, SyncDecision, TransferReport
│   ├── error.rs
│   ├── device_fs.rs           # device file system access for the commands
│   ├── devpath.rs             # device path parsing
│   ├── paths.rs               # local path handling
│   ├── progress.rs            # cp progress bars and progress events
│   ├── cmd/
│   │   ├── mod.rs
│   │   ├── ls.rs              # ls command
│   │   ├── tree.rs            # tree command
│   │   ├── cp.rs              # cp command, incremental rules, retries
│   │   ├── verify.rs          # verify command
│   │   └── worker.rs          # hidden worker subcommand; owns the WPD COM objects
│   ├── wpd/
│   │   ├── mod.rs
│   │   ├── com.rs             # COM apartment lifecycle; no cross-thread COM handles
│   │   ├── device.rs          # discover/open
│   │   ├── enumerate.rs       # traverse object tree
│   │   ├── properties.rs      # object name, content type, size
│   │   └── stream.rs          # IPortableDeviceResources::GetStream
│   ├── backup/
│   │   ├── mod.rs
│   │   ├── engine.rs          # copy executor with a ProgressSink (CLI bars, GUI events)
│   │   ├── planner.rs         # path mapping, cp/rsync DEST rules, lazy folder walk
│   │   ├── transfer.rs        # .part copy + validation + atomic rename
│   │   ├── manifest.rs        # JSONL load/append/reconcile
│   │   └── paths.rs           # Windows filename validation, case collisions
│   ├── supervisor.rs          # parent side: worker lifecycle, watchdog, restart
│   ├── ipc.rs                 # JSONL control messages and the data pipe
│   └── gui/                   # Phase 4; see section 10
│       ├── mod.rs
│       ├── app.rs             # egui window (Windows only); UI thread
│       ├── device.rs          # device thread; owns the DeviceFs
│       ├── selection.rs       # tree model, check marks, list selection
│       ├── nav.rs             # Back/Forward/Up history and path bar segments
│       ├── cache.rs           # open-file cache paths and size limit
│       ├── config.rs          # read-only win-iphone-dcim.toml and env overrides
│       ├── filedesc.rs        # FILEGROUPDESCRIPTORW layout and flags for the Explorer paste
│       ├── chunks.rs          # bounded chunk channel from the device thread to a paste stream
│       ├── dataobject.rs      # IDataObject and IStream for the Explorer paste (Windows only)
│       ├── dnd.rs             # IDropSource and DoDragDrop (Windows only)
│       └── shell.rs           # ShellExecuteW and Explorer (Windows only)
└── tests/
    └── e2e.rs                 # runs the built executable against the fake device
```

Each module has its unit tests in `<module>/tests.rs` (for example `src/cli/tests.rs`).

The crate is a library with two binaries: the CLI and the GUI. The GUI starts the CLI program in its own folder as the worker. Start the worker with the hidden `worker` subcommand. The parent and the worker exchange commands and results as JSONL through stdin/stdout. Use stderr only for logs. This rule keeps noise out of the protocol output.

## 5. CLI interface

```powershell
win-iphone-dcim.exe devices
win-iphone-dcim.exe ls   [-d <device>] [-l] [-R] [--json] [PATH...]
win-iphone-dcim.exe tree [-d <device>] [-L <depth>] [--json] [PATH]
win-iphone-dcim.exe cp   [-d <device>] [-r] [-n | -f] [-p] [-a] [--dry-run] [--verify size|local-hash] SRC... DEST
win-iphone-dcim.exe verify [--hash] DEST
```

### Path model

- A device path starts at the device root `/`. Example: `/Internal Storage/DCIM/202601_a`. Quote a path that has spaces.
- A local path (`DEST`) is a normal Windows path.

### Commands

- `devices` lists the connected devices with their stable selection keys.
- `ls` lists one level. `ls` with no `PATH` lists `/`.
  - `-l` shows the size, the content type, and the modification date if the device supplies them.
  - `-R` recurses.
  - `--json` prints one JSON object per line.
- `tree` prints a branch view like the Unix `tree` command, with `├──`, `│`, and `└──`. `tree` shows folders and files.
  - `-L <depth>` limits the depth.
  - `--json` prints one JSON object per line. Each object has `depth`, `path`, `name`, `is_folder`, `size`, and `object_id`.
- `cp` copies from the device to a local path. Each `SRC` is a device path. `DEST` is a local path. `cp` never writes to the device.
- `verify` checks the files in `DEST` against the manifest in `DEST/.win-iphone-dcim/manifest.jsonl`.
  - Each record needs a file with the recorded size. `--hash` also recomputes the stored BLAKE3 hash.
  - A file under `DEST` without a record is `unrecorded`. `verify` ignores `.win-iphone-dcim/` and `*.part` files.
  - `verify` prints one line per file and `[verify] ok=N missing=N size-mismatch=N hash-mismatch=N unrecorded=N`. The exit code is `0` if all files are ok, otherwise `1`.

### Copy rules

`cp` follows the rules of Unix `cp` and `rsync`:

- One file `SRC` and a `DEST` that is an existing folder: copy the file into the folder.
- One file `SRC` and a `DEST` that does not exist: create a file with the `DEST` name.
- A folder `SRC` needs `-r`. Without `-r`, fail with a clear message.
- Use the trailing slash rule of `rsync`:
  - `cp -r "/Internal Storage/DCIM" D:\Backup` creates `D:\Backup\DCIM\...`.
  - `cp -r "/Internal Storage/DCIM/" D:\Backup` copies the contents of `DCIM` into `D:\Backup\...`. It does not create a `DCIM` folder.
- Two or more `SRC`: `DEST` must be an existing folder. Otherwise, fail.
- One folder `SRC`, `-r`, and a `DEST` that does not exist: create `DEST` and copy the contents into it, as `cp -r` does.

### Conflicts and manifest

- By default, when a target file exists, warn and skip the file. Keep the existing file. Count the file as skipped. The summary shows these files in the `exists` field. Such a skip is not a failure.
- `-f` (force) overwrites an existing target file. Copy to a `.part` file first. Verify the size. Then replace the target with an atomic rename. Never truncate the existing file before the new data is complete.
- `-n` (no-clobber) skips every existing target file silently. `-n` and `-f` cannot be used together.
- The tool never overwrites a file in place.
- Add `-i` (interactive replace or skip prompt) only in a later version.
- Every `cp` writes the JSONL manifest to `DEST/.win-iphone-dcim/manifest.jsonl`. Every `cp` applies the incremental rules in section 7.
- A second `cp` of the same tree skips the verified files.
- A separate `sync` command does not exist.

### `cp` flags

| Flag | Default | Description |
| --- | --- | --- |
| `-r` | false | Copy folders recursively. A folder `SRC` needs this flag |
| `-n` | false | Do not overwrite. Skip every existing target file silently. Conflicts with `-f` |
| `-f` | false | Overwrite an existing target file. Copy to a `.part` file, verify the size, then replace the target atomically. Without `-f` and `-n`, the tool warns and skips an existing target file |
| `-p` | false | Preserve timestamps. After the commit, set the local modified time, and the created time where Windows allows it, from `WPD_OBJECT_DATE_MODIFIED` and `WPD_OBJECT_DATE_CREATED`. If the device gives no date, keep the copy time and log it |
| `-a` | false | Archive mode. Equal to `-r -p`. Permissions, ownership and links do not exist on the device, so `-a` preserves only timestamps |
| `--dry-run` | false | Enumerate and print the copy plan only. Do not write files |
| `--verify size\|local-hash` | size | Verification mode for the incremental rules of section 7 |

### Global flags

| Flag | Default | Description |
| --- | --- | --- |
| `-d`, `--device <id>` | Automatic if only one device is connected | Use the stable selection key that `devices` lists, if possible. Use an index only for interactive use |
| `--retries <n>` | 3 | Maximum number of retries for each file. This number counts the additional attempts after a failure |
| `--timeout <duration>` | 120s | Watchdog for a worker that does not respond. The tool cannot cancel all COM calls. Whole seconds (`90`) or a number with the unit `s`, `m` or `h` (`90s`, `2m`) |
| `--no-isolate` | false | Run the WPD calls in the main process, not in a worker. A hung call then hangs the tool. Debugging only |
| `--log-format` | text | `text` / `json` |
| `--diagnostic` | false | Also log the raw device ID. Without it, logs show the first 8 hex characters of its BLAKE3 hash |

### Example output

`tree`:

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

`cp`:

```text
[copy] DCIM/202601_a/IMG_0001.HEIC  3.7 MiB / 3.7 MiB
[skip] DCIM/202601_a/IMG_0002.MOV  verified
[retry 1/3] DCIM/202601_b/IMG_0021.MOV  device temporarily unavailable
[done] copied=142 skipped=65 exists=3 failed=0
```

Recommended exit codes: `0` all files completed; `1` some files failed; `2` CLI/configuration error; `3` the tool cannot find or open the device; `4` worker/IPC failure.

## 6. Device discovery and object enumeration

1. Initialize COM in the worker on a dedicated OS thread (`CoInitializeEx`). Use MTA first. If tests show poor compatibility, test STA. With STA, process the message pump correctly.
2. Create `IPortableDeviceManager`. List the device IDs and friendly names.
3. Create `IPortableDevice`. Open it with read-only intent. Use `IPortableDeviceValues` to set the necessary client metadata / access mode. Use the Microsoft samples as the reference for the specific keys and values.
4. Call `Content()` → `EnumObjects(0, parent_id, None)`. Enumerate downward from `WPD_DEVICE_OBJECT_ID`.
5. Get these properties for each object: `WPD_OBJECT_NAME`, `WPD_OBJECT_ORIGINAL_FILE_NAME` (if applicable), `WPD_OBJECT_CONTENT_TYPE`, and `WPD_OBJECT_SIZE` (if applicable). Use them to identify folders and files.
6. Find the exact DCIM folder. Output the relative paths below it. Do not use WPD object IDs as file names.
7. Enumerate in batches with `IEnumPortableDeviceObjectIDs::Next()` until the end. Do not load all object IDs into memory at one time.
8. Record each invalid path or name as an error. Do not allow traversal outside the destination root folder.

**Reminder:** Two objects can have the same name. Object IDs can change after a reconnection. Do not use only the object ID as the primary identity in the manifest. Combine the device identity, the relative path, the size, and the available metadata.

## 7. Transfer and verification flow

Per-file flow:

```text
enumerated file entry
  -> safe destination path
  -> inspect existing destination/manifest
  -> open WPD IStream with WPD_RESOURCE_DEFAULT + STGM_READ
  -> create unique .part file with create_new
  -> read stream sequentially + write local file
  -> flush / sync_data as policy requires
  -> ensure bytes copied match expected WPD size (if provided)
  -> rename .part to final path ONLY when target does not already exist
     (with -f: replace the target atomically)
  -> append committed manifest record
```

- Use the optimal buffer size that the driver returns. Apply reasonable lower and upper limits to abnormal values, for example 64 KiB to 4 MiB. Adjust these limits after benchmarks on real devices.
- Treat only a `Read()` that returns 0 as EOF. If the expected size is known and the stream has fewer bytes, fail. If the stream has more bytes than expected, report a metadata mismatch. Keep the error information.
- Mark the verification status as `size-unavailable` if the tool cannot get the source size. Do not claim that the file is verified.
- Give each `.part` file a unique name. Never treat a remaining partial file as a complete file. Before a retry, remove or quarantine the partial file from the failed attempt.
- Use a safe no-clobber strategy for the final rename. On Windows, `rename` can affect an existing file. Guarantee that no overwrite occurs, for example with explicit Windows create/move flags. With `-f`, replace the target atomically: on Windows, call `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`.
- Use a hash only as proof of local integrity. A hash proves that the local file did not change before and after a copy, or that two reads match. A hash does not prove that the iPhone original and the local file are identical. Only a verifiable source checksum from the device can prove that.
- Do not skip a file on its file name alone. The content or metadata of a file on the iPhone can change.

### Incremental rules

- A **completed manifest record**, an existing target file, and a matching size → Skip the file.
- An enabled `local-hash` mode and a stored hash in the manifest → Recompute the hash. Skip the file only if the hash matches.
- An existing target file with a different size, or an inconsistent manifest → Report a conflict. By default, warn, skip, and keep the existing file. With `-f`, replace it.
- No manifest record, but an existing target file with the same size → Mark the file as `unverified-existing` in the first version. By default, do not overwrite the file. Do not claim a successful backup automatically. Add an explicit adopt feature later (not in v1).
- Manifest writes → Make the writes tolerate an unexpected shutdown. Append and flush each record. When you read the manifest, skip an incomplete last JSONL line. Before a run, rebuild the state and compare it with the local files.

## 8. Hangs, retries, and device reconnection

WPD COM calls can block at the driver/USB layer. **A thread timeout or `tokio::time::timeout` cannot reliably cancel a synchronous COM call**.

### Architecture

```text
Parent supervisor (CLI, owns log/manifest coordinator)
  |
  +-- Spawn worker process (owns WPD COM objects)
        |
        +-- Enumerate / transfer one file at a time
        +-- Emit start/progress/success/error via IPC
```

- Keep a heartbeat / last-activity time in the parent. **Note:** A COM `Read()` can block during a large file transfer. Then the worker possibly cannot send heartbeats.
- Do these steps when the worker times out: record the current file and phase → attempt a graceful stop → terminate the worker process → clean up the `.part` file → restart the worker and enumerate the device again.
- Do not reuse old COM pointers or stale object IDs after a worker restart.
- Identify error categories, such as `device disconnected`, `access denied/trust required`, `timeout`, and `local disk full`. Retry only transient errors.
- Use backoff for retries, for example 1s, 3s, 10s. This prevents infinite loops.
- Expect user actions after the device reconnects. The device can require the user to unlock it, keep the screen awake, or tap Trust again.
- Use **one file at a time with a rebuildable worker** in the first version. If repeated tests show that long enumerations also hang, add restartable checkpoints to the enumeration phase.

## 9. Security, paths, and data integrity

- For names that come from the device: sanitize or reject absolute paths, `..`, drive prefixes, UNC paths, invalid Windows file names, reserved names (such as CON, NUL), and path traversal.
- Treat case-insensitive collisions on Windows, duplicate file names, and illegal characters as errors. **For an unsafe name or a collision, report an error and stop the transfer of that file**. An existing target file is not such an error: the tool warns and skips it, or replaces it with `-f` (section 5). Never overwrite silently. Never rename silently and then call the result "structure fully preserved".
- Do not use EXIF dates to make folders or to rename files.
- Do not use the modification time as the only criterion. The file size and time from the device can be missing or incorrect. Timestamps that `-p` preserves are metadata only. Do not use them in skip or verify decisions.
- Do not delete extra files at the destination by default. This prevents accidental loss of past backups.
- Do not show the full value of sensitive device identifiers in logs. The user can select a diagnostic mode.
- Set the iPhone "Transfer to Mac or PC" setting to **Keep Originals**. Also verify the actual transferred content with samples.
- Report the visibility limit if iCloud Photos "Optimize iPhone Storage" is on. In that case, some originals can be absent from the device. Do not claim a backup of all iCloud photos.

### Local destination paths

- Accept `DEST` as any Windows path: a drive path (`D:\Backup`), a UNC path (`\\server\share\Backup`), or a mapped network drive. Samba and other SMB servers are valid destinations.
- Handle Unicode in all paths. Keep local paths as `OsString` or `PathBuf`. Never keep them as `String`. Convert to UTF-16 only at the Windows API boundary.
- Device names arrive as UTF-16 from WPD. Convert them with the lossless Windows path functions. Reject a name that is not valid Unicode. Do not replace characters silently.
- Support paths longer than 260 characters. Make `DEST` absolute with `std::path::absolute`. Before every file operation, add the verbatim prefix `\\?\` for drive paths. Use `\\?\UNC\server\share` for UNC paths. Do not depend on the `LongPathsEnabled` registry setting.
- Use Windows API calls that guarantee no overwrite. `std::fs::rename` on Windows uses `MOVEFILE_REPLACE_EXISTING`. Do not use it for the final commit. Call `MoveFileExW` without that flag, or use `create_new` plus a copy. The commit must fail if the target exists. Only `-f` replaces a target, with `MoveFileExW` and `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH` through the `\\?\` path.
- Expect SMB differences. A share can be case-sensitive or case-insensitive. Check for a case-insensitive collision with the actual listing of the target folder. Do not assume the case behavior.
- `fsync` on SMB can be slow or a no-op. Report this as a limit. Do not fail.
- Treat a network error during a write as a transient error. Keep the `.part` file rule. Never leave a partial file under the final name.

## 10. Implementation phases and DoD

### Phase 0 — Minimum feasibility PoC (priority)

- [ ] Create a Rust project that compiles. Connect the project to windows-rs WPD COM.
- [ ] Show the iPhone with `devices`. Show clear errors for "no device" and "not trusted".
- [ ] `ls` and `tree --json` list the root, `DCIM`, and `202601_a` / `202601_b`.
- [ ] List the file name, size, object ID (debug only), and content type.
- [ ] Download one HEIC, one MOV, and one AAE/DNG file individually with `cp`, if the device has them.
- [ ] Compare the size and format of the Windows copies with the actual files. Make sure that the copies open. Record the iPhone model, the iOS version, and the Windows version.

**Go / No-Go:** Start Phase 1 only after you confirm that the tool preserves the necessary folder names.

### Phase 1 — Full backup MVP

- [x] Implement `cp -r` with recursive enumeration and streamed copy.
- [x] Implement the trailing slash rules and the multiple SRC rule exactly as section 5 states.
- [x] Write to a safe `.part` file. Commit with no-clobber after completion.
- [x] Verify the copy size. Show an error summary. Return a non-zero exit code for failures.
- [x] Implement dry-run and logging. Add path safety tests.
- [x] Implement `-p` and `-a`. Set file times with `SetFileTime` through the `\\?\` path.

### Phase 2 — Incremental and reliability

- [x] Implement the JSONL manifest, skip, conflict, and retry.
- [x] Implement child process isolation, the watchdog, and worker crash recovery.
- [x] Rebuild the enumeration after a USB unplug/replug. Do not overwrite existing data. `RemoteFs` resolves nodes by device path in the new worker. A real unplug test is still manual.
- [ ] Run long tests on a real device with 10,000+ files and many large MOV files.

### Phase 3 — Release

- [x] Run fmt/clippy/test/build on GitHub Actions `windows-latest`.
- [x] Produce the `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` Release EXEs, a checksum for each, and basic usage documentation. The release ships one zip per target with the real EXE names (the GUI starts `win-iphone-dcim.exe` from its own folder) and one `SHA256SUMS.txt` over the zips.
- [ ] Test on a minimum of two iPhone/iOS combinations. If only one combination is available, state the verification scope clearly.
- [x] Add mock backend / fixture tests for use without an iPhone, behind the `fake-device` cargo feature, not in release builds. CI must not depend on hardware.

Verification scope as of 2026-10-09: on real hardware, the GUI starts on Windows x64 and lists the iPhone. No copy from a real iPhone has run yet. All automated tests use the in-memory fake device.

### Phase 4 — GUI (last phase, optional)

Goal: give the user a window to select folders and files from the DCIM tree, then copy them with the normal Windows copy flow. The user can paste the selection into File Explorer. Explorer then shows its own progress dialog and its own "Replace or Skip Files" dialog.

- [x] Build the GUI as a separate binary or a `gui` subcommand that reuses the CLI core. Do not duplicate the WPD code.
- [x] Show the DCIM tree with checkboxes. Show the file name, the size, and the folder path. Do not show the WPD object ID.
- [x] Let the user open a folder in the tree and browse its files, as in a file manager. Show a list view with name, size, and date.
- [x] Open a file with the Windows default application when the user double-clicks it. Windows applications cannot read a WPD stream directly. Download the file first to a local cache folder. Then call `ShellExecuteW` with the `open` verb on the cached file.
- [x] Put the cache in `<cache folder>\<device-id-hash>\<relative path>`. The cache folder is `cache` next to the GUI exe, or `cache_dir` from the read-only `win-iphone-dcim.toml` next to the exe or `WIN_IPHONE_DCIM_CACHE_DIR`. If it is not writable, use `%LOCALAPPDATA%\win-iphone-dcim\cache`. Keep the cache under a soft size limit (default 512 MiB): before a download, delete the least recently used files; never refuse the download. Keep the GUI portable: save no settings. Reuse a cached file when its size matches the WPD size. Show a progress indicator during the download. Let the user clear the cache from the GUI.
- [x] Give the cached file its original file name and extension. Then Windows picks the correct application for HEIC, MOV, DNG, and other types.
- [x] Do not open the file from the GUI before the download is complete. A partial file can crash the viewer.
- [x] Implement a COM `IDataObject` that offers `CFSTR_FILEDESCRIPTORW` and `CFSTR_FILECONTENTS`. Give each `FILEDESCRIPTORW` the relative path under `DCIM`, the `FD_FILESIZE` flag with the WPD size, and `FD_ATTRIBUTES` for folders. Supply each `CFSTR_FILECONTENTS` as an `IStream` that reads from the WPD stream on demand.
- [x] Put the `IDataObject` on the clipboard with `OleSetClipboard`. Also support drag and drop with `DoDragDrop`. Explorer pulls the data from this process, so the process must stay open until the paste completes.
- [x] Serve one `IStream` at a time from the worker process over IPC. Keep all WPD COM objects in the worker. Do not pass COM pointers to the GUI process.
- [x] Let Explorer handle conflicts. Do not add a second conflict dialog.
- [x] Add an in-app copy mode that uses the Phase 1/2 transfer engine and the manifest. Use it when the user wants verification and an incremental copy. The Explorer paste mode has no manifest and no verification.
- [x] Use a Rust GUI toolkit that compiles with the MSVC target and does not need a web runtime. Evaluate `egui`/`eframe` first. Evaluate native Win32 controls second.

Known limits:

- The Explorer paste copies through this process. The speed is the same as the CLI, not faster.
- A WPD hang during a paste blocks Explorer's copy dialog until the watchdog restarts the worker and the `IStream` returns an error.
- The paste mode cannot skip verified files, because Explorer does not read the manifest.
- Double-click opens a cached copy, not the file on the device. Changes in the viewer do not go back to the iPhone.
- HEIC and HEVC playback needs the Windows HEIF and HEVC extensions. The tool does not install them. Show a hint if `ShellExecuteW` reports no association.

## 11. Test scenarios

| Test | Expected result |
| --- | --- |
| `202601_a` / `202601_b` each contain a file with the same name | The tool keeps each file in its correct folder |
| Two backups in sequence | The second backup skips verified files |
| Unplug the cable during a MOV download | The tool does not treat the `.part` file as a complete file. Existing backups stay unchanged |
| A single file larger than 4 GiB | The tool uses `u64` sizes and streams the data. No integer overflow occurs |
| Not sufficient local disk space | The tool shows a clear error and keeps existing files |
| iPhone locked / not trusted | The tool shows a clear error. The tool does not retry without limit |
| WPD `Read()` hangs | The parent detects the hang and cleans up the worker. `cp` retries the file in a new worker (automated in `tests/e2e.rs` with the fake device) |
| A file of a different size is at the target path | The tool warns and skips. With `-f` it replaces the file through a `.part` file. It never overwrites in place |
| Case collision or illegal file name | The tool shows a clear error. The tool does not rename the file silently |
| No file size metadata | The tool correctly marks the status as unverified |
| An old object ID becomes invalid after a reconnection | The tool enumerates again and gets the new object ID |
| A file name contains Unicode characters | The tool processes the name correctly with UTF-16 / Windows path operations |
| DEST is a UNC path on a Samba share | The tool copies and verifies normally. The final rename does not overwrite |
| DEST plus relative path is longer than 260 characters | The tool creates the file with the `\\?\` prefix. No `ERROR_PATH_NOT_FOUND` occurs |
| A folder or file name contains CJK or emoji characters | The name is identical on the device and at DEST |
| Double-click a HEIC file in the GUI | The tool downloads the file to the cache and opens it with the default Windows application. The device file is unchanged |
| `cp -a` on a folder | Every copied file has the device modified time. The skip decision on a second run does not depend on it. |
| `cp -r SRC DEST` and `cp -r SRC/ DEST` | The first creates `DEST\<name>`. The second copies the contents into `DEST`. Add tests for both |
| Paste a selection into Explorer when a file already exists at the target | Explorer shows its own Replace or Skip dialog. The tool does not add a dialog. Automated tests cover the descriptor layout; the Explorer interaction is manual |
| Close the GUI during an Explorer paste | The GUI asks first. After "Close anyway", Explorer reports a copy error. No `.part` or partial file remains at the target as a complete file. Automated tests cover the descriptor layout; the Explorer interaction is manual |

## 12. Core design constraints / common mistakes

1. **Do not treat WPD as a real disk path**: Object IDs / COM interfaces are not `std::fs` paths. Use the WPD stream to download files.
2. **Do not send COM interfaces across threads unconditionally**: Some WPD interfaces in `windows-rs` are `!Send` / `!Sync`. Keep the lifetime of each COM object in the worker thread / apartment that initialized it. Send only serializable metadata between processes.
3. **Do not use Explorer Shell COM in place of WPD**: This tool exists to avoid the unstable copy behavior of the Shell.
4. **Do not confuse the iPhone original bytes with the transferred version**: The iOS transfer setting can convert files. Then the stream is not the original HEIC, even if the program saves the stream exactly. Verify the originals requirement on a real device.
5. **Do not assume a full backup equals the entire content of the Photos app**: The DCIM data on USB possibly does not include photos that are only in the cloud.
6. **Do not use hash verification to dress up false promises**: A local hash is not a source checksum.
7. **Do not rely only on a thread watchdog**: If a synchronous COM call hangs, only a separate process gives isolation. Test the termination behavior.

## 13. Main official references

- Microsoft WPD API overview: https://learn.microsoft.com/en-us/windows/win32/wpd_sdk/windows-portable-devices-sdk
- `IPortableDeviceContent::EnumObjects`: https://learn.microsoft.com/en-us/windows/win32/api/portabledeviceapi/nf-portabledeviceapi-iportabledevicecontent-enumobjects
- `IPortableDeviceResources::GetStream`: https://learn.microsoft.com/en-us/windows/win32/api/portabledeviceapi/nf-portabledeviceapi-iportabledeviceresources-getstream
- Microsoft PortableDeviceCOM sample: https://github.com/microsoft/Windows-classic-samples/tree/main/Samples/PortableDeviceCOM
- Microsoft sample transfer implementation: https://github.com/microsoft/Windows-classic-samples/blob/main/Samples/PortableDeviceCOM/cpp/ContentTransfer.cpp
- windows-rs repo: https://github.com/microsoft/windows-rs
- windows-rs WPD binding docs: https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/Devices/PortableDevices/index.html
- COM initialization: https://learn.microsoft.com/en-us/windows/win32/learnwin32/initializing-the-com-library

## 14. First work item for the Coding Agent

> Create the Rust CLI project from this document. Implement only Phase 0. Use the WPD COM bindings of the Microsoft `windows` crate to implement `devices` and `tree --json`. Enumerate the full Windows WPD object tree recursively. Find out if iPhone DCIM folders, such as `202601_a` and `202601_b`, show their original names. Do not use the Shell Namespace, the old Go WPD bindings, or self-made date folders.
>
> Do not send COM handles across threads. First, make sure that `cargo check` passes on the Windows MSVC target. Then add a single-file streaming download PoC. If Windows API types or features are not clear, read the pinned windows-rs docs first. Also read the official Microsoft PortableDeviceCOM sample. Do not guess function signatures.
