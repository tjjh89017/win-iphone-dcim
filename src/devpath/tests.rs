use super::*;

#[test]
fn root() {
    let p = DevicePath::parse("/").unwrap();
    assert!(p.is_root());
    assert!(!p.trailing_slash());
    assert_eq!(p.to_string(), "/");
}

#[test]
fn components_keep_spaces_and_unicode() {
    let p = DevicePath::parse("/Internal Storage/DCIM/202601_a/照片.HEIC").unwrap();
    assert_eq!(
        p.components(),
        ["Internal Storage", "DCIM", "202601_a", "照片.HEIC"]
    );
    assert!(!p.trailing_slash());
}

#[test]
fn trailing_slash_is_kept() {
    let p = DevicePath::parse("/Internal Storage/DCIM/").unwrap();
    assert_eq!(p.components(), ["Internal Storage", "DCIM"]);
    assert!(p.trailing_slash());
    assert_eq!(p.normalized(), "/Internal Storage/DCIM");
    assert_eq!(p.to_string(), "/Internal Storage/DCIM/");
}

#[test]
fn leading_slash_is_required() {
    assert_eq!(
        DevicePath::parse("DCIM/202601_a"),
        Err(DevicePathError::NotAbsolute("DCIM/202601_a".into()))
    );
    assert!(DevicePath::parse("").is_err());
}

#[test]
fn empty_components_are_rejected() {
    for bad in ["//", "/a//b", "//a", "/a//"] {
        assert!(
            matches!(
                DevicePath::parse(bad),
                Err(DevicePathError::EmptyComponent(_))
            ),
            "{bad}"
        );
    }
}

#[test]
fn dot_components_are_rejected() {
    assert!(matches!(
        DevicePath::parse("/a/../b"),
        Err(DevicePathError::DotComponent(_))
    ));
    assert!(matches!(
        DevicePath::parse("/./a"),
        Err(DevicePathError::DotComponent(_))
    ));
}
