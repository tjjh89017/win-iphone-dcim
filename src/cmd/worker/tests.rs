use super::*;

#[test]
fn wpd_backend_is_unsupported_off_windows() {
    if !fake::requested() {
        assert!(matches!(backend(), Err(Error::UnsupportedPlatform)));
    }
}
