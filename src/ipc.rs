//! Protocol between the parent process and the worker process.
//!
//! Control channel: one JSON object per line. The parent writes `Request`s
//! to the worker's stdin. The worker writes `Response`s to its stdout. The
//! worker's stderr carries logs only. One request is in flight at a time.
//!
//! Data channel: the bytes of a `Read` do not go through JSON. The parent
//! creates an anonymous pipe and gives the write end to the worker (see
//! `DATA_PIPE_ENV` and `data_pipe_from_raw`). For each `Read` the worker
//! writes frames to it: a `u32` little-endian length, then that many bytes.
//! No frame is longer than the `chunk_size` of the request. A zero-length
//! frame ends the stream. Then the worker sends `ReadDone` on stdout.
//!
//! Heartbeat: during a `Read` the worker sends `Progress` at most every
//! `PROGRESS_INTERVAL`, between frames. A COM `IStream::Read` call that
//! blocks in the driver cannot send heartbeats or frames. The parent
//! watchdog then sees no activity and kills the worker.
//!
//! Object ids in requests are valid only for the worker process that sent
//! them. Every request that names an object also carries its device path,
//! and the worker resolves the path when it does not know the id.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::device_fs::{self, DeviceFs};
use crate::devpath::DevicePath;
use crate::error::{Error, Result};
use crate::model::{DeviceInfo, LocalTime, Node, ObjectId};

/// Version of this protocol. `Hello` must carry the same value on both sides.
pub const PROTOCOL_VERSION: u32 = 1;

/// Environment variable that carries the data pipe handle when the worker
/// command line cannot carry `--data-pipe` (the test harness).
pub const DATA_PIPE_ENV: &str = "WIN_IPHONE_DCIM_DATA_PIPE";

/// Shortest time between two `Progress` messages.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

/// Largest `chunk_size` the worker accepts.
pub const MAX_CHUNK: u32 = 16 * 1024 * 1024;

/// Longest control line. A folder with many thousands of entries fits.
const MAX_LINE: u64 = 64 * 1024 * 1024;

const E_FAIL: u32 = 0x8000_4005;

/// An object to act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// Device path. The worker resolves it when `id` is absent or unknown.
    pub path: String,
    /// Object id that this worker process returned earlier. Never an id
    /// from an earlier worker process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Vec<u16>>,
}

impl Target {
    /// A target by path only. Tests use it.
    #[cfg(test)]
    pub fn path(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// First request. The worker answers `Hello` with its version.
    Hello {
        protocol_version: u32,
    },
    ListDevices,
    /// Open a device. `None` selects the only connected device.
    Open {
        index: Option<usize>,
    },
    /// Resolve a device path to a node.
    Resolve {
        path: String,
    },
    /// The children of a folder.
    List {
        target: Target,
    },
    /// Fresh properties of an object, resolved by path.
    Stat {
        target: Target,
    },
    /// Stream the data of a file over the data pipe.
    Read {
        target: Target,
        chunk_size: u32,
    },
    /// Exit after this request. There is no response.
    Shutdown,
    /// Sleep without any output, then answer `Hung`. Simulates a blocked COM call.
    #[cfg(test)]
    Hang {
        ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol_version: u32,
    },
    Devices {
        devices: Vec<WireDevice>,
    },
    /// `device_id` is sensitive: the parent hashes it and never logs it.
    Opened {
        root: WireNode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_id: Option<String>,
    },
    Node {
        node: WireNode,
    },
    Nodes {
        nodes: Vec<WireNode>,
    },
    /// Heartbeat during a `Read`. Not a final response.
    Progress {
        bytes_so_far: u64,
    },
    /// Final response of a `Read`, after the end frame. `bytes` is the
    /// number of data bytes sent in frames.
    ReadDone {
        bytes: u64,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<WireError>,
    },
    Error(WireError),
    #[cfg(test)]
    Hung,
}

/// `Node` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireNode {
    pub id: Vec<u16>,
    pub name: Option<String>,
    pub original_file_name: Option<String>,
    pub is_folder: bool,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    pub modified: Option<i64>,
    pub created: Option<i64>,
    pub raw_file_name: Option<Vec<u16>>,
}

