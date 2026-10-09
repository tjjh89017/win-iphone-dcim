//! Streamed file transfer: `.part` file, size check, atomic commit
//! (SPEC.md section 7).
//!
//! The data goes to a unique `<final>.<random>.part` file in the target
//! folder. Only a complete file with the expected size gets the final name.
//! A `.part` file is never treated as a complete file.

use std::collections::hash_map::RandomState;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use crate::device_fs::DeviceFs;
use crate::error::{Error, Result};
use crate::model::{LocalTime, Node, TransferReport, Verification};

const PART_SUFFIX: &str = ".part";
const RANDOM_HEX: usize = 16;

/// A new unique `.part` path next to `target`: `<name>.<16 hex>.part`.
pub fn part_path(target: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u32(std::process::id());
    if let Ok(d) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        h.write_u128(d.as_nanos());
    }
    let mut name: OsString = target.file_name().unwrap_or_default().to_owned();
    name.push(format!(".{:016x}{PART_SUFFIX}", h.finish()));
    target.with_file_name(name)
}

/// True for a name that `part_path` makes.
pub fn is_part_file(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(stem) = name.strip_suffix(PART_SUFFIX) else {
        return false;
    };
    match stem.rsplit_once('.') {
        Some((base, hex)) => {
            !base.is_empty()
                && hex.len() == RANDOM_HEX
                && hex.bytes().all(|b| b.is_ascii_hexdigit())
        }
        None => false,
    }
}

/// `.part` files that an earlier run left in `dir`. They stay in place.
pub fn leftover_parts(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut parts: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| is_part_file(&e.file_name()))
        .map(|e| e.path())
        .collect();
    parts.sort();
    parts
}

/// Create a new `.part` file for `target` with create_new.
fn create_part(target: &Path) -> Result<(File, PathBuf)> {
    let mut last = None;
    for _ in 0..8 {
        let part = part_path(target);
        match OpenOptions::new().write(true).create_new(true).open(&part) {
            Ok(f) => return Ok((f, part)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => last = Some(e),
            Err(source) => {
                return Err(Error::Io {
                    context: format!("create {}", part.display()),
                    source,
                });
            }
        }
    }
    Err(Error::Io {
        context: format!("create a unique .part file for {}", target.display()),
        source: last.unwrap_or_else(|| io::Error::from(ErrorKind::AlreadyExists)),
    })
}

/// Counts written bytes, reports each write to a callback, and optionally
/// hashes the bytes as they go to disk.
struct Counting<'a, W> {
    inner: W,
    on_bytes: &'a mut dyn FnMut(u64),
    hasher: Option<blake3::Hasher>,
}

