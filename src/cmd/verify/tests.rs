use super::*;
use crate::backup::manifest::Record;
use crate::model::Verification;

fn add(m: &mut Manifest, path: &str, data: &[u8], hash: bool) {
    let digest = hash.then(|| *blake3::hash(data).as_bytes());
    m.append(Record::committed(
        None,
        path.into(),
        format!("/DCIM/{path}"),
        data.len() as u64,
        None,
        None,
        if hash {
            Verification::LocalHash
        } else {
            Verification::SizeOk
        },
        digest,
    ))
    .unwrap();
}

fn verify(dest: &Path, hash: bool) -> (String, usize) {
    let mut out = Vec::new();
    let n = run(dest, VerifyOptions { hash }, &mut out).unwrap();
    (String::from_utf8(out).unwrap(), n)
}

#[test]
fn reports_each_category() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("a")).unwrap();
    let mut m = Manifest::load(root).unwrap();
    std::fs::write(root.join("a/ok.HEIC"), b"good").unwrap();
    add(&mut m, "a/ok.HEIC", b"good", true);
    add(&mut m, "a/gone.HEIC", b"gone", false);
    std::fs::write(root.join("a/short.MOV"), b"12").unwrap();
    add(&mut m, "a/short.MOV", b"1234", false);
    std::fs::write(root.join("a/changed.DNG"), b"abcX").unwrap();
    add(&mut m, "a/changed.DNG", b"abcd", true);
    std::fs::write(root.join("a/extra.JPG"), b"x").unwrap();
    std::fs::write(root.join("a/extra.JPG.0123456789abcdef.part"), b"x").unwrap();

    let (out, problems) = verify(root, true);
    assert!(out.contains("[ok] a/ok.HEIC\n"), "{out}");
    assert!(out.contains("[missing] a/gone.HEIC\n"), "{out}");
    assert!(
        out.contains("[size-mismatch] a/short.MOV  local=2 manifest=4"),
        "{out}"
    );
    assert!(out.contains("[hash-mismatch] a/changed.DNG"), "{out}");
    assert!(out.contains("[unrecorded] a/extra.JPG\n"), "{out}");
    assert!(!out.contains(".part"), "{out}");
    assert!(!out.contains("manifest.jsonl"), "{out}");
    assert!(
        out.ends_with("[verify] ok=1 missing=1 size-mismatch=1 hash-mismatch=1 unrecorded=1\n"),
        "{out}"
    );
    assert_eq!(problems, 4);

    // Without --hash the changed file passes the size check.
    let (out, _) = verify(root, false);
    assert!(out.contains("[ok] a/changed.DNG"), "{out}");
}

#[test]
fn all_ok_returns_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let mut m = Manifest::load(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("f"), b"1").unwrap();
    add(&mut m, "f", b"1", true);
    let (out, problems) = verify(tmp.path(), true);
    assert_eq!(problems, 0, "{out}");
    assert!(out.contains("ok=1 missing=0"), "{out}");
}

#[test]
fn missing_dest_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let err = run(
        &tmp.path().join("nope"),
        VerifyOptions::default(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::NotAFolderLocal(_)));
}
