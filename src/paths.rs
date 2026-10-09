//! Local path handling: verbatim (long and UNC) paths and device file names.
//!
//! Local paths stay `Path`/`OsString` end to end. Device names are converted
//! from their raw UTF-16 units without any lossy step.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use crate::backup::paths::safe_file_name;
use crate::error::{Error, Result};

const BACKSLASH: u16 = b'\\' as u16;
const SLASH: u16 = b'/' as u16;
const COLON: u16 = b':' as u16;
const QUESTION: u16 = b'?' as u16;
const DOT: u16 = b'.' as u16;

/// Add the Windows verbatim prefix to an absolute Windows path, as UTF-16 units.
///
/// - `D:\dir` becomes `\\?\D:\dir`.
/// - `\\server\share\dir` becomes `\\?\UNC\server\share\dir`.
/// - `\\?\...` and `\\.\...` stay unchanged.
/// - Any other form (relative, or rooted without a drive) stays unchanged.
///
/// Verbatim paths skip Win32 normalization, so `/` becomes `\` here. The
/// input must already be absolute and free of `.` and `..` components.
pub fn verbatim_wide(path: &[u16]) -> Vec<u16> {
    let is_sep = |c: u16| c == BACKSLASH || c == SLASH;
    let to_backslash = |c: &u16| if *c == SLASH { BACKSLASH } else { *c };
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    match path {
        [a, b, c, d, ..]
            if is_sep(*a) && is_sep(*b) && (*c == QUESTION || *c == DOT) && is_sep(*d) =>
        {
            path.to_vec()
        }
        [a, b, rest @ ..] if is_sep(*a) && is_sep(*b) => {
            let mut out: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
            out.extend(rest.iter().map(to_backslash));
            out
        }
        [drive, colon, sep, ..] if *colon == COLON && is_sep(*sep) && is_ascii_letter(*drive) => {
            let mut out = prefix;
            out.extend(path.iter().map(to_backslash));
            out
        }
        _ => path.to_vec(),
    }
}

fn is_ascii_letter(c: u16) -> bool {
    u8::try_from(c).is_ok_and(|b| b.is_ascii_alphabetic())
}

/// Return `path` in verbatim form on Windows. Other platforms return it unchanged.
pub fn to_verbatim(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        PathBuf::from(OsString::from_wide(&verbatim_wide(&wide)))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Make a user-given local path absolute, then verbatim on Windows.
/// This gives long-path and UNC support without the LongPathsEnabled setting.
pub fn normalize_local(path: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(path).map_err(|source| Error::Io {
        context: format!("make {} absolute", path.display()),
        source,
    })?;
    Ok(to_verbatim(&abs))
}

/// Convert a device file name (raw UTF-16 from WPD) to a local file name.
/// Reject names that are not valid UTF-16 or that could leave the folder.
pub fn device_name_to_os(units: &[u16]) -> Result<OsString> {
    let text = String::from_utf16(units).map_err(|_| Error::InvalidDeviceName {
        units: units
            .iter()
            .map(|u| format!("{u:04X}"))
            .collect::<Vec<_>>()
            .join(" "),
    })?;
    safe_file_name(&text)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        Ok(OsString::from_wide(units))
    }
    #[cfg(not(windows))]
    {
        Ok(OsString::from(text))
    }
}

#[cfg(test)]
mod tests;
