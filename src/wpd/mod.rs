//! Windows Portable Devices backend.
//!
//! All COM code is `cfg(windows)`. On other platforms the same functions
//! return `Error::UnsupportedPlatform`. COM is initialized (MTA) on the
//! calling thread, and every COM object stays on that thread: the returned
//! backend is `!Send`.

use crate::device_fs::DeviceFs;
use crate::error::Result;
use crate::model::DeviceInfo;

#[cfg(windows)]
mod com;
#[cfg(windows)]
mod device;
#[cfg(windows)]
mod enumerate;
#[cfg(windows)]
mod properties;
#[cfg(windows)]
mod stream;

/// Lower limit for the transfer buffer.
pub const MIN_BUFFER: usize = 64 * 1024;
/// Upper limit for the transfer buffer.
pub const MAX_BUFFER: usize = 4 * 1024 * 1024;

/// Clamp the driver's optimal buffer size to `MIN_BUFFER..=MAX_BUFFER`.
pub fn clamp_buffer_size(optimal: u32) -> usize {
    usize::try_from(optimal)
        .unwrap_or(MAX_BUFFER)
        .clamp(MIN_BUFFER, MAX_BUFFER)
}

/// List the WPD devices.
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    #[cfg(windows)]
    {
        let _com = com::ComApartment::init_mta()?;
        device::list_devices()
    }
    #[cfg(not(windows))]
    {
        Err(crate::error::Error::UnsupportedPlatform)
    }
}

/// Open a device read-only. With no selection, exactly one device must be connected.
pub fn open(selection: Option<usize>) -> Result<Box<dyn DeviceFs>> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows_backend::WpdFs::open(selection)?))
    }
    #[cfg(not(windows))]
    {
        let _ = selection;
        Err(crate::error::Error::UnsupportedPlatform)
    }
}

#[cfg(windows)]
mod windows_backend {
    use std::io::Write;

    use windows::Win32::Devices::PortableDevices::{
        IPortableDevice, IPortableDeviceContent, IPortableDeviceKeyCollection,
        IPortableDeviceProperties, IPortableDeviceResources, WPD_DEVICE_OBJECT_ID,
    };

    use super::com::{ComApartment, wpd_err};
    use super::{device, enumerate, properties, stream};
    use crate::device_fs::DeviceFs;
    use crate::error::Result;
    use crate::model::{Node, ObjectId};

    /// An open device. Fields drop in order, so the COM objects are released
    /// before the apartment is closed.
    pub struct WpdFs {
        content: IPortableDeviceContent,
        props: IPortableDeviceProperties,
        resources: IPortableDeviceResources,
        keys: IPortableDeviceKeyCollection,
        _device: IPortableDevice,
        friendly_name: Option<String>,
        _com: ComApartment,
    }

    impl WpdFs {
        pub fn open(selection: Option<usize>) -> Result<Self> {
            let com = ComApartment::init_mta()?;
            let (device, friendly_name) = device::open(selection)?;
            // SAFETY: COM is initialized on this thread.
            unsafe {
                let content = device
                    .Content()
                    .map_err(|e| wpd_err("IPortableDevice::Content", &e, true))?;
                let props = content
                    .Properties()
                    .map_err(|e| wpd_err("IPortableDeviceContent::Properties", &e, true))?;
                let resources = content
                    .Transfer()
                    .map_err(|e| wpd_err("IPortableDeviceContent::Transfer", &e, true))?;
                let keys = properties::key_collection()?;
                Ok(Self {
                    content,
                    props,
                    resources,
                    keys,
                    _device: device,
                    friendly_name,
                    _com: com,
                })
            }
        }
    }

    impl DeviceFs for WpdFs {
        fn root(&self) -> Node {
            // SAFETY: WPD_DEVICE_OBJECT_ID is a static NUL-terminated string.
            let id = ObjectId(unsafe { WPD_DEVICE_OBJECT_ID.as_wide() }.to_vec());
            Node {
                id,
                name: self.friendly_name.clone(),
                original_file_name: None,
                is_folder: true,
                size: None,
                content_type: None,
                modified: None,
                created: None,
                raw_file_name: None,
            }
        }

        fn list(&self, dir: &Node) -> Result<Vec<Node>> {
            enumerate::children(&self.content, &self.props, &self.keys, &dir.id)
        }

        fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64> {
            stream::read_to(&self.resources, &file.id, out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_size_is_clamped() {
        assert_eq!(clamp_buffer_size(0), MIN_BUFFER);
        assert_eq!(clamp_buffer_size(4096), MIN_BUFFER);
        assert_eq!(clamp_buffer_size(256 * 1024), 256 * 1024);
        assert_eq!(clamp_buffer_size(u32::MAX), MAX_BUFFER);
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_reports_unsupported_platform() {
        assert!(matches!(
            list_devices(),
            Err(crate::error::Error::UnsupportedPlatform)
        ));
        assert!(matches!(
            open(None),
            Err(crate::error::Error::UnsupportedPlatform)
        ));
    }
}