impl<W: Write> Write for Counting<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        if let Some(h) = self.hasher.as_mut() {
            h.update(&buf[..n]);
        }
        (self.on_bytes)(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Copy `node` to `target` through a `.part` file.
///
/// With `replace` false the commit fails with `Error::OutputExists` if a
/// file is at `target`. With `replace` true an existing file is replaced
/// atomically, and only after the new data is complete. On any failure the
/// `.part` file is removed. `on_bytes` gets the size of each write. With
/// `hash` true the report has the BLAKE3 hash of the bytes written, computed
/// while they stream to disk.
pub fn transfer(
    fs: &dyn DeviceFs,
    node: &Node,
    target: &Path,
    replace: bool,
    hash: bool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<TransferReport> {
    let start = Instant::now();
    let (file, part) = create_part(target)?;
    let result = write_part(fs, node, file, &part, hash, on_bytes).and_then(|(bytes, digest)| {
        let committed = if replace {
            commit_replace(&part, target)
        } else {
            commit_no_clobber(&part, target)
        };
        committed.map_err(|e| match e.kind() {
            ErrorKind::AlreadyExists => Error::OutputExists(target.to_path_buf()),
            _ => Error::Io {
                context: format!("rename {} to {}", part.display(), target.display()),
                source: e,
            },
        })?;
        Ok((bytes, digest))
    });
    match result {
        Ok((bytes, digest)) => Ok(TransferReport {
            bytes,
            verification: match (node.size, digest) {
                (None, _) => Verification::SizeUnavailable,
                (Some(_), Some(_)) => Verification::LocalHash,
                (Some(_), None) => Verification::SizeOk,
            },
            hash: digest,
            replaced: replace,
            elapsed: start.elapsed(),
        }),
        Err(e) => {
            remove_part(&part);
            Err(e)
        }
    }
}

fn remove_part(part: &Path) {
    match std::fs::remove_file(part) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("cannot remove partial file {}: {e}", part.display()),
    }
}

/// Stream the device data into `file`, flush it, and check the byte count.
/// Return the byte count and, with `hash`, the BLAKE3 hash.
fn write_part(
    fs: &dyn DeviceFs,
    node: &Node,
    file: File,
    part: &Path,
    hash: bool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(u64, Option<[u8; 32]>)> {
    let mut w = Counting {
        inner: file,
        on_bytes,
        hasher: hash.then(blake3::Hasher::new),
    };
    let written = fs.read_to(node, &mut w)?;
    // A no-op or slow flush on SMB is a known limit, not an error.
    w.inner.sync_all().map_err(|source| Error::Io {
        context: format!("flush {}", part.display()),
        source,
    })?;
    match node.size {
        Some(expected) if expected != written => Err(Error::SizeMismatch {
            path: part.to_path_buf(),
            written,
            expected,
        }),
        _ => Ok((written, w.hasher.map(|h| *h.finalize().as_bytes()))),
    }
}

/// Rename `part` to `target`. Fail with `ErrorKind::AlreadyExists` if
/// `target` exists. Never replaces a file.
///
/// Windows: `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`, through the
/// verbatim path. Other platforms: `hard_link` + remove. If the file system
/// has no hard links, copy into a `create_new` file.
pub fn commit_no_clobber(part: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
        win::move_file(part, target, MOVEFILE_WRITE_THROUGH)
    }
    #[cfg(not(windows))]
    {
        match std::fs::hard_link(part, target) {
            Ok(()) => std::fs::remove_file(part),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(e),
            Err(e) => {
                tracing::debug!("hard link failed ({e}); copying with create_new");
                copy_no_clobber(part, target)?;
                std::fs::remove_file(part)
            }
        }
    }
}

#[cfg(not(windows))]
fn copy_no_clobber(part: &Path, target: &Path) -> io::Result<()> {
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let copied = io::copy(&mut File::open(part)?, &mut out).and_then(|_| out.sync_all());
    if copied.is_err() {
        drop(out);
        let _ = std::fs::remove_file(target);
    }
    copied
}

/// Rename `part` to `target` and replace an existing `target` atomically.
///
/// Windows: `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING |
/// MOVEFILE_WRITE_THROUGH`, through the verbatim path. Other platforms:
/// `std::fs::rename`.
pub fn commit_replace(part: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        win::move_file(
            part,
            target,
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(part, target)
    }
}

/// Convert a device local time to a `SystemTime`. Windows uses the time
/// zone of this computer. Other platforms treat the value as UTC.
pub fn local_to_system_time(t: LocalTime) -> io::Result<SystemTime> {
    #[cfg(windows)]
    {
        win::local_to_system_time(t)
    }
    #[cfg(not(windows))]
    {
        let d = std::time::Duration::from_secs(t.0.unsigned_abs());
        let st = if t.0 >= 0 {
            SystemTime::UNIX_EPOCH.checked_add(d)
        } else {
            SystemTime::UNIX_EPOCH.checked_sub(d)
        };
        st.ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "time out of range"))
    }
}

/// Set the modified time and, on Windows, the created time of `path`.
/// Windows calls `SetFileTime` on a handle opened through the verbatim path.
pub fn set_file_times(
    path: &Path,
    modified: Option<LocalTime>,
    created: Option<LocalTime>,
) -> io::Result<()> {
    let modified = modified.map(local_to_system_time).transpose()?;
    let created = created.map(local_to_system_time).transpose()?;
    if modified.is_none() && created.is_none() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        win::set_file_times(path, modified, created)
    }
    #[cfg(not(windows))]
    {
        let _ = created;
        match modified {
            Some(m) => OpenOptions::new().write(true).open(path)?.set_modified(m),
            None => Ok(()),
        }
    }
}

