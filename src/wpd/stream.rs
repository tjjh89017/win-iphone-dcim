//! Object data download. Follows ContentTransfer.cpp of the Microsoft
//! PortableDeviceCOM sample.

use std::io::Write;

use windows::Win32::Devices::PortableDevices::{IPortableDeviceResources, WPD_RESOURCE_DEFAULT};
use windows::Win32::System::Com::{IStream, STGM_READ};

use super::clamp_buffer_size;
use super::com::{WideZ, wpd_err};
use crate::error::{Error, Result};
use crate::model::ObjectId;

/// Stream `WPD_RESOURCE_DEFAULT` of `id` to `out`. Return the bytes written.
pub fn read_to(
    resources: &IPortableDeviceResources,
    id: &ObjectId,
    out: &mut dyn Write,
) -> Result<u64> {
    let wide = WideZ::new(&id.0);
    let ctx = format!("GetStream for object {}", id.display());
    let mut optimal = 0u32;
    let mut stream: Option<IStream> = None;
    // SAFETY: `wide` outlives the call; the out-pointers are valid locals.
    unsafe {
        resources.GetStream(
            wide.pcwstr(),
            &WPD_RESOURCE_DEFAULT,
            STGM_READ.0,
            &mut optimal,
            &mut stream,
        )
    }
    .map_err(|e| wpd_err(ctx.as_str(), &e, false))?;
    let stream = stream.ok_or_else(|| Error::Wpd {
        context: ctx,
        code: 0,
        message: "no stream returned".into(),
    })?;

    let buffer_size = clamp_buffer_size(optimal);
    tracing::debug!(optimal, buffer_size, "transfer buffer");
    let mut buf = vec![0u8; buffer_size];
    let mut total: u64 = 0;
    loop {
        let mut read = 0u32;
        // SAFETY: `buf` has `buffer_size` bytes; buffer_size <= 4 MiB fits in u32.
        let hr =
            unsafe { stream.Read(buf.as_mut_ptr().cast(), buffer_size as u32, Some(&mut read)) };
        if hr.is_err() {
            return Err(Error::from_hresult(
                format!("IStream::Read after {total} bytes"),
                hr.0 as u32,
                hr.message(),
                false,
            ));
        }
        // Only a read of 0 bytes is end of stream.
        if read == 0 {
            return Ok(total);
        }
        let n = (read as usize).min(buffer_size);
        out.write_all(&buf[..n]).map_err(|source| Error::Io {
            context: "write local file".into(),
            source,
        })?;
        total += n as u64;
    }
}
