//! Error types and exit codes. No Windows types here, so the mapping tests run on Linux.

use std::path::PathBuf;

use thiserror::Error;

/// Exit codes per SPEC.md section 5.
pub mod exit {
    pub const OK: i32 = 0;
    pub const FILE_FAILED: i32 = 1;
    pub const CLI: i32 = 2;
    pub const DEVICE: i32 = 3;
    pub const INTERNAL: i32 = 4;
}

const E_ACCESSDENIED: u32 = 0x8007_0005;
const ERROR_NOT_READY: u32 = 0x8007_0015;
const ERROR_GEN_FAILURE: u32 = 0x8007_001F;
const ERROR_SEM_TIMEOUT: u32 = 0x8007_0079;
const ERROR_BUSY: u32 = 0x8007_00AA;
const ERROR_DEVICE_NOT_CONNECTED: u32 = 0x8007_048F;

/// Category of a failed HRESULT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HresultKind {
    /// The device refused access. On an iPhone this usually means locked or not trusted.
    AccessDenied,
    /// The device is gone, busy, or not ready.
    Unavailable,
    Other,
}

impl HresultKind {
    pub fn classify(code: u32) -> Self {
        match code {
            E_ACCESSDENIED => Self::AccessDenied,
            ERROR_NOT_READY
            | ERROR_GEN_FAILURE
            | ERROR_SEM_TIMEOUT
            | ERROR_BUSY
            | ERROR_DEVICE_NOT_CONNECTED => Self::Unavailable,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("no WPD device found; connect the iPhone with a USB cable, unlock it and tap Trust")]
    NoDevice,

    #[error(
        "device index {index} does not exist; {count} device(s) found (run `win-iphone-dcim devices`)"
    )]
    DeviceIndexOutOfRange { index: usize, count: usize },

    #[error("{count} devices found; select one with -d <index> (run `win-iphone-dcim devices`)")]
    DeviceAmbiguous { count: usize },

    #[error(
        "{context}: access denied (HRESULT 0x{code:08X}); unlock the iPhone, tap Trust on the device, then try again"
    )]
    AccessDenied { context: String, code: u32 },

    #[error(
        "{context}: device unavailable (HRESULT 0x{code:08X}: {message}); check the cable and keep the iPhone unlocked"
    )]
    DeviceUnavailable {
        context: String,
        code: u32,
        message: String,
    },

    #[error("{context}: cannot open the device (HRESULT 0x{code:08X}: {message})")]
    DeviceOpen {
        context: String,
        code: u32,
        message: String,
    },

    #[error("{context}: WPD call failed (HRESULT 0x{code:08X}: {message})")]
    Wpd {
        context: String,
        code: u32,
        message: String,
    },

    #[error("{path}: not found (no object named {component:?})")]
    PathNotFound { path: String, component: String },

    #[error("{0}: not a folder")]
    NotAFolder(String),

    #[error("{0}: is a folder; -r is not implemented yet")]
    FolderNeedsRecursive(String),

    #[error("destination must be an existing folder when there are two or more sources: {}", .0.display())]
    DestNotFolder(PathBuf),

    #[error("unsafe file name from the device: {0:?}")]
    UnsafeFileName(String),

    #[error("device file name is not valid UTF-16 (units: {units}); refusing to copy it")]
    InvalidDeviceName { units: String },

    #[error("output file already exists: {}", .0.display())]
    OutputExists(PathBuf),

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{}: size mismatch: wrote {written} bytes, device reported {expected} bytes; file removed", .path.display())]
    SizeMismatch {
        path: PathBuf,
        written: u64,
        expected: u64,
    },

    #[error("WPD is only available on Windows; this build runs on an unsupported platform")]
    #[cfg_attr(windows, allow(dead_code))]
    UnsupportedPlatform,
}

