//! Device discovery and read-only open. Follows DeviceEnumeration.cpp of the
//! Microsoft PortableDeviceCOM sample.

use windows::Win32::Devices::PortableDevices::{
    IPortableDevice, IPortableDeviceManager, IPortableDeviceValues, PortableDeviceFTM,
    PortableDeviceManager, PortableDeviceValues, WPD_CLIENT_DESIRED_ACCESS,
    WPD_CLIENT_MAJOR_VERSION, WPD_CLIENT_MINOR_VERSION, WPD_CLIENT_NAME, WPD_CLIENT_REVISION,
    WPD_CLIENT_SECURITY_QUALITY_OF_SERVICE,
};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Storage::FileSystem::SECURITY_IMPERSONATION;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{PWSTR, w};

use super::com::{WideZ, take_co_wide, wpd_err};
use crate::error::{Result, select_device};
use crate::model::DeviceInfo;

const CLIENT_MAJOR: u32 = 0;
const CLIENT_MINOR: u32 = 1;
const CLIENT_REVISION: u32 = 0;

fn manager() -> Result<IPortableDeviceManager> {
    // SAFETY: COM is initialized on this thread by the caller.
    unsafe { CoCreateInstance(&PortableDeviceManager, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| wpd_err("create PortableDeviceManager", &e, false))
}

/// PnP device IDs in the order `GetDevices` returns them. The device index is this order.
fn device_ids(mgr: &IPortableDeviceManager) -> Result<Vec<WideZ>> {
    let mut count = 0u32;
    // SAFETY: a null array asks for the count only.
    unsafe { mgr.GetDevices(std::ptr::null_mut(), &mut count) }
        .map_err(|e| wpd_err("IPortableDeviceManager::GetDevices (count)", &e, false))?;
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut raw = vec![PWSTR::null(); count as usize];
    let mut got = count;
    // SAFETY: `raw` has room for `count` pointers.
    let res = unsafe { mgr.GetDevices(raw.as_mut_ptr(), &mut got) };
    // Take every returned string so each one is freed, even on error.
    let ids: Vec<WideZ> = raw
        .into_iter()
        .take(got.min(count) as usize)
        // SAFETY: each entry is null or a CoTaskMemAlloc string from GetDevices.
        .filter_map(|p| unsafe { take_co_wide(p) })
        .map(|units| WideZ::new(&units))
        .collect();
    res.map_err(|e| wpd_err("IPortableDeviceManager::GetDevices", &e, false))?;
    Ok(ids)
}

#[derive(Clone, Copy)]
enum ManagerString {
    FriendlyName,
    Manufacturer,
    Description,
}

/// Read a device string with the two-call pattern of the sample. Returns `None` on failure.
fn manager_string(
    mgr: &IPortableDeviceManager,
    id: &WideZ,
    which: ManagerString,
) -> Option<String> {
    let call = |buf: PWSTR, len: &mut u32| -> windows::core::Result<()> {
        // SAFETY: `buf` is null (length query) or has room for `*len` UTF-16 units.
        unsafe {
            match which {
                ManagerString::FriendlyName => mgr.GetDeviceFriendlyName(id.pcwstr(), buf, len),
                ManagerString::Manufacturer => mgr.GetDeviceManufacturer(id.pcwstr(), buf, len),
                ManagerString::Description => mgr.GetDeviceDescription(id.pcwstr(), buf, len),
            }
        }
    };
    let mut len = 0u32;
    if let Err(e) = call(PWSTR::null(), &mut len) {
        tracing::debug!(error = %e, "device string length query failed");
        return None;
    }
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    if let Err(e) = call(PWSTR(buf.as_mut_ptr()), &mut len) {
        tracing::debug!(error = %e, "device string query failed");
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let mgr = manager()?;
    let ids = device_ids(&mgr)?;
    tracing::debug!(count = ids.len(), "WPD devices found");
    Ok(ids
        .iter()
        .enumerate()
        .map(|(index, id)| DeviceInfo {
            index,
            friendly_name: manager_string(&mgr, id, ManagerString::FriendlyName),
            manufacturer: manager_string(&mgr, id, ManagerString::Manufacturer),
            description: manager_string(&mgr, id, ManagerString::Description),
        })
        .collect())
}

fn client_info() -> Result<IPortableDeviceValues> {
    let ctx = "build WPD client information";
    // SAFETY: COM is initialized on this thread; all keys are static PROPERTYKEYs.
    unsafe {
        let values: IPortableDeviceValues =
            CoCreateInstance(&PortableDeviceValues, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| wpd_err(ctx, &e, true))?;
        values
            .SetStringValue(&WPD_CLIENT_NAME, w!("win-iphone-dcim"))
            .and_then(|_| values.SetUnsignedIntegerValue(&WPD_CLIENT_MAJOR_VERSION, CLIENT_MAJOR))
            .and_then(|_| values.SetUnsignedIntegerValue(&WPD_CLIENT_MINOR_VERSION, CLIENT_MINOR))
            .and_then(|_| values.SetUnsignedIntegerValue(&WPD_CLIENT_REVISION, CLIENT_REVISION))
            .and_then(|_| {
                values.SetUnsignedIntegerValue(
                    &WPD_CLIENT_SECURITY_QUALITY_OF_SERVICE,
                    SECURITY_IMPERSONATION.0,
                )
            })
            // Read-only access: this tool never writes to the device.
            .and_then(|_| {
                values.SetUnsignedIntegerValue(&WPD_CLIENT_DESIRED_ACCESS, GENERIC_READ.0)
            })
            .map_err(|e| wpd_err(ctx, &e, true))?;
        Ok(values)
    }
}

/// Open the selected device with read-only access.
pub fn open(selection: Option<usize>) -> Result<(IPortableDevice, Option<String>)> {
    let mgr = manager()?;
    let ids = device_ids(&mgr)?;
    let index = select_device(selection, ids.len())?;
    let id = &ids[index];
    let friendly = manager_string(&mgr, id, ManagerString::FriendlyName);
    let info = client_info()?;
    // SAFETY: COM is initialized on this thread; `id` outlives the call.
    unsafe {
        let device: IPortableDevice =
            CoCreateInstance(&PortableDeviceFTM, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| wpd_err("create PortableDeviceFTM", &e, true))?;
        device
            .Open(id.pcwstr(), &info)
            .map_err(|e| wpd_err(format!("open device {index}"), &e, true))?;
        tracing::info!(index, name = friendly.as_deref(), "device opened read-only");
        Ok((device, friendly))
    }
}
