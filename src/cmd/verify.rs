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
    if manifest.len() == 0 {
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
mod tests {
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
}
