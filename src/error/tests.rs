use super::*;

#[test]
fn access_denied_maps_to_device_exit_code() {
    let e = Error::from_hresult("open", 0x8007_0005, String::new(), false);
    assert!(matches!(e, Error::AccessDenied { .. }));
    assert_eq!(e.exit_code(), exit::DEVICE);
    assert!(e.to_string().contains("Trust"));
}

#[test]
fn unknown_hresult_depends_on_phase() {
    let open = Error::from_hresult("open", 0x8000_4005, "Unspecified error".into(), true);
    assert!(matches!(open, Error::DeviceOpen { .. }));
    assert_eq!(open.exit_code(), exit::DEVICE);

    let later = Error::from_hresult("enumerate", 0x8000_4005, "Unspecified error".into(), false);
    assert!(matches!(later, Error::Wpd { .. }));
    assert_eq!(later.exit_code(), exit::INTERNAL);
}

#[test]
fn disconnected_is_unavailable() {
    assert_eq!(HresultKind::classify(0x8007_048F), HresultKind::Unavailable);
    assert_eq!(HresultKind::classify(0x8007_001F), HresultKind::Unavailable);
}

#[test]
fn hresult_is_printed_in_hex() {
    let e = Error::from_hresult("read", 0x8007_048F, "gone".into(), false);
    assert!(e.to_string().contains("0x8007048F"), "{e}");
}

#[test]
fn device_selection_rules() {
    assert!(matches!(select_device(None, 0), Err(Error::NoDevice)));
    assert!(matches!(select_device(Some(0), 0), Err(Error::NoDevice)));
    assert_eq!(select_device(None, 1).unwrap(), 0);
    assert!(matches!(
        select_device(None, 2),
        Err(Error::DeviceAmbiguous { count: 2 })
    ));
    assert_eq!(select_device(Some(1), 2).unwrap(), 1);
    let err = select_device(Some(2), 2).unwrap_err();
    assert!(matches!(
        err,
        Error::DeviceIndexOutOfRange { index: 2, count: 2 }
    ));
    assert_eq!(err.exit_code(), exit::DEVICE);
    assert_eq!(Error::DeviceAmbiguous { count: 2 }.exit_code(), exit::CLI);
}

#[test]
fn failure_kinds() {
    assert_eq!(
        Error::NameCollision {
            path: "/a".into(),
            other: "A".into()
        }
        .kind(),
        FailureKind::Collision
    );
    assert_eq!(
        Error::FolderNeedsRecursive("/a".into()).kind(),
        FailureKind::Usage
    );
    assert_eq!(
        Error::from_hresult("read", 0x8007_048F, String::new(), false).kind(),
        FailureKind::Device
    );
}

#[test]
fn transient_and_permanent_errors() {
    let io = |kind| Error::Io {
        context: "write".into(),
        source: std::io::Error::from(kind),
    };
    use std::io::ErrorKind as K;
    assert!(Error::from_hresult("read", 0x8007_00AA, String::new(), false).is_transient());
    assert!(
        Error::WorkerRestarted {
            context: "read".into(),
            reason: "timeout".into()
        }
        .is_transient()
    );
    assert!(io(K::TimedOut).is_transient());
    assert!(io(K::ConnectionReset).is_transient());
    assert!(!io(K::StorageFull).is_transient());
    assert!(!io(K::PermissionDenied).is_transient());
    assert!(!io(K::NotFound).is_transient());
    assert!(!Error::from_hresult("read", 0x8007_0005, String::new(), false).is_transient());
    assert!(
        !Error::PathNotFound {
            path: "/a".into(),
            component: "a".into()
        }
        .is_transient()
    );
    assert!(
        !Error::UnsafeFileName {
            name: "CON".into(),
            reason: "reserved"
        }
        .is_transient()
    );
    assert!(
        !Error::NameCollision {
            path: "/a".into(),
            other: "A".into()
        }
        .is_transient()
    );
}

#[test]
fn no_device_exit_code() {
    assert_eq!(Error::NoDevice.exit_code(), exit::DEVICE);
    assert_eq!(Error::UnsupportedPlatform.exit_code(), exit::INTERNAL);
}