impl From<&Node> for WireNode {
    fn from(n: &Node) -> Self {
        Self {
            id: n.id.0.clone(),
            name: n.name.clone(),
            original_file_name: n.original_file_name.clone(),
            is_folder: n.is_folder,
            size: n.size,
            content_type: n.content_type.clone(),
            modified: n.modified.map(|t| t.0),
            created: n.created.map(|t| t.0),
            raw_file_name: n.raw_file_name.clone(),
        }
    }
}

impl From<WireNode> for Node {
    fn from(n: WireNode) -> Self {
        Self {
            id: ObjectId(n.id),
            name: n.name,
            original_file_name: n.original_file_name,
            is_folder: n.is_folder,
            size: n.size,
            content_type: n.content_type,
            modified: n.modified.map(LocalTime),
            created: n.created.map(LocalTime),
            raw_file_name: n.raw_file_name,
        }
    }
}

/// `DeviceInfo` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDevice {
    pub index: usize,
    pub friendly_name: Option<String>,
    pub manufacturer: Option<String>,
    pub description: Option<String>,
}

impl From<&DeviceInfo> for WireDevice {
    fn from(d: &DeviceInfo) -> Self {
        Self {
            index: d.index,
            friendly_name: d.friendly_name.clone(),
            manufacturer: d.manufacturer.clone(),
            description: d.description.clone(),
        }
    }
}

impl From<WireDevice> for DeviceInfo {
    fn from(d: WireDevice) -> Self {
        Self {
            index: d.index,
            friendly_name: d.friendly_name,
            manufacturer: d.manufacturer,
            description: d.description,
        }
    }
}

/// Category of a worker error. The parent rebuilds the same `Error`
/// variant from it, so exit codes and retry decisions do not change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NoDevice,
    DeviceIndexOutOfRange,
    DeviceAmbiguous,
    AccessDenied,
    DeviceUnavailable,
    DeviceOpen,
    Wpd,
    PathNotFound,
    NotAFolder,
    InvalidDeviceName,
    UnsupportedPlatform,
    Other,
}

/// An error on the wire. `message` is the full text for logs. The other
/// fields hold the parts that rebuild the `Error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    pub kind: ErrorKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hresult: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// HRESULT message, path, or invalid name units, by `kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}

impl WireError {
    fn new(kind: ErrorKind, message: String) -> Self {
        Self {
            kind,
            message,
            hresult: None,
            context: None,
            detail: None,
            component: None,
            index: None,
            count: None,
        }
    }

    /// A worker-side failure that has no own `Error` variant.
    pub fn other(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Other, message.into())
    }

    pub fn into_error(self) -> Error {
        let context = self.context.unwrap_or_default();
        let detail = self.detail.unwrap_or_default();
        let code = self.hresult.unwrap_or(E_FAIL);
        match self.kind {
            ErrorKind::NoDevice => Error::NoDevice,
            ErrorKind::DeviceIndexOutOfRange => Error::DeviceIndexOutOfRange {
                index: self.index.unwrap_or_default(),
                count: self.count.unwrap_or_default(),
            },
            ErrorKind::DeviceAmbiguous => Error::DeviceAmbiguous {
                count: self.count.unwrap_or_default(),
            },
            ErrorKind::AccessDenied => Error::AccessDenied { context, code },
            ErrorKind::DeviceUnavailable => Error::DeviceUnavailable {
                context,
                code,
                message: detail,
            },
            ErrorKind::DeviceOpen => Error::DeviceOpen {
                context,
                code,
                message: detail,
            },
            ErrorKind::Wpd => Error::Wpd {
                context,
                code,
                message: detail,
            },
            ErrorKind::PathNotFound => Error::PathNotFound {
                path: detail,
                component: self.component.unwrap_or_default(),
            },
            ErrorKind::NotAFolder => Error::NotAFolder(detail),
            ErrorKind::InvalidDeviceName => Error::InvalidDeviceName { units: detail },
            ErrorKind::UnsupportedPlatform => Error::UnsupportedPlatform,
            ErrorKind::Other => Error::Wpd {
                context: "worker".into(),
                code,
                message: self.message,
            },
        }
    }
}

