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
