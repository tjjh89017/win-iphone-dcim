use super::*;

fn record(path: &str, size: u64) -> Record {
    Record::committed(
        Some(device_key("raw-id")),
        path.into(),
        format!("/DCIM/{path}"),
        size,
        None,
        None,
        Verification::SizeOk,
        None,
    )
}

#[test]
fn append_and_load_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut m = Manifest::load(tmp.path()).unwrap();
    assert_eq!(m.len(), 0);
    m.append(record("a/x.HEIC", 3)).unwrap();
    m.append(record("a/y.MOV", 5)).unwrap();
    m.append(record("a/x.HEIC", 4)).unwrap();
    let text = std::fs::read_to_string(Manifest::path_for(tmp.path())).unwrap();
    assert_eq!(text.lines().count(), 3);
    let m = Manifest::load(tmp.path()).unwrap();
    assert_eq!(m.len(), 2);
    // The later record wins.
    assert_eq!(m.get("a/x.HEIC").unwrap().record.size, 4);
}

#[test]
fn record_json_has_spec_fields() {
    let mut r = record("DCIM/a/IMG_0001.HEIC", 6);
    r.hash_alg = Some(HASH_ALG.into());
    r.hash = Some("ab".into());
    r.modified = Some("2024-01-02 03:04:05".into());
    let v: serde_json::Value = serde_json::to_value(&r).unwrap();
    assert_eq!(v["v"], 1);
    assert_eq!(v["path"], "DCIM/a/IMG_0001.HEIC");
    assert_eq!(v["verification"], "size");
    assert_eq!(v["hash_alg"], "blake3");
    assert_eq!(v["device"].as_str().unwrap().len(), 16);
    assert!(v["committed_at"].as_str().unwrap().ends_with('Z'));
    assert!(v.get("created").is_none());
    let unavailable = serde_json::to_value(Verification::SizeUnavailable).unwrap();
    assert_eq!(unavailable, "size-unavailable");
    let hashed = serde_json::to_value(Verification::LocalHash).unwrap();
    assert_eq!(hashed, "local-hash");
}

#[test]
fn truncated_last_line_is_skipped_and_next_append_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let mut m = Manifest::load(tmp.path()).unwrap();
    m.append(record("a", 1)).unwrap();
    let path = Manifest::path_for(tmp.path());
    let mut f = OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(br#"{"v":1,"path":"b","si"#).unwrap();
    drop(f);
    let mut m = Manifest::load(tmp.path()).unwrap();
    assert_eq!(m.len(), 1);
    assert!(m.get("a").is_some());
    m.append(record("c", 2)).unwrap();
    let m = Manifest::load(tmp.path()).unwrap();
    assert_eq!(m.len(), 2);
    assert!(m.get("c").is_some());
}

#[test]
fn invalid_middle_line_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(DIR_NAME)).unwrap();
    let good = serde_json::to_string(&record("a", 1)).unwrap();
    std::fs::write(
        Manifest::path_for(tmp.path()),
        format!("{good}\nnot json\n\n{}\n", good.replace("\"a\"", "\"b\"")),
    )
    .unwrap();
    let m = Manifest::load(tmp.path()).unwrap();
    assert_eq!(m.len(), 2);
}

#[test]
fn reconcile_marks_missing_and_resized_files_stale() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("d")).unwrap();
    std::fs::write(tmp.path().join("d/ok"), b"123").unwrap();
    std::fs::write(tmp.path().join("d/resized"), b"12").unwrap();
    let mut m = Manifest::load(tmp.path()).unwrap();
    m.append(record("d/ok", 3)).unwrap();
    m.append(record("d/resized", 3)).unwrap();
    m.append(record("d/missing", 3)).unwrap();
    let counts = m.reconcile();
    assert_eq!(counts, Reconciled { ok: 1, stale: 2 });
    assert!(m.get("d/ok").unwrap().stale.is_none());
    assert!(
        m.get("d/missing")
            .unwrap()
            .stale
            .as_deref()
            .unwrap()
            .contains("missing")
    );
}

#[test]
fn relative_and_local_paths() {
    let root = Path::new("/backup");
    assert_eq!(
        relative_path(root, &root.join("DCIM").join("a.HEIC")).as_deref(),
        Some("DCIM/a.HEIC")
    );
    assert_eq!(relative_path(root, Path::new("/other/a")), None);
    assert_eq!(relative_path(root, root), None);
    assert_eq!(local_path(root, "../x/./y"), root.join("x").join("y"));
}

#[test]
fn device_key_hides_raw_id() {
    let raw = r"\\?\usb#vid_05ac&pid_12a8#00008030001234567890abcd#{6ac27878}";
    let key = device_key(raw);
    assert_eq!(key.len(), 16);
    assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(!raw.contains(&key));
    assert_eq!(device_log_key(&key).len(), 8);
    assert_eq!(key, device_key(raw));
}

#[test]
fn hash_file_matches_blake3() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("f");
    std::fs::write(&p, b"hello").unwrap();
    assert_eq!(hash_file(&p).unwrap(), *blake3::hash(b"hello").as_bytes());
    assert_eq!(to_hex(&[0, 255, 16]), "00ff10");
}