impl From<&Error> for WireError {
    fn from(e: &Error) -> Self {
        let mut w = Self::new(ErrorKind::Other, e.to_string());
        match e {
            Error::NoDevice => w.kind = ErrorKind::NoDevice,
            Error::DeviceIndexOutOfRange { index, count } => {
                w.kind = ErrorKind::DeviceIndexOutOfRange;
                w.index = Some(*index);
                w.count = Some(*count);
            }
            Error::DeviceAmbiguous { count } => {
                w.kind = ErrorKind::DeviceAmbiguous;
                w.count = Some(*count);
            }
            Error::AccessDenied { context, code } => {
                w.kind = ErrorKind::AccessDenied;
                w.context = Some(context.clone());
                w.hresult = Some(*code);
            }
            Error::DeviceUnavailable {
                context,
                code,
                message,
            }
            | Error::DeviceOpen {
                context,
                code,
                message,
            }
            | Error::Wpd {
                context,
                code,
                message,
            } => {
                w.kind = match e {
                    Error::DeviceUnavailable { .. } => ErrorKind::DeviceUnavailable,
                    Error::DeviceOpen { .. } => ErrorKind::DeviceOpen,
                    _ => ErrorKind::Wpd,
                };
                w.context = Some(context.clone());
                w.hresult = Some(*code);
                w.detail = Some(message.clone());
            }
            Error::PathNotFound { path, component } => {
                w.kind = ErrorKind::PathNotFound;
                w.detail = Some(path.clone());
                w.component = Some(component.clone());
            }
            Error::NotAFolder(path) => {
                w.kind = ErrorKind::NotAFolder;
                w.detail = Some(path.clone());
            }
            Error::InvalidDeviceName { units } => {
                w.kind = ErrorKind::InvalidDeviceName;
                w.detail = Some(units.clone());
            }
            Error::UnsupportedPlatform => w.kind = ErrorKind::UnsupportedPlatform,
            _ => {}
        }
        w
    }
}

