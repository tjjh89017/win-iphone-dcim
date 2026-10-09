use super::*;
use serde_json::{Value, json};

fn file_node() -> Node {
    Node {
        id: ObjectId::new("o42"),
        name: Some("IMG_0001".into()),
        original_file_name: Some("IMG_0001.MOV".into()),
        is_folder: false,
        size: Some(5 * 1024 * 1024 * 1024),
        content_type: Some("9261B03C-3D78-4519-85E3-02C5E1F50BB9".into()),
        modified: None,
        created: None,
        raw_file_name: Some("IMG_0001.MOV".encode_utf16().collect()),
    }
}

#[test]
fn ls_record_serializes_expected_fields() {
    let rec = LsRecord::new(
        "/Internal Storage/DCIM/202601_a/IMG_0001.MOV".into(),
        &file_node(),
    );
    let line = serde_json::to_string(&rec).unwrap();
    assert!(!line.contains('\n'));
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        value,
        json!({
            "path": "/Internal Storage/DCIM/202601_a/IMG_0001.MOV",
            "name": "IMG_0001",
            "original_file_name": "IMG_0001.MOV",
            "is_folder": false,
            "size": 5_368_709_120u64,
            "content_type": "9261B03C-3D78-4519-85E3-02C5E1F50BB9",
            "object_id": "o42"
        })
    );
}

#[test]
fn tree_record_has_null_size_for_folders() {
    let rec = TreeRecord {
        depth: 1,
        path: "/DCIM".into(),
        name: "DCIM".into(),
        is_folder: true,
        size: None,
        object_id: "o1".into(),
    };
    let value = serde_json::to_value(rec).unwrap();
    assert_eq!(value["size"], Value::Null);
    assert_eq!(value["depth"], json!(1));
}

#[test]
fn display_name_prefers_original_file_name() {
    let mut n = file_node();
    assert_eq!(n.display_name(), "IMG_0001.MOV");
    n.original_file_name = None;
    assert_eq!(n.display_name(), "IMG_0001");
    n.name = None;
    assert!(n.display_name().contains("not valid UTF-16"));
    n.raw_file_name = None;
    assert_eq!(n.display_name(), "o42");
}

#[test]
fn join_device_path_handles_root() {
    assert_eq!(
        join_device_path("/", "Internal Storage"),
        "/Internal Storage"
    );
    assert_eq!(
        join_device_path("/Internal Storage", "DCIM"),
        "/Internal Storage/DCIM"
    );
}

#[test]
fn human_size_uses_binary_units() {
    assert_eq!(human_size(0), "0 B");
    assert_eq!(human_size(1023), "1023 B");
    assert_eq!(human_size(1024), "1.0 KiB");
    assert_eq!(human_size(3_879_731), "3.7 MiB");
    assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GiB");
}

fn ole(date: f64) -> Option<String> {
    LocalTime::from_ole(date).map(|t| t.to_string())
}

#[test]
fn ole_date_conversion() {
    assert_eq!(ole(25_569.0).as_deref(), Some("1970-01-01 00:00:00"));
    assert_eq!(ole(45_658.5).as_deref(), Some("2025-01-01 12:00:00"));
    assert_eq!(ole(0.0), None);
    assert_eq!(ole(f64::NAN), None);
}

#[test]
fn speed_is_in_mib_per_second() {
    let d = std::time::Duration::from_secs(2);
    assert_eq!(human_speed(4 * 1024 * 1024, d), "2.0 MiB/s");
    assert_eq!(human_speed(1, std::time::Duration::ZERO), "- MiB/s");
    assert_eq!(
        Verification::SizeUnavailable.to_string(),
        "size-unavailable"
    );
}

#[test]
fn local_time_parse_round_trips() {
    for s in [
        "1970-01-01 00:00:00",
        "2024-02-29 23:59:59",
        "1999-12-31 08:07:06",
    ] {
        assert_eq!(LocalTime::parse(s).to_string(), s);
    }
}
