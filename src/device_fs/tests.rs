use super::fake::{FakeFs, dcim};
use super::*;

fn p(s: &str) -> DevicePath {
    DevicePath::parse(s).unwrap()
}

#[test]
fn resolves_root_and_nested_paths() {
    let fs = dcim();
    assert_eq!(resolve(&fs, &p("/")).unwrap().id, fs.root().id);
    let a = resolve(&fs, &p("/Internal Storage/DCIM/202601_a")).unwrap();
    assert!(a.is_folder);
    let f = resolve(&fs, &p("/Internal Storage/DCIM/202601_b/IMG_0001.HEIC")).unwrap();
    assert_eq!(f.size, Some(6));
}

#[test]
fn not_found_names_the_failing_component() {
    let fs = dcim();
    let err = resolve(&fs, &p("/Internal Storage/DCIM/202612_z/IMG.HEIC")).unwrap_err();
    match err {
        Error::PathNotFound { component, .. } => assert_eq!(component, "202612_z"),
        other => panic!("unexpected: {other}"),
    }
}

#[test]
fn original_file_name_wins_over_name() {
    let mut fs = FakeFs::new();
    let x = fs.folder(0, "X");
    fs.node_mut(x).name = Some("IMG_0001".into());
    let y = fs.file(0, "Y", b"y");
    fs.node_mut(y).original_file_name = Some("IMG_0001".into());
    let found = resolve(&fs, &p("/IMG_0001")).unwrap();
    assert_eq!(found.id, fs.node_mut(y).id.clone());
}

#[test]
fn file_with_trailing_slash_is_not_a_folder() {
    let fs = dcim();
    let err = resolve(&fs, &p("/Internal Storage/DCIM/202601_a/IMG_0001.HEIC/")).unwrap_err();
    assert!(matches!(err, Error::NotAFolder(_)));
}