fn invalid_data(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// Write one message as a JSON line and flush.
pub fn write_message<T: Serialize>(w: &mut dyn Write, msg: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(invalid_data)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()
}

/// Read one line without the line end. `Ok(None)` at end of input.
pub fn read_line(r: &mut dyn BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let n = r.take(MAX_LINE).read_until(b'\n', &mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(if n as u64 == MAX_LINE {
            invalid_data("control line too long")
        } else {
            io::Error::new(io::ErrorKind::UnexpectedEof, "control line not terminated")
        });
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

/// Parse one control line.
pub fn parse<T: DeserializeOwned>(line: &[u8]) -> io::Result<T> {
    serde_json::from_slice(line).map_err(invalid_data)
}

/// Write one data frame. An empty `data` is the end frame.
pub fn write_frame(w: &mut dyn Write, data: &[u8]) -> io::Result<()> {
    let len = u32::try_from(data.len()).map_err(|_| invalid_data("frame too long"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(data)
}

/// Read one data frame into `buf`. Return `false` for the end frame. A
/// frame longer than `max` is an `InvalidData` error. End of input inside
/// or before a frame is an `UnexpectedEof` error.
pub fn read_frame(r: &mut dyn Read, max: u32, buf: &mut Vec<u8>) -> io::Result<bool> {
    let mut header = [0u8; 4];
    r.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header);
    if len > max {
        return Err(invalid_data(format!(
            "data frame of {len} bytes is longer than the chunk size {max}"
        )));
    }
    buf.clear();
    buf.resize(len as usize, 0);
    r.read_exact(buf)?;
    Ok(len > 0)
}

/// Wrap the write end of the data pipe that the parent passed by number.
///
/// # Safety
/// `raw` must be an open file descriptor (Unix) or handle (Windows) that
/// this process inherited and that nothing else in this process uses.
pub unsafe fn data_pipe_from_raw(raw: u64) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        let fd = i32::try_from(raw).map_err(|_| invalid_data("data pipe fd out of range"))?;
        // SAFETY: the caller guarantees an owned, open fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{FromRawHandle, RawHandle};
        let handle =
            usize::try_from(raw).map_err(|_| invalid_data("data pipe handle out of range"))?;
        // SAFETY: the caller guarantees an owned, open handle.
        Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
    }
}

/// The handle number of a pipe end, as the worker gets it.
#[cfg(unix)]
pub fn raw_pipe(p: &io::PipeWriter) -> u64 {
    use std::os::fd::AsRawFd;
    p.as_raw_fd() as u64
}

/// The handle number of a pipe end, as the worker gets it.
#[cfg(windows)]
pub fn raw_pipe(p: &io::PipeWriter) -> u64 {
    use std::os::windows::io::AsRawHandle;
    p.as_raw_handle() as usize as u64
}

/// What the worker serves: WPD in production, a fake in tests.
pub trait Backend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>>;
    fn open(&self, index: Option<usize>) -> Result<Box<dyn DeviceFs>>;
}

/// Splits the data of a `Read` into frames and sends heartbeats.
struct FrameWriter<'a> {
    data: &'a mut dyn Write,
    control: &'a mut dyn Write,
    buf: Vec<u8>,
    chunk: usize,
    sent: u64,
    last_progress: Instant,
    /// Set when a pipe write fails: the parent is gone.
    broken: Option<io::Error>,
}

impl FrameWriter<'_> {
    fn send_frame(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let result = write_frame(self.data, &self.buf).and_then(|_| self.data.flush());
        if let Err(e) = result {
            let copy = io::Error::new(e.kind(), e.to_string());
            self.broken = Some(e);
            return Err(copy);
        }
        self.sent += self.buf.len() as u64;
        self.buf.clear();
        if self.last_progress.elapsed() >= PROGRESS_INTERVAL {
            self.last_progress = Instant::now();
            let progress = Response::Progress {
                bytes_so_far: self.sent,
            };
            if let Err(e) = write_message(self.control, &progress) {
                let copy = io::Error::new(e.kind(), e.to_string());
                self.broken = Some(e);
                return Err(copy);
            }
        }
        Ok(())
    }
}

impl Write for FrameWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = bytes.len().min(self.chunk - self.buf.len());
        self.buf.extend_from_slice(&bytes[..n]);
        if self.buf.len() == self.chunk {
            self.send_frame()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_frame()
    }
}

/// Worker state for one process.
struct Session {
    fs: Option<Box<dyn DeviceFs>>,
    /// Nodes that this process returned, by object id.
    nodes: HashMap<Vec<u16>, Node>,
}

impl Session {
    fn fs(&self) -> Result<&dyn DeviceFs> {
        self.fs
            .as_deref()
            .ok_or_else(|| WireError::other("no device is open; send Open first").into_error())
    }

    fn remember(&mut self, node: &Node) {
        self.nodes.insert(node.id.0.clone(), node.clone());
    }

    fn resolve(&self, path: &str) -> Result<Node> {
        let parsed = DevicePath::parse(path).map_err(|e| Error::PathNotFound {
            path: path.to_owned(),
            component: e.to_string(),
        })?;
        device_fs::resolve(self.fs()?, &parsed)
    }

    fn target(&self, t: &Target) -> Result<Node> {
        if let Some(node) = t.id.as_ref().and_then(|id| self.nodes.get(id)) {
            return Ok(node.clone());
        }
        self.resolve(&t.path)
    }

