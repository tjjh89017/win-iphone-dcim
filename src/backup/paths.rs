//! Windows file name rules for names that come from the device (SPEC.md section 9).
//!
//! A name that breaks a rule is an error. The tool never renames a file to
//! make it fit.

use std::collections::HashMap;

use crate::error::{Error, Result};

const ILLEGAL: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Base names that Windows reserves, with or without an extension.
const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

/// Check one file or folder name. Return the reason if Windows cannot hold
/// it safely, or if it could leave the destination folder.
pub fn check_name(name: &str) -> std::result::Result<(), &'static str> {
    if name.is_empty() {
        return Err("empty name");
    }
    if name == "." || name == ".." {
        return Err("'.' or '..'");
    }
    if name.contains(['/', '\\']) {
        return Err("path separator");
    }
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err("drive prefix");
    }
    if name.chars().any(|c| c.is_ascii_control()) {
        return Err("control character");
    }
    if name.contains(ILLEGAL) {
        return Err("illegal character");
    }
    if name.ends_with(['.', ' ']) {
        return Err("trailing dot or space");
    }
    if is_reserved(name) {
        return Err("reserved Windows name");
    }
    Ok(())
}

/// True for CON, PRN, AUX, NUL, COM1-9 and LPT1-9, in any case, also with an
/// extension (`NUL.txt`). Windows also reserves the superscript digits 1-3.
fn is_reserved(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = base.to_ascii_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return true;
    }
    let mut chars = upper.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (prefix == "COM" || prefix == "LPT") && matches!(rest.as_slice(), ['1'..='9' | '¹' | '²' | '³'])
}

/// Return `name` if it passes `check_name`, else `Error::UnsafeFileName`.
pub fn safe_file_name(name: &str) -> Result<&str> {
    check_name(name).map_err(|reason| Error::UnsafeFileName {
        name: name.to_owned(),
        reason,
    })?;
    Ok(name)
}

/// Fold case per character, as a case-insensitive file system compares names.
pub fn fold_case(name: &str) -> String {
    name.chars()
        .map(|c| {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

/// For each name, the index of another name in the list that is equal to it
/// after case folding, or `None`. Names in one source folder that collide
/// cannot both exist in a Windows folder, so the caller copies neither.
pub fn case_collisions<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<Option<usize>> {
    let folded: Vec<String> = names.into_iter().map(fold_case).collect();
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in folded.iter().enumerate() {
        groups.entry(f.as_str()).or_default().push(i);
    }
    let mut partner = vec![None; folded.len()];
    for members in groups.values().filter(|m| m.len() > 1) {
        for &i in members {
            partner[i] = members.iter().copied().find(|&j| j != i);
        }
    }
    partner
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(name: &str) -> &'static str {
        check_name(name).unwrap_err()
    }

    #[test]
    fn normal_names_pass() {
        for ok in [
            "IMG_0001.HEIC",
            "202601_a",
            ".hidden",
            "a.b.c",
            "照片 001.HEIC",
            "旅行🗾",
            "CONSOLE.txt",
            "COM10",
            "LPT0",
            "com",
            "NULL",
        ] {
            assert_eq!(check_name(ok), Ok(()), "{ok:?}");
        }
    }

    #[test]
    fn empty_and_dot_names() {
        assert_eq!(reason(""), "empty name");
        assert_eq!(reason("."), "'.' or '..'");
        assert_eq!(reason(".."), "'.' or '..'");
    }

    #[test]
    fn separators() {
        for bad in ["a/b", "a\\b", "/abs", "\\\\server\\share", "../x", "..\\x"] {
            assert_eq!(reason(bad), "path separator", "{bad:?}");
        }
    }

    #[test]
    fn drive_prefix() {
        assert_eq!(reason("C:x"), "drive prefix");
        assert_eq!(reason("d:"), "drive prefix");
    }

    #[test]
    fn illegal_characters() {
        for c in ['<', '>', ':', '"', '|', '?', '*'] {
            assert_eq!(reason(&format!("ab{c}cd")), "illegal character", "{c:?}");
        }
    }

    #[test]
    fn control_characters() {
        for c in ['\0', '\u{1}', '\n', '\t', '\u{1f}', '\u{7f}'] {
            assert_eq!(reason(&format!("a{c}b")), "control character", "{c:?}");
        }
    }

    #[test]
    fn trailing_dot_or_space() {
        assert_eq!(reason("name."), "trailing dot or space");
        assert_eq!(reason("name "), "trailing dot or space");
        assert_eq!(reason("..."), "trailing dot or space");
    }

    #[test]
    fn reserved_names() {
        for bad in [
            "CON",
            "con",
            "Prn",
            "AUX",
            "nul",
            "NUL.txt",
            "con.tar.gz",
            "COM1",
            "com9.log",
            "LPT1",
            "lpt9",
            "COM¹",
            "LPT³.x",
            "AUX .txt",
        ] {
            assert_eq!(reason(bad), "reserved Windows name", "{bad:?}");
        }
    }

    #[test]
    fn safe_file_name_wraps_the_reason() {
        let err = safe_file_name("CON").unwrap_err();
        assert!(matches!(
            err,
            Error::UnsafeFileName {
                reason: "reserved Windows name",
                ..
            }
        ));
        assert_eq!(safe_file_name("IMG.HEIC").unwrap(), "IMG.HEIC");
    }

    #[test]
    fn case_collisions_mark_both_entries() {
        let names = ["IMG_0001.HEIC", "img_0001.heic", "IMG_0002.MOV", "Ä", "ä"];
        let c = case_collisions(names);
        assert_eq!(c, vec![Some(1), Some(0), None, Some(4), Some(3)]);
    }

    #[test]
    fn identical_names_collide() {
        assert_eq!(
            case_collisions(["a", "a", "b"]),
            vec![Some(1), Some(0), None]
        );
    }

    #[test]
    fn three_way_collision_marks_all() {
        let c = case_collisions(["x", "X", "x"]);
        assert!(c.iter().all(Option::is_some));
    }
}