impl Error {
    /// Build an error from a failed HRESULT. Set `opening` while the device is being opened.
    pub fn from_hresult(
        context: impl Into<String>,
        code: u32,
        message: String,
        opening: bool,
    ) -> Self {
        let context = context.into();
        match HresultKind::classify(code) {
            HresultKind::AccessDenied => Self::AccessDenied { context, code },
            HresultKind::Unavailable => Self::DeviceUnavailable {
                context,
                code,
                message,
            },
            HresultKind::Other if opening => Self::DeviceOpen {
                context,
                code,
                message,
            },
            HresultKind::Other => Self::Wpd {
                context,
                code,
                message,
            },
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NoDevice
            | Self::DeviceIndexOutOfRange { .. }
            | Self::AccessDenied { .. }
            | Self::DeviceUnavailable { .. }
            | Self::DeviceOpen { .. } => exit::DEVICE,
            Self::DeviceAmbiguous { .. } | Self::DestNotFolder(_) => exit::CLI,
            Self::PathNotFound { .. }
            | Self::NotAFolder(_)
            | Self::FolderNeedsRecursive(_)
            | Self::UnsafeFileName(_)
            | Self::InvalidDeviceName { .. }
            | Self::OutputExists(_)
            | Self::Io { .. }
            | Self::SizeMismatch { .. } => exit::FILE_FAILED,
            Self::Wpd { .. } | Self::UnsupportedPlatform => exit::INTERNAL,
        }
    }
}

impl Error {
    /// True if the device is gone or refuses access, so later items cannot succeed either.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::AccessDenied { .. } | Self::DeviceUnavailable { .. } | Self::UnsupportedPlatform
        )
    }

    /// True if this is a failed write to a closed pipe, for example `| head`.
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Io { source, .. } if source.kind() == std::io::ErrorKind::BrokenPipe)
    }
}

/// Pick the device index. With no selection, exactly one device must be connected.
pub fn select_device(selection: Option<usize>, count: usize) -> Result<usize> {
    match (selection, count) {
        (_, 0) => Err(Error::NoDevice),
        (None, 1) => Ok(0),
        (None, count) => Err(Error::DeviceAmbiguous { count }),
        (Some(index), count) if index < count => Ok(index),
        (Some(index), count) => Err(Error::DeviceIndexOutOfRange { index, count }),
    }
}

/// Wrap a failed write to stdout.
pub fn stdout_err(source: std::io::Error) -> Error {
    Error::Io {
        context: "write to stdout".into(),
        source,
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_denied_maps_to_device_exit_code() {
        let e = Error::from_hresult("open", 0x8007_0005, String::new(), false);
        assert!(matches!(e, Error::AccessDenied { .. }));
        assert_eq!(e.exit_code(), exit::DEVICE);
        assert!(e.to_string().contains("Trust"));
    }

    #[test]
    fn unknown_hresult_depends_on_phase() {
        let open = Error::from_hresult("open", 0x8000_4005, "Unspecified error".into(), true);
        assert!(matches!(open, Error::DeviceOpen { .. }));
        assert_eq!(open.exit_code(), exit::DEVICE);

        let later =
            Error::from_hresult("enumerate", 0x8000_4005, "Unspecified error".into(), false);
        assert!(matches!(later, Error::Wpd { .. }));
        assert_eq!(later.exit_code(), exit::INTERNAL);
    }

    #[test]
    fn disconnected_is_unavailable() {
        assert_eq!(HresultKind::classify(0x8007_048F), HresultKind::Unavailable);
        assert_eq!(HresultKind::classify(0x8007_001F), HresultKind::Unavailable);
    }

    #[test]
    fn hresult_is_printed_in_hex() {
        let e = Error::from_hresult("read", 0x8007_048F, "gone".into(), false);
        assert!(e.to_string().contains("0x8007048F"), "{e}");
    }

    #[test]
    fn device_selection_rules() {
        assert!(matches!(select_device(None, 0), Err(Error::NoDevice)));
        assert!(matches!(select_device(Some(0), 0), Err(Error::NoDevice)));
        assert_eq!(select_device(None, 1).unwrap(), 0);
        assert!(matches!(
            select_device(None, 2),
            Err(Error::DeviceAmbiguous { count: 2 })
        ));
        assert_eq!(select_device(Some(1), 2).unwrap(), 1);
        let err = select_device(Some(2), 2).unwrap_err();
        assert!(matches!(
            err,
            Error::DeviceIndexOutOfRange { index: 2, count: 2 }
        ));
        assert_eq!(err.exit_code(), exit::DEVICE);
        assert_eq!(Error::DeviceAmbiguous { count: 2 }.exit_code(), exit::CLI);
    }

    #[test]
    fn no_device_exit_code() {
        assert_eq!(Error::NoDevice.exit_code(), exit::DEVICE);
        assert_eq!(Error::UnsupportedPlatform.exit_code(), exit::INTERNAL);
    }
}
