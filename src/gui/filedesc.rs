//! The file list that the GUI offers to File Explorer as
//! `CFSTR_FILEDESCRIPTORW`: relative paths, flags, and the byte layout of
//! `FILEGROUPDESCRIPTORW`.
//!
//! This module is portable. The Windows data object in `dataobject` copies
//! the bytes from `Listing::group` into an `HGLOBAL`.

use std::time::SystemTime;

use crate::backup::transfer::local_to_system_time;
use crate::model::LocalTime;

/// `FD_ATTRIBUTES`: `dwFileAttributes` is valid.
pub const FD_ATTRIBUTES: u32 = 0x0000_0004;
/// `FD_CREATETIME`: `ftCreationTime` is valid.
pub const FD_CREATETIME: u32 = 0x0000_0008;
/// `FD_WRITESTIME`: `ftLastWriteTime` is valid.
pub const FD_WRITESTIME: u32 = 0x0000_0020;
/// `FD_FILESIZE`: `nFileSizeHigh` and `nFileSizeLow` are valid.
pub const FD_FILESIZE: u32 = 0x0000_0040;
/// `FD_PROGRESSUI`: Explorer shows its progress dialog.
pub const FD_PROGRESSUI: u32 = 0x0000_4000;
/// `FD_UNICODE`: the names are UTF-16.
pub const FD_UNICODE: u32 = 0x8000_0000;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

/// `MAX_PATH`: the size of `cFileName` in UTF-16 units, NUL included.
pub const MAX_PATH: usize = 260;
/// `sizeof(FILEDESCRIPTORW)`.
pub const DESCRIPTOR_SIZE: usize = 592;
/// Offset of `cFileName` in `FILEDESCRIPTORW`.
const NAME_OFFSET: usize = 72;

/// 100 ns intervals from 1601-01-01 to 1970-01-01.
const EPOCH_DIFF_100NS: i128 = 116_444_736_000_000_000;

/// One object of a paste, from the device thread's walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileItem {
    /// Device path, for the stream request.
    pub path: String,
    /// Path under the common parent of the selection, with `\` separators.
    pub rel: String,
    pub is_folder: bool,
    pub size: Option<u64>,
    pub modified: Option<LocalTime>,
    pub created: Option<LocalTime>,
}

/// The fields of one `FILEDESCRIPTORW` that this tool sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub flags: u32,
    pub attributes: u32,
    /// `FILETIME` ticks (100 ns since 1601-01-01, UTC).
    pub created: Option<u64>,
    pub written: Option<u64>,
    pub size: Option<u64>,
    /// UTF-16 relative path without the NUL.
    pub name: Vec<u16>,
}

/// A file that Explorer can ask a stream for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteFile {
    pub path: String,
    /// The last component of the relative path.
    pub name: String,
    pub size: Option<u64>,
    /// 1-based position among the files of the listing.
    pub number: usize,
}

/// The descriptors of a paste and the files behind them.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    /// The `FILEGROUPDESCRIPTORW` bytes.
    pub group: Vec<u8>,
    /// One entry per descriptor index (`lindex`): `None` for a folder.
    pub entries: Vec<Option<PasteFile>>,
    /// The number of files (not folders).
    pub files: usize,
    /// Items left out, with the reason.
    pub skipped: Vec<String>,
}

impl Listing {
    /// Build the listing. `to_filetime` converts device times to
    /// `FILETIME` ticks; items it cannot convert get no time flag.
    pub fn new(items: &[FileItem], to_filetime: impl Fn(LocalTime) -> Option<u64>) -> Self {
        let mut descriptors = Vec::with_capacity(items.len());
        let mut listing = Self::default();
        for item in items {
            match descriptor(item, &to_filetime) {
                Ok(d) => {
                    descriptors.push(d);
                    listing.entries.push((!item.is_folder).then(|| {
                        listing.files += 1;
                        PasteFile {
                            path: item.path.clone(),
                            name: item.rel.rsplit('\\').next().unwrap_or(&item.rel).to_owned(),
                            size: item.size,
                            number: listing.files,
                        }
                    }));
                }
                Err(e) => listing.skipped.push(e),
            }
        }
        listing.group = group_bytes(&descriptors);
        listing
    }

    /// The number of descriptors.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The file at descriptor index `lindex`. `None` for a folder or an
    /// index out of range.
    pub fn file(&self, lindex: i32) -> Option<&PasteFile> {
        let i = usize::try_from(lindex).ok()?;
        self.entries.get(i)?.as_ref()
    }
}

