use super::*;

fn v(s: &str) -> String {
    String::from_utf16(&verbatim_wide(&s.encode_utf16().collect::<Vec<_>>())).unwrap()
}

#[test]
fn drive_path_gets_prefix() {
    assert_eq!(v(r"D:\iPhoneBackup\DCIM"), r"\\?\D:\iPhoneBackup\DCIM");
    assert_eq!(v(r"c:\x"), r"\\?\c:\x");
}

#[test]
fn unc_path_gets_unc_prefix() {
    assert_eq!(v(r"\\server\share\Backup"), r"\\?\UNC\server\share\Backup");
}

#[test]
fn forward_slashes_become_backslashes() {
    assert_eq!(v("D:/a/b"), r"\\?\D:\a\b");
    assert_eq!(v("//server/share/a"), r"\\?\UNC\server\share\a");
}

#[test]
fn verbatim_and_device_paths_are_unchanged() {
    assert_eq!(v(r"\\?\D:\a"), r"\\?\D:\a");
    assert_eq!(v(r"\\?\UNC\server\share"), r"\\?\UNC\server\share");
    assert_eq!(v(r"\\.\COM1"), r"\\.\COM1");
}

#[test]
fn other_forms_are_unchanged() {
    assert_eq!(v(r"relative\dir"), r"relative\dir");
    assert_eq!(v(r"\rooted"), r"\rooted");
    assert_eq!(v("/tmp/x"), "/tmp/x");
    assert_eq!(v("D:relative"), "D:relative");
}

#[test]
fn long_path_keeps_every_unit() {
    let long = format!(r"D:\{}", "a".repeat(300));
    let out = v(&long);
    assert_eq!(out.len(), long.len() + 4);
    assert!(out.starts_with(r"\\?\D:\aaa"));
}

#[test]
fn normalize_local_makes_absolute() {
    let p = normalize_local(Path::new("some/relative")).unwrap();
    assert!(p.is_absolute());
    assert!(p.ends_with("some/relative"));
}

#[test]
fn device_name_round_trips_unicode() {
    let units: Vec<u16> = "照片 001.HEIC".encode_utf16().collect();
    assert_eq!(
        device_name_to_os(&units).unwrap(),
        OsString::from("照片 001.HEIC")
    );
}

#[test]
fn invalid_utf16_name_is_rejected() {
    let units = [0x0041, 0xD800, 0x0042];
    let err = device_name_to_os(&units).unwrap_err();
    assert!(matches!(err, Error::InvalidDeviceName { .. }));
    assert!(err.to_string().contains("D800"), "{err}");
}

#[test]
fn unsafe_names_are_rejected() {
    for bad in ["", ".", "..", "a/b", "a\\b", "C:x", "CON", "a?b", "end."] {
        assert!(safe_file_name(bad).is_err(), "{bad:?}");
        let units: Vec<u16> = bad.encode_utf16().collect();
        assert!(device_name_to_os(&units).is_err(), "{bad:?}");
    }
    assert_eq!(safe_file_name("IMG_0001.HEIC").unwrap(), "IMG_0001.HEIC");
}