    fn handle(&mut self, backend: &dyn Backend, req: Request) -> Result<Response> {
        Ok(match req {
            Request::Hello { protocol_version } if protocol_version == PROTOCOL_VERSION => {
                Response::Hello {
                    protocol_version: PROTOCOL_VERSION,
                }
            }
            Request::Hello { protocol_version } => {
                return Err(WireError::other(format!(
                    "protocol version {protocol_version} is not supported; the worker speaks {PROTOCOL_VERSION}"
                ))
                .into_error());
            }
            Request::ListDevices => Response::Devices {
                devices: backend.list_devices()?.iter().map(Into::into).collect(),
            },
            Request::Open { index } => {
                self.fs = None;
                self.nodes.clear();
                let fs = backend.open(index)?;
                let root = fs.root();
                let device_id = fs.device_id();
                self.fs = Some(fs);
                self.remember(&root);
                Response::Opened {
                    root: (&root).into(),
                    device_id,
                }
            }
            Request::Resolve { path } => {
                let node = self.resolve(&path)?;
                self.remember(&node);
                Response::Node {
                    node: (&node).into(),
                }
            }
            Request::List { target } => {
                let dir = self.target(&target)?;
                if !dir.is_folder {
                    return Err(Error::NotAFolder(target.path));
                }
                let children = self.fs()?.list(&dir)?;
                for c in &children {
                    self.remember(c);
                }
                Response::Nodes {
                    nodes: children.iter().map(Into::into).collect(),
                }
            }
            Request::Stat { target } => {
                let node = self.resolve(&target.path)?;
                self.remember(&node);
                Response::Node {
                    node: (&node).into(),
                }
            }
            Request::Read { .. } | Request::Shutdown => {
                unreachable!("serve handles Read and Shutdown")
            }
            #[cfg(test)]
            Request::Hang { ms } => {
                std::thread::sleep(Duration::from_millis(ms));
                Response::Hung
            }
        })
    }

    /// Send the frames and the end frame. Return the final response, or an
    /// I/O error if a pipe to the parent is broken.
    fn read(
        &self,
        target: &Target,
        chunk_size: u32,
        control: &mut dyn Write,
        data: &mut dyn Write,
    ) -> io::Result<Response> {
        let chunk = chunk_size.clamp(1, MAX_CHUNK) as usize;
        let mut w = FrameWriter {
            data,
            control,
            buf: Vec::with_capacity(chunk),
            chunk,
            sent: 0,
            last_progress: Instant::now(),
            broken: None,
        };
        let result = self
            .target(target)
            .and_then(|node| self.fs()?.read_to(&node, &mut w));
        let flushed = w.send_frame();
        if let Some(e) = w.broken.take() {
            return Err(e);
        }
        flushed?;
        let sent = w.sent;
        write_frame(w.data, &[])?;
        w.data.flush()?;
        Ok(match result {
            Ok(_) => Response::ReadDone {
                bytes: sent,
                ok: true,
                error: None,
            },
            Err(e) => Response::ReadDone {
                bytes: sent,
                ok: false,
                error: Some((&e).into()),
            },
        })
    }
}

/// Serve requests until `Shutdown` or end of input. Device errors go back
/// as `Error` responses. Return an error only when the parent is gone or
/// sent something that is not a request.
pub fn serve(
    backend: &dyn Backend,
    input: &mut dyn BufRead,
    control: &mut dyn Write,
    data: &mut dyn Write,
) -> io::Result<()> {
    let mut session = Session {
        fs: None,
        nodes: HashMap::new(),
    };
    while let Some(line) = read_line(input)? {
        let req: Request = parse(&line)?;
        tracing::debug!(?req, "worker request");
        let response = match req {
            Request::Shutdown => return Ok(()),
            Request::Read { target, chunk_size } => {
                session.read(&target, chunk_size, control, data)?
            }
            other => session
                .handle(backend, other)
                .unwrap_or_else(|e| Response::Error((&e).into())),
        };
        write_message(control, &response)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
