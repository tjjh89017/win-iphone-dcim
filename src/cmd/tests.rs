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