/// The descriptor of `item`. Fails if the relative path does not fit in
/// `cFileName`.
pub fn descriptor(
    item: &FileItem,
    to_filetime: impl Fn(LocalTime) -> Option<u64>,
) -> Result<Descriptor, String> {
    let name: Vec<u16> = item.rel.encode_utf16().collect();
    if name.len() >= MAX_PATH {
        return Err(format!(
            "{}: the path is longer than {} characters, which File Explorer cannot paste",
            item.path,
            MAX_PATH - 1
        ));
    }
    let mut flags = FD_ATTRIBUTES | FD_PROGRESSUI | FD_UNICODE;
    let (attributes, size) = if item.is_folder {
        (FILE_ATTRIBUTE_DIRECTORY, None)
    } else {
        (FILE_ATTRIBUTE_NORMAL, item.size)
    };
    if size.is_some() {
        flags |= FD_FILESIZE;
    }
    let written = item.modified.and_then(&to_filetime);
    if written.is_some() {
        flags |= FD_WRITESTIME;
    }
    let created = item.created.and_then(&to_filetime);
    if created.is_some() {
        flags |= FD_CREATETIME;
    }
    Ok(Descriptor {
        flags,
        attributes,
        created,
        written,
        size,
        name,
    })
}

/// (`nFileSizeHigh`, `nFileSizeLow`).
pub fn split_size(size: u64) -> (u32, u32) {
    ((size >> 32) as u32, size as u32)
}

/// `FILETIME` ticks of `t`. `None` before 1601.
pub fn filetime(t: SystemTime) -> Option<u64> {
    let since = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    u64::try_from(since / 100 + EPOCH_DIFF_100NS).ok()
}

/// `FILETIME` ticks of a device local time, in this computer's time zone
/// on Windows.
pub fn device_filetime(t: LocalTime) -> Option<u64> {
    local_to_system_time(t).ok().and_then(filetime)
}

/// Append the `FILEDESCRIPTORW` of `d`, little-endian and packed.
fn write_descriptor(d: &Descriptor, out: &mut Vec<u8>) {
    let start = out.len();
    out.resize(start + DESCRIPTOR_SIZE, 0);
    let b = &mut out[start..];
    let put32 = |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |b: &mut [u8], at: usize, v: u64| {
        // FILETIME: dwLowDateTime, then dwHighDateTime.
        put32(b, at, v as u32);
        put32(b, at + 4, (v >> 32) as u32);
    };
    put32(b, 0, d.flags);
    // clsid (4), sizel (20) and pointl (28) stay zero.
    put32(b, 36, d.attributes);
    put64(b, 40, d.created.unwrap_or(0));
    // ftLastAccessTime (48) stays zero.
    put64(b, 56, d.written.unwrap_or(0));
    let (high, low) = split_size(d.size.unwrap_or(0));
    put32(b, 64, high);
    put32(b, 68, low);
    for (i, unit) in d.name.iter().take(MAX_PATH - 1).enumerate() {
        let at = NAME_OFFSET + i * 2;
        b[at..at + 2].copy_from_slice(&unit.to_le_bytes());
    }
}

/// The `FILEGROUPDESCRIPTORW` bytes: `cItems`, then the descriptors.
pub fn group_bytes(descriptors: &[Descriptor]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + descriptors.len() * DESCRIPTOR_SIZE);
    out.extend_from_slice(&(descriptors.len() as u32).to_le_bytes());
    for d in descriptors {
        write_descriptor(d, &mut out);
    }
    out
}

/// The parent of a device path. `/` for a top-level object.
pub fn parent(path: &str) -> &str {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/",
        Some((p, _)) => p,
    }
}

/// The deepest folder that holds all `paths`.
pub fn common_parent(paths: &[String]) -> String {
    let mut common: Option<Vec<&str>> = None;
    for p in paths {
        let parts: Vec<&str> = parent(p).split('/').filter(|c| !c.is_empty()).collect();
        common = Some(match common {
            None => parts,
            Some(c) => c
                .iter()
                .zip(&parts)
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| *a)
                .collect(),
        });
    }
    format!("/{}", common.unwrap_or_default().join("/"))
}

/// `path` under `base`, with `\` separators. `None` if `path` is not
/// below `base`.
pub fn relative(base: &str, path: &str) -> Option<String> {
    let base = base.trim_end_matches('/');
    let rest = path.strip_prefix(base)?.strip_prefix('/')?;
    let rest = rest.trim_end_matches('/');
    (!rest.is_empty()).then(|| rest.replace('/', "\\"))
}

#[cfg(test)]
mod tests;
