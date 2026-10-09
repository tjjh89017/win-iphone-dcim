//! Command implementations. They write results to `out` and logs to stderr.

pub mod cp;
pub mod ls;
pub mod tree;

use std::io::Write;

use crate::error::{Error, Result, stdout_err};
use crate::model::DeviceInfo;

/// Print the device list. An empty list is an error.
pub fn devices(list: &[DeviceInfo], out: &mut dyn Write) -> Result<()> {
    if list.is_empty() {
        return Err(Error::NoDevice);
    }
    for d in list {
        writeln!(
            out,
            "[{}] {}  (manufacturer: {}, description: {})",
            d.index,
            d.friendly_name.as_deref().unwrap_or("<no name>"),
            d.manufacturer.as_deref().unwrap_or("-"),
            d.description.as_deref().unwrap_or("-"),
        )
        .map_err(stdout_err)?;
    }
    Ok(())
}

/// Log a per-item error. Return it if it is fatal, so the caller stops.
fn report(err: Error) -> Result<()> {
    if err.is_fatal() {
        return Err(err);
    }
    tracing::error!("{err}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_prints_one_line_per_device() {
        let list = vec![DeviceInfo {
            index: 0,
            friendly_name: Some("Apple iPhone".into()),
            manufacturer: Some("Apple Inc.".into()),
            description: None,
        }];
        let mut out = Vec::new();
        devices(&list, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "[0] Apple iPhone  (manufacturer: Apple Inc., description: -)\n"
        );
    }

    #[test]
    fn no_devices_is_an_error() {
        assert!(matches!(
            devices(&[], &mut Vec::new()),
            Err(Error::NoDevice)
        ));
    }
}
