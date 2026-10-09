//! Error types and exit codes. No Windows types here, so the mapping tests run on Linux.

use std::path::PathBuf;

use thiserror::Error;

use crate::model::FailureKind;

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

    #[error("{0}: is a folder; use -r to copy folders")]
    FolderNeedsRecursive(String),

    #[error("destination must be an existing folder when there are two or more sources: {}", .0.display())]
    DestNotFolder(PathBuf),

    #[error("unsafe file name from the device: {name:?} ({reason})")]
    UnsafeFileName { name: String, reason: &'static str },

    #[error(
        "{path}: name differs only by case from {other:?} in the same folder; neither is copied"
    )]
    NameCollision { path: String, other: String },

    #[error("{}: exists and is not a folder", .0.display())]
    NotAFolderLocal(PathBuf),

    #[error("{}: a folder is at the target file path", .0.display())]
    TargetIsFolder(PathBuf),

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

    /// The worker process that owns the WPD COM objects stopped or hung and
    /// was restarted while it served this request. `RemoteFs` returns it. The
    /// request can succeed on a new attempt, so it is a transient error.
    #[error("{context}: the device worker was restarted ({reason})")]
    WorkerRestarted { context: String, reason: String },

    #[error(
        "the device worker program is missing: {}; keep win-iphone-dcim.exe in the same folder as the GUI",
        .0.display()
    )]
    WorkerMissing(PathBuf),

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
            | Self::UnsafeFileName { .. }
            | Self::NameCollision { .. }
            | Self::NotAFolderLocal(_)
            | Self::TargetIsFolder(_)
            | Self::InvalidDeviceName { .. }
            | Self::OutputExists(_)
            | Self::Io { .. }
            | Self::SizeMismatch { .. } => exit::FILE_FAILED,
            Self::Wpd { .. }
            | Self::WorkerRestarted { .. }
            | Self::WorkerMissing(_)
            | Self::UnsupportedPlatform => exit::INTERNAL,
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

    /// True if a new attempt can succeed: the device is busy or gone for a
    /// moment, an I/O call timed out, a network (SMB) write failed, or the
    /// worker was restarted. Not found, unsafe names, collisions, a full disk
    /// and access denied are permanent (SPEC.md section 8).
    pub fn is_transient(&self) -> bool {
        match self {
            Self::DeviceUnavailable { .. } | Self::WorkerRestarted { .. } => true,
            Self::Io { source, .. } => is_transient_io(source),
            _ => false,
        }
    }

    /// Category for the `cp` error summary.
    pub fn kind(&self) -> FailureKind {
        match self {
            Self::PathNotFound { .. } => FailureKind::NotFound,
            Self::NotAFolder(_) | Self::FolderNeedsRecursive(_) => FailureKind::Usage,
            Self::UnsafeFileName { .. } | Self::InvalidDeviceName { .. } => FailureKind::NameUnsafe,
            Self::NameCollision { .. } => FailureKind::Collision,
            Self::SizeMismatch { .. } => FailureKind::SizeMismatch,
            Self::OutputExists(_) | Self::NotAFolderLocal(_) | Self::TargetIsFolder(_) => {
                FailureKind::TargetExists
            }
            Self::Io { .. } => FailureKind::Io,
            Self::WorkerRestarted { .. } => FailureKind::Worker,
            _ => FailureKind::Device,
        }
    }

    /// True if this is a failed write to a closed pipe, for example `| head`.
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Io { source, .. } if source.kind() == std::io::ErrorKind::BrokenPipe)
    }
}

/// Windows error codes of a failed network or SMB operation, or a timeout.
const TRANSIENT_OS_ERRORS: [i32; 6] = [
    59,   // ERROR_UNEXP_NET_ERR
    64,   // ERROR_NETNAME_DELETED
    121,  // ERROR_SEM_TIMEOUT
    1231, // ERROR_NETWORK_UNREACHABLE
    1236, // ERROR_CONNECTION_ABORTED
    1460, // ERROR_TIMEOUT
];

fn is_transient_io(e: &std::io::Error) -> bool {
    use std::io::ErrorKind as K;
    if cfg!(windows)
        && e.raw_os_error()
            .is_some_and(|c| TRANSIENT_OS_ERRORS.contains(&c))
    {
        return true;
    }
    matches!(
        e.kind(),
        K::TimedOut
            | K::Interrupted
            | K::ConnectionReset
            | K::ConnectionAborted
            | K::NetworkDown
            | K::NetworkUnreachable
            | K::HostUnreachable
    )
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
mod tests;
