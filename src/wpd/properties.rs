//! Object properties: name, original file name, content type, size, dates.

use windows::Win32::Devices::PortableDevices::{
    IPortableDeviceKeyCollection, IPortableDeviceProperties, IPortableDeviceValues,
    PortableDeviceKeyCollection, WPD_CONTENT_TYPE_FOLDER, WPD_CONTENT_TYPE_FUNCTIONAL_OBJECT,
    WPD_OBJECT_CONTENT_TYPE, WPD_OBJECT_DATE_CREATED, WPD_OBJECT_DATE_MODIFIED, WPD_OBJECT_NAME,
    WPD_OBJECT_ORIGINAL_FILE_NAME, WPD_OBJECT_SIZE,
};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VT_DATE;

use super::com::{WideZ, guid_string, take_co_wide, wpd_err};
use crate::error::Result;
use crate::model::{LocalTime, Node, ObjectId};

/// The keys that `read` asks for. Build it once and reuse it.
pub fn key_collection() -> Result<IPortableDeviceKeyCollection> {
    let ctx = "build WPD property key collection";
    // SAFETY: COM is initialized on this thread; the keys are static PROPERTYKEYs.
    unsafe {
        let keys: IPortableDeviceKeyCollection =
            CoCreateInstance(&PortableDeviceKeyCollection, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| wpd_err(ctx, &e, false))?;
        for key in [
            &WPD_OBJECT_NAME,
            &WPD_OBJECT_ORIGINAL_FILE_NAME,
            &WPD_OBJECT_CONTENT_TYPE,
            &WPD_OBJECT_SIZE,
            &WPD_OBJECT_DATE_MODIFIED,
            &WPD_OBJECT_DATE_CREATED,
        ] {
            keys.Add(key).map_err(|e| wpd_err(ctx, &e, false))?;
        }
        Ok(keys)
    }
}

/// Read one object. A missing property gives `None`, not an error.
pub fn read(
    props: &IPortableDeviceProperties,
    keys: &IPortableDeviceKeyCollection,
    id: ObjectId,
) -> Result<Node> {
    let wide = WideZ::new(&id.0);
    // SAFETY: COM is initialized on this thread; `wide` outlives the calls.
    unsafe {
        let values = props
            .GetValues(wide.pcwstr(), keys)
            .map_err(|e| wpd_err(format!("GetValues for object {}", id.display()), &e, false))?;
        let raw_name = values
            .GetStringValue(&WPD_OBJECT_NAME)
            .ok()
            .and_then(|p| take_co_wide(p));
        let raw_original = values
            .GetStringValue(&WPD_OBJECT_ORIGINAL_FILE_NAME)
            .ok()
            .and_then(|p| take_co_wide(p));
        // Strict decoding: a name that is not valid UTF-16 stays `None` here.
        let name = raw_name.as_deref().and_then(|u| String::from_utf16(u).ok());
        let original_file_name = raw_original
            .as_deref()
            .and_then(|u| String::from_utf16(u).ok());
        let content_type = values.GetGuidValue(&WPD_OBJECT_CONTENT_TYPE).ok();
        let size = values.GetUnsignedLargeIntegerValue(&WPD_OBJECT_SIZE).ok();
        let modified = date_value(&values, &WPD_OBJECT_DATE_MODIFIED);
        let created = date_value(&values, &WPD_OBJECT_DATE_CREATED);
        // Folders and storages can have children. An object with no content
        // type is treated as a folder so that nothing below it is hidden.
        let is_folder = match content_type {
            None => true,
            Some(t) => t == WPD_CONTENT_TYPE_FOLDER || t == WPD_CONTENT_TYPE_FUNCTIONAL_OBJECT,
        };
        Ok(Node {
            id,
            name,
            original_file_name,
            is_folder,
            size,
            content_type: content_type.as_ref().map(guid_string),
            modified,
            created,
            raw_file_name: raw_original.or(raw_name),
        })
    }
}

/// A date property, if the device gives a `VT_DATE`.
unsafe fn date_value(values: &IPortableDeviceValues, key: &PROPERTYKEY) -> Option<LocalTime> {
    // SAFETY: the key is a static PROPERTYKEY.
    let mut pv = unsafe { values.GetValue(key) }.ok()?;
    // SAFETY: `vt` tells which union member is valid; `date` is read only for VT_DATE.
    let text = unsafe {
        let inner = &pv.Anonymous.Anonymous;
        if inner.vt == VT_DATE {
            LocalTime::from_ole(inner.Anonymous.date)
        } else {
            None
        }
    };
    // SAFETY: `pv` is an initialized PROPVARIANT that we own.
    let _ = unsafe { PropVariantClear(&mut pv) };
    text
}