#[cfg(windows)]
mod win {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use windows::Win32::Foundation::{FILETIME, HANDLE, SYSTEMTIME};
    use windows::Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW, SetFileTime};
    use windows::Win32::System::Time::{SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime};
    use windows::core::{HSTRING, PCWSTR};

    use crate::model::LocalTime;
    use crate::paths::to_verbatim;

    /// 100 ns intervals from 1601-01-01 to 1970-01-01.
    const EPOCH_DIFF_100NS: i128 = 116_444_736_000_000_000;

    fn io_err(e: &windows::core::Error) -> io::Error {
        let code = e.code().0 as u32;
        if code & 0xFFFF_0000 == 0x8007_0000 {
            io::Error::from_raw_os_error((code & 0xFFFF) as i32)
        } else {
            io::Error::other(e.message())
        }
    }

    fn wide(path: &Path) -> HSTRING {
        HSTRING::from(to_verbatim(path).as_path())
    }

    pub fn move_file(from: &Path, to: &Path, flags: MOVE_FILE_FLAGS) -> io::Result<()> {
        let (from, to) = (wide(from), wide(to));
        // SAFETY: both strings are NUL-terminated and outlive the call.
        unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), flags) }
            .map_err(|e| io_err(&e))
    }

    pub fn local_to_system_time(t: LocalTime) -> io::Result<SystemTime> {
        let (y, mo, d, h, mi, s) = t.civil();
        let year = u16::try_from(y)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "year out of range"))?;
        let local = SYSTEMTIME {
            wYear: year,
            wMonth: mo as u16,
            wDayOfWeek: 0,
            wDay: d as u16,
            wHour: h as u16,
            wMinute: mi as u16,
            wSecond: s as u16,
            wMilliseconds: 0,
        };
        let mut utc = SYSTEMTIME::default();
        let mut ft = FILETIME::default();
        // SAFETY: all pointers are valid locals.
        unsafe {
            TzSpecificLocalTimeToSystemTime(None, &local, &mut utc).map_err(|e| io_err(&e))?;
            SystemTimeToFileTime(&utc, &mut ft).map_err(|e| io_err(&e))?;
        }
        let ticks =
            ((ft.dwHighDateTime as i128) << 32 | ft.dwLowDateTime as i128) - EPOCH_DIFF_100NS;
        let d = Duration::from_nanos((ticks.unsigned_abs() * 100) as u64);
        let st = if ticks >= 0 {
            SystemTime::UNIX_EPOCH.checked_add(d)
        } else {
            SystemTime::UNIX_EPOCH.checked_sub(d)
        };
        st.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "time out of range"))
    }

    fn to_filetime(t: SystemTime) -> FILETIME {
        let since = match t.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        };
        let ticks = (since / 100 + EPOCH_DIFF_100NS).max(0) as u64;
        FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        }
    }

    pub fn set_file_times(
        path: &Path,
        modified: Option<SystemTime>,
        created: Option<SystemTime>,
    ) -> io::Result<()> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(to_verbatim(path))?;
        let modified = modified.map(to_filetime);
        let created = created.map(to_filetime);
        let handle = HANDLE(file.as_raw_handle());
        // SAFETY: `file` keeps the handle open; the FILETIME values outlive the call.
        unsafe {
            SetFileTime(
                handle,
                created.as_ref().map(|t| t as *const FILETIME),
                None,
                modified.as_ref().map(|t| t as *const FILETIME),
            )
        }
        .map_err(|e| io_err(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_fs::fake::FakeFs;

    fn file_fs(data: &[u8]) -> (FakeFs, Node) {
        let mut fs = FakeFs::new();
        let i = fs.file(0, "IMG_0001.HEIC", data);
        let node = fs.node_mut(i).clone();
        (fs, node)
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn part_names_are_unique_and_recognized() {
        let target = Path::new("dir").join("IMG_0001.HEIC");
        let a = part_path(&target);
        let b = part_path(&target);
        assert_ne!(a, b);
        assert_eq!(a.parent(), target.parent());
        let name = a.file_name().unwrap();
        assert!(name.to_str().unwrap().starts_with("IMG_0001.HEIC."));
        assert!(is_part_file(name));
        assert!(!is_part_file(OsStr::new("IMG_0001.HEIC")));
        assert!(!is_part_file(OsStr::new("notes.part")));
        assert!(!is_part_file(OsStr::new("x.zzzzzzzzzzzzzzzz.part")));
        assert!(is_part_file(OsStr::new("x.0123456789abcdef.part")));
    }

    #[test]
    fn transfer_writes_final_file_and_no_part() {
        let tmp = tempfile::tempdir().unwrap();
        let (fs, node) = file_fs(b"hello");
        let target = tmp.path().join("IMG_0001.HEIC");
        let mut seen = 0;
        let rep = transfer(&fs, &node, &target, false, false, &mut |n| seen += n).unwrap();
        assert_eq!(rep.bytes, 5);
        assert_eq!(seen, 5);
        assert_eq!(rep.verification, Verification::SizeOk);
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        assert_eq!(files_in(tmp.path()), ["IMG_0001.HEIC"]);
    }

    #[test]
    fn transfer_hashes_while_streaming() {
        let tmp = tempfile::tempdir().unwrap();
        let (fs, node) = file_fs(b"hello");
        let target = tmp.path().join("IMG_0001.HEIC");
        let rep = transfer(&fs, &node, &target, false, true, &mut |_| {}).unwrap();
        assert_eq!(rep.verification, Verification::LocalHash);
        assert_eq!(rep.hash, Some(*blake3::hash(b"hello").as_bytes()));
    }

    #[test]
    fn missing_size_is_unverified() {
        let tmp = tempfile::tempdir().unwrap();
        let (fs, mut node) = file_fs(b"hello");
        node.size = None;
        let rep = transfer(&fs, &node, &tmp.path().join("x"), false, false, &mut |_| {}).unwrap();
        assert_eq!(rep.verification, Verification::SizeUnavailable);
    }

    #[test]
    fn size_mismatch_removes_part_and_reports() {
        let tmp = tempfile::tempdir().unwrap();
        for wrong in [4, 6] {
            let (fs, mut node) = file_fs(b"hello");
            node.size = Some(wrong);
            let target = tmp.path().join("x");
            let err = transfer(&fs, &node, &target, false, false, &mut |_| {}).unwrap_err();
            assert!(
                matches!(err, Error::SizeMismatch { written: 5, expected, .. } if expected == wrong)
            );
            assert!(files_in(tmp.path()).is_empty());
        }
    }

    #[test]
    fn read_failure_removes_part() {
        let tmp = tempfile::tempdir().unwrap();
        let mut fs = FakeFs::new();
        let i = fs.file(0, "big.mov", &[7u8; 1000]);
        fs.fail_read(i, 300, false);
        let node = fs.node_mut(i).clone();
        let target = tmp.path().join("big.mov");
        assert!(transfer(&fs, &node, &target, false, false, &mut |_| {}).is_err());
        assert!(files_in(tmp.path()).is_empty());
    }

    #[test]
    fn commit_refuses_existing_target() {
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("a.0123456789abcdef.part");
        let target = tmp.path().join("a");
        std::fs::write(&part, b"new").unwrap();
        std::fs::write(&target, b"old").unwrap();
        let err = commit_no_clobber(&part, &target).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(std::fs::read(&part).unwrap(), b"new");
    }

    #[test]
    fn commit_moves_when_target_is_free() {
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("a.0123456789abcdef.part");
        let target = tmp.path().join("a");
        std::fs::write(&part, b"new").unwrap();
        commit_no_clobber(&part, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!part.exists());
    }

    #[test]
    fn commit_replace_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let part = tmp.path().join("a.0123456789abcdef.part");
        let target = tmp.path().join("a");
        std::fs::write(&part, b"new").unwrap();
        std::fs::write(&target, b"old").unwrap();
        commit_replace(&part, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!part.exists());
    }

    #[test]
    fn transfer_without_replace_keeps_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (fs, node) = file_fs(b"hello");
        let target = tmp.path().join("IMG_0001.HEIC");
        std::fs::write(&target, b"old").unwrap();
        let err = transfer(&fs, &node, &target, false, false, &mut |_| {}).unwrap_err();
        assert!(matches!(err, Error::OutputExists(_)));
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(files_in(tmp.path()), ["IMG_0001.HEIC"]);
    }

    #[test]
    fn leftover_parts_are_listed_not_used() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.0123456789abcdef.part"), b"x").unwrap();
        std::fs::write(tmp.path().join("b.jpg"), b"x").unwrap();
        let parts = leftover_parts(tmp.path());
        assert_eq!(parts, [tmp.path().join("a.0123456789abcdef.part")]);
    }

    #[test]
    fn set_file_times_sets_modified() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        std::fs::write(&path, b"x").unwrap();
        let t = LocalTime::parse("2021-06-15 10:20:30");
        set_file_times(&path, Some(t), Some(t)).unwrap();
        let got = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(got, local_to_system_time(t).unwrap());
    }

    #[test]
    fn long_target_path_works() {
        let tmp = tempfile::tempdir().unwrap();
        let mut dir = crate::paths::normalize_local(tmp.path()).unwrap();
        for i in 0..4 {
            dir.push(format!("{i}{}", "d".repeat(90)));
        }
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join(format!("照片🎉{}.HEIC", "f".repeat(60)));
        assert!(target.as_os_str().len() > 300);
        let (fs, node) = file_fs(b"long");
        transfer(&fs, &node, &target, false, false, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"long");
    }
}
