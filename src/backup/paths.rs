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
mod tests;
