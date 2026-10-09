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
    /// `WPD_OBJECT_DATE_MODIFIED`, device local time.
    pub modified: Option<LocalTime>,
    /// `WPD_OBJECT_DATE_CREATED`, device local time.
    pub created: Option<LocalTime>,
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

/// A device date and time without a time zone: seconds since
/// 1970-01-01 00:00:00 on the device's wall clock. WPD gives dates as
/// `VT_DATE` in device local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalTime(pub i64);

impl LocalTime {
    /// Convert an OLE automation date (`VT_DATE`, days since 1899-12-30).
    /// Returns `None` for values out of a sane range.
    pub fn from_ole(date: f64) -> Option<Self> {
        // 1900-01-01 .. 9999-12-31
        if !date.is_finite() || !(2.0..2_958_466.0).contains(&date) {
            return None;
        }
        const UNIX_EPOCH_OLE_DAYS: f64 = 25_569.0;
        Some(Self(
            ((date - UNIX_EPOCH_OLE_DAYS) * 86_400.0).round() as i64
        ))
    }

    /// (year, month, day, hour, minute, second).
    pub fn civil(self) -> (i64, u32, u32, u32, u32, u32) {
        let days = self.0.div_euclid(86_400);
        let rem = self.0.rem_euclid(86_400) as u32;
        let (y, m, d) = civil_from_days(days);
        (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
    }

    /// Parse `YYYY-MM-DD HH:MM:SS`. Test helper.
    #[cfg(test)]
    pub fn parse(s: &str) -> Self {
        let n: Vec<i64> = s
            .split(['-', ' ', ':'])
            .map(|p| p.parse().unwrap())
            .collect();
        let days = days_from_civil(n[0], n[1] as u32, n[2] as u32);
        Self(days * 86_400 + n[3] * 3600 + n[4] * 60 + n[5])
    }
}

impl std::fmt::Display for LocalTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (y, m, d, hh, mm, ss) = self.civil();
        write!(f, "{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
    }
}

/// (year, month, day) to days since 1970-01-01. Algorithm by Howard Hinnant.
#[cfg(test)]
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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

/// What `cp` does with one planned file when its target is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDecision {
    /// No local file at the target. Copy it.
    Copy,
    /// A local file is at the target and `-f` is set. Copy to a `.part`
    /// file, then replace the target atomically.
    Overwrite,
    /// A local file is at the target. Keep it. `warn` is false with `-n`.
    SkipExists { warn: bool },
}

/// How far the size of a copied file is verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verification {
    /// The byte count matches `WPD_OBJECT_SIZE`.
    SizeOk,
    /// The device gave no size. The copy is not verified.
    SizeUnavailable,
}

impl std::fmt::Display for Verification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SizeOk => "size-ok",
            Self::SizeUnavailable => "size-unavailable",
        })
    }
}

/// Result of one committed file transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferReport {
    pub bytes: u64,
    pub verification: Verification,
    /// True if an existing target file was replaced (`-f`).
    pub replaced: bool,
    pub elapsed: std::time::Duration,
}

/// Category of a per-file failure in the `cp` error summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FailureKind {
    NotFound,
    /// A folder without `-r`, or a file path with a trailing `/`.
    Usage,
    NameUnsafe,
    Collision,
    SizeMismatch,
    TargetExists,
    Io,
    Device,
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotFound => "not found",
            Self::Usage => "usage",
            Self::NameUnsafe => "name unsafe",
            Self::Collision => "collision",
            Self::SizeMismatch => "size mismatch",
            Self::TargetExists => "target exists",
            Self::Io => "io",
            Self::Device => "device",
        })
    }
}

/// Bytes per second as `12.3 MiB/s`.
pub fn human_speed(bytes: u64, elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return "- MiB/s".into();
    }
    format!("{:.1} MiB/s", bytes as f64 / secs / (1024.0 * 1024.0))
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
}
