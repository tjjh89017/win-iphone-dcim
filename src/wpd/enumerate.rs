//! Children of one object. Follows ContentEnumeration.cpp of the Microsoft
//! PortableDeviceCOM sample.

use windows::Win32::Devices::PortableDevices::{
    IPortableDeviceContent, IPortableDeviceKeyCollection, IPortableDeviceProperties,
};
use windows::Win32::Foundation::S_OK;
use windows::core::PWSTR;

use super::com::{WideZ, take_co_wide, wpd_err};
use super::properties;
use crate::error::{Error, Result};
use crate::model::{Node, ObjectId};

/// Object IDs requested per `IEnumPortableDeviceObjectIDs::Next` call.
const BATCH: usize = 32;

/// List the direct children of `parent`, reading their properties.
pub fn children(
    content: &IPortableDeviceContent,
    props: &IPortableDeviceProperties,
    keys: &IPortableDeviceKeyCollection,
    parent: &ObjectId,
) -> Result<Vec<Node>> {
    let ctx = || format!("enumerate children of {}", parent.display());
    let wide = WideZ::new(&parent.0);
    // SAFETY: COM is initialized on this thread; `wide` outlives the call.
    let ids = unsafe { content.EnumObjects(0, wide.pcwstr(), None) }
        .map_err(|e| wpd_err(ctx(), &e, false))?;
    let mut nodes = Vec::new();
    loop {
        let mut raw = [PWSTR::null(); BATCH];
        let mut fetched = 0u32;
        // SAFETY: `raw` has BATCH slots; the enumerator fills `fetched` of them.
        let hr = unsafe { ids.Next(&mut raw, &mut fetched) };
        // Take every returned string so each one is freed, even on error.
        let batch: Vec<ObjectId> = raw
            .into_iter()
            .take((fetched as usize).min(BATCH))
            // SAFETY: each entry is null or a CoTaskMemAlloc string from Next.
            .filter_map(|p| unsafe { take_co_wide(p) })
            .map(ObjectId)
            .collect();
        if hr.is_err() {
            return Err(Error::from_hresult(
                format!("IEnumPortableDeviceObjectIDs::Next ({})", ctx()),
                hr.0 as u32,
                hr.message(),
                false,
            ));
        }
        for id in batch {
            nodes.push(properties::read(props, keys, id)?);
        }
        // S_FALSE means the enumerator returned the last objects.
        if hr != S_OK || fetched == 0 {
            return Ok(nodes);
        }
    }
}
