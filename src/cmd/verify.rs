//! `verify DEST`: check the local files against the manifest
//! (SPEC.md section 5). It reads local files only and never opens a device.

use std::collections::BTreeSet;
use std::io::{ErrorKind, Write};
use std::path::Path;

use crate::backup::manifest::{DIR_NAME, Manifest, hash_file, local_path, relative_path, to_hex};
use crate::error::{Error, Result, stdout_err};
use crate::paths::normalize_local;

#[derive(Debug, Clone, Copy, Default)]
pub struct VerifyOptions {
    /// `--hash`: recompute BLAKE3 where the manifest has a hash.
    pub hash: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    ok: usize,
    missing: usize,
    size_mismatch: usize,
    hash_mismatch: usize,
    unrecorded: usize,
}

impl Counts {
    fn problems(&self) -> usize {
        self.missing + self.size_mismatch + self.hash_mismatch + self.unrecorded
    }
}

/// Verify `dest`. Return the number of files that are not ok.
pub fn run(dest: &Path, opts: VerifyOptions, out: &mut dyn Write) -> Result<usize> {
    let root = normalize_local(dest)?;
    if !root.is_dir() {
        return Err(Error::NotAFolderLocal(root));
    }
    let manifest = Manifest::load(&root)?;
    if manifest.is_empty() {
        tracing::warn!(
            "no manifest records in {}",
            Manifest::path_for(&root).display()
        );
    }
    let mut counts = Counts::default();
    let mut line = |s: String| writeln!(out, "{s}").map_err(stdout_err);
    for entry in manifest.entries() {
        let r = &entry.record;
        let local = local_path(&root, &r.path);
        let size = match std::fs::metadata(&local) {
            Ok(m) if m.is_file() => m.len(),
            Ok(_) => {
                counts.missing += 1;
                line(format!("[missing] {}  not a file", r.path))?;
                continue;
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                counts.missing += 1;
                line(format!("[missing] {}", r.path))?;
                continue;
            }
            Err(e) => {
                counts.missing += 1;
                line(format!("[missing] {}  {e}", r.path))?;
                continue;
            }
        };
        if size != r.size {
            counts.size_mismatch += 1;
            line(format!(
                "[size-mismatch] {}  local={size} manifest={}",
                r.path, r.size
            ))?;
            continue;
        }
        if let Some(stored) = r.blake3().filter(|_| opts.hash) {
            match hash_file(&local) {
                Ok(h) if to_hex(&h).eq_ignore_ascii_case(stored) => {}
                Ok(_) => {
                    counts.hash_mismatch += 1;
                    line(format!("[hash-mismatch] {}", r.path))?;
                    continue;
                }
                Err(e) => {
                    counts.hash_mismatch += 1;
                    line(format!("[hash-mismatch] {}  cannot read: {e}", r.path))?;
                    continue;
                }
            }
        }
        counts.ok += 1;
        line(format!("[ok] {}", r.path))?;
    }
    let mut unrecorded = BTreeSet::new();
    walk(&root, &root, &manifest, &mut unrecorded)?;
    for path in unrecorded {
        counts.unrecorded += 1;
        line(format!("[unrecorded] {path}"))?;
    }
    line(format!(
        "[verify] ok={} missing={} size-mismatch={} hash-mismatch={} unrecorded={}",
        counts.ok, counts.missing, counts.size_mismatch, counts.hash_mismatch, counts.unrecorded
    ))?;
    Ok(counts.problems())
}

/// Collect the files under `dir` that have no record. Skip the tool's own
/// folder and `*.part` files.
fn walk(
    root: &Path,
    dir: &Path,
    manifest: &Manifest,
    unrecorded: &mut BTreeSet<String>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|source| Error::Io {
        context: format!("list {}", dir.display()),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::Io {
            context: format!("list {}", dir.display()),
            source,
        })?;
        let name = entry.file_name();
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| Error::Io {
            context: format!("inspect {}", path.display()),
            source,
        })?;
        if file_type.is_dir() {
            if name != DIR_NAME {
                walk(root, &path, manifest, unrecorded)?;
            }
            continue;
        }
        if name.to_string_lossy().ends_with(".part") {
            continue;
        }
        match relative_path(root, &path) {
            Some(rel) if manifest.get(&rel).is_some() => {}
            Some(rel) => {
                unrecorded.insert(rel);
            }
            None => {
                unrecorded.insert(path.display().to_string());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
