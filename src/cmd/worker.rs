//! Hidden `worker` subcommand: the child process that owns the WPD COM
//! objects (SPEC.md section 8). The parent starts it as
//! `<exe> worker --data-pipe <handle>` and talks to it with the protocol in
//! `ipc`.
//!
//! The worker runs on one thread. `wpd::open` and `wpd::list_devices`
//! initialize COM on this thread, and every COM object stays on it. The
//! worker starts no other thread.

use std::io;

use crate::device_fs::DeviceFs;
use crate::error::{Error, Result};
use crate::ipc::{self, Backend};
use crate::model::DeviceInfo;
use crate::wpd;

/// Serve WPD requests until `Shutdown`, end of input on stdin, or a broken
/// pipe to the parent. `data_pipe` is the handle number of the inherited
/// write end of the data pipe.
pub fn run_worker(data_pipe: u64) -> Result<()> {
    let backend = backend()?;
    // SAFETY: the supervisor created this pipe end for this process only
    // and closed its own copy after the spawn.
    let mut data = unsafe { ipc::data_pipe_from_raw(data_pipe) }.map_err(|source| Error::Io {
        context: "open the data pipe".into(),
        source,
    })?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut control = io::stdout().lock();
    ipc::serve(backend.as_ref(), &mut input, &mut control, &mut data).map_err(|source| Error::Io {
        context: "worker connection to the parent process".into(),
        source,
    })
}

struct WpdBackend;

impl Backend for WpdBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        wpd::list_devices()
    }

    fn open(&self, index: Option<usize>) -> Result<Box<dyn DeviceFs>> {
        wpd::open(index)
    }
}

/// In test builds, `WIN_IPHONE_DCIM_FAKE_FS=1` serves the in-memory fake
/// device. Otherwise WPD, which is available only on Windows.
fn backend() -> Result<Box<dyn Backend>> {
    #[cfg(test)]
    if std::env::var_os("WIN_IPHONE_DCIM_FAKE_FS").is_some_and(|v| v == "1") {
        return Ok(Box::new(fake::FakeBackend));
    }
    if cfg!(windows) {
        Ok(Box::new(WpdBackend))
    } else {
        Err(Error::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod fake {
    use super::*;
    use crate::device_fs::fake::dcim;

    pub struct FakeBackend;

    impl Backend for FakeBackend {
        fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
            Ok(vec![DeviceInfo {
                index: 0,
                friendly_name: Some("Apple iPhone".into()),
                manufacturer: Some("Apple Inc.".into()),
                description: None,
            }])
        }

        fn open(&self, _: Option<usize>) -> Result<Box<dyn DeviceFs>> {
            Ok(Box::new(dcim()))
        }
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn wpd_backend_is_unsupported_off_windows() {
        if std::env::var_os("WIN_IPHONE_DCIM_FAKE_FS").is_none() {
            assert!(matches!(backend(), Err(Error::UnsupportedPlatform)));
        }
    }
}
