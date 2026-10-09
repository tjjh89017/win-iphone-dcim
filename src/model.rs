//! Plain data types shared by the commands. No Windows types here.

use serde::Serialize;

/// A WPD device as `devices` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceInfo {
    pub index: usize,
    pub friendly_name: Option<String>,
    pub manufacturer: Option<String>,
    pub description: Option<String>,
}

/// A WPD object ID as UTF-16 code units, without the terminating NUL.
/// It goes back to WPD unchanged, even if it is not valid UTF-16.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectId(pub Vec<u16>);

impl ObjectId {
    #[cfg(test)]
    pub fn new(s: &str) -> Self {
        Self(s.encode_utf16().collect())
    }

    /// Text form for output. Debug only: an object ID can change after a reconnection.
    pub fn display(&self) -> String {
        String::from_utf16_lossy(&self.0)
    }
}

/// One object on the device: a folder, a storage, or a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: ObjectId,
    /// `WPD_OBJECT_NAME`.
    pub name: Option<String>,
    /// `WPD_OBJECT_ORIGINAL_FILE_NAME`.
    pub original_file_name: Option<String>,
    /// True for objects that can have children (folders and storages).
    pub is_folder: bool,
    /// `WPD_OBJECT_SIZE` in bytes.
    pub size: Option<u64>,
    /// `WPD_OBJECT_CONTENT_TYPE` as `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`.
    pub content_type: Option<String>,
    /// `WPD_OBJECT_DATE_MODIFIED` as `YYYY-MM-DD HH:MM:SS`, device local time.
    pub modified: Option<String>,
    /// Raw UTF-16 of the original file name, else of the object name.
    /// `cp` builds the local file name from these units, never from a lossy string.
    /// The string fields above are `None` when their value is not valid UTF-16.
    pub raw_file_name: Option<Vec<u16>>,
}

impl Node {
    /// The name to show: original file name, then object name, then object ID.
    pub fn display_name(&self) -> String {
        self.original_file_name
            .clone()
            .or_else(|| self.name.clone())
            .unwrap_or_else(|| match self.raw_file_name {
                Some(_) => format!("<name is not valid UTF-16; object {}>", self.id.display()),
                None => self.id.display(),
            })
    }

    /// True if `component` names this object. Path resolution tries
    /// `Match::OriginalFileName` on all siblings first, then `Match::Name`.
    pub fn matches(&self, component: &str, by: Match) -> bool {
        match by {
            Match::OriginalFileName => self.original_file_name.as_deref() == Some(component),
            Match::Name => self.name.as_deref() == Some(component),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    OriginalFileName,
    Name,
}

/// Join a device folder path and a child name. The root is `/`.
pub fn join_device_path(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// JSONL record of `ls --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LsRecord {
    pub path: String,
    pub name: Option<String>,
    pub original_file_name: Option<String>,
    pub is_folder: bool,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    pub object_id: String,
}

impl LsRecord {
    pub fn new(path: String, node: &Node) -> Self {
        Self {
            path,
            name: node.name.clone(),
            original_file_name: node.original_file_name.clone(),
            is_folder: node.is_folder,
            size: node.size,
            content_type: node.content_type.clone(),
            object_id: node.id.display(),
        }
    }
}

/// JSONL record of `tree --json`. Depth 0 is the root of the listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeRecord {
    pub depth: usize,
    pub path: String,
    pub name: String,
    pub is_folder: bool,
    pub size: Option<u64>,
    pub object_id: String,
}

/// Format a byte count with binary units, for example `3.7 MiB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Convert an OLE automation date (`VT_DATE`, days since 1899-12-30) to
/// `YYYY-MM-DD HH:MM:SS`. Returns `None` for values out of a sane range.
pub fn ole_date_to_string(date: f64) -> Option<String> {
    // 1900-01-01 .. 9999-12-31
    if !date.is_finite() || !(2.0..2_958_466.0).contains(&date) {
        return None;
    }
    const UNIX_EPOCH_OLE_DAYS: f64 = 25_569.0;
    let secs = ((date - UNIX_EPOCH_OLE_DAYS) * 86_400.0).round() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    Some(format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    ))
}

/// Days since 1970-01-01 to (year, month, day). Algorithm by Howard Hinnant.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn ole_date_conversion() {
        assert_eq!(
            ole_date_to_string(25_569.0).as_deref(),
            Some("1970-01-01 00:00:00")
        );
        assert_eq!(
            ole_date_to_string(45_658.5).as_deref(),
            Some("2025-01-01 12:00:00")
        );
        assert_eq!(ole_date_to_string(0.0), None);
        assert_eq!(ole_date_to_string(f64::NAN), None);
    }
}
