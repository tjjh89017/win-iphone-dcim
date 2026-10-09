//! COM apartment lifecycle and small COM helpers.

use std::marker::PhantomData;

use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::core::PWSTR;

use crate::error::{Error, Result};

/// COM is initialized (MTA) on this thread while the value lives.
///
/// The type is `!Send`, so the apartment stays on the thread that made it.
pub struct ComApartment {
    _not_send: PhantomData<*const ()>,
}

impl ComApartment {
    pub fn init_mta() -> Result<Self> {
        // SAFETY: the matching CoUninitialize runs in Drop on the same thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_err() {
            return Err(Error::Wpd {
                context: "CoInitializeEx(COINIT_MULTITHREADED)".into(),
                code: hr.0 as u32,
                message: hr.message(),
            });
        }
        Ok(Self {
            _not_send: PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: CoInitializeEx succeeded on this thread (S_OK or S_FALSE).
        unsafe { CoUninitialize() };
    }
}

/// Convert a `windows` error to our error.
pub fn wpd_err(context: impl Into<String>, err: &windows::core::Error, opening: bool) -> Error {
    Error::from_hresult(context, err.code().0 as u32, err.message(), opening)
}

/// A NUL-terminated UTF-16 buffer to pass as `PCWSTR`.
pub struct WideZ(Vec<u16>);

impl WideZ {
    pub fn new(units: &[u16]) -> Self {
        Self(units.iter().copied().chain(std::iter::once(0)).collect())
    }

    /// The units without the terminating NUL.
    pub fn units(&self) -> &[u16] {
        &self.0[..self.0.len() - 1]
    }

    /// Valid while `self` lives.
    pub fn pcwstr(&self) -> windows::core::PCWSTR {
        windows::core::PCWSTR(self.0.as_ptr())
    }
}

/// Copy a COM-allocated string as UTF-16 units and free it.
///
/// # Safety
/// `p` must be null or a NUL-terminated string from `CoTaskMemAlloc`.
pub unsafe fn take_co_wide(p: PWSTR) -> Option<Vec<u16>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: caller guarantees a valid NUL-terminated string.
    let units = unsafe { p.as_wide() }.to_vec();
    // SAFETY: the string came from CoTaskMemAlloc and is not used after this.
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    Some(units)
}

/// Format a GUID as `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`.
pub fn guid_string(g: &windows::core::GUID) -> String {
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7]
    )
}
