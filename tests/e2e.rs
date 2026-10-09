//! End-to-end tests of the built program. Each test runs the executable
//! with `WIN_IPHONE_DCIM_FAKE_FS=1`. The parent starts the worker process
//! as in normal use, and the worker serves the in-memory fake device:
//!
//! ```text
//! /Internal Storage/DCIM/202601_a/IMG_0001.HEIC   "heic-a"
//! /Internal Storage/DCIM/202601_a/IMG_0002.MOV    2048 zero bytes
//! /Internal Storage/DCIM/202601_b/IMG_0001.HEIC   "heic-b"
//! ```

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_win-iphone-dcim");
const DCIM: &str = "/Internal Storage/DCIM";
const HEIC_B: &str = "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC";

fn command(args: &[&str]) -> Command {
    let mut cmd = Command::new(EXE);
    cmd.args(args)
        .env("WIN_IPHONE_DCIM_FAKE_FS", "1")
        .env("RUST_LOG", "info")
        .env_remove("WIN_IPHONE_DCIM_FAKE_HANG_ONCE");
    cmd
}

fn output(cmd: &mut Command) -> (i32, String, String) {
    let Output {
        status,
        stdout,
        stderr,
    } = cmd.output().expect("run the program");
    (
        status.code().expect("exit code"),
        String::from_utf8(stdout).expect("UTF-8 stdout"),
        String::from_utf8(stderr).expect("UTF-8 stderr"),
    )
}

/// Run and expect exit code 0. Return stdout.
fn ok(args: &[&str]) -> String {
    let (code, out, err) = output(&mut command(args));
    assert_eq!(code, 0, "{args:?}\nstdout:\n{out}\nstderr:\n{err}");
    out
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn devices_ls_and_tree_through_the_worker() {
    let out = ok(&["devices"]);
    assert!(out.starts_with("[0] Apple iPhone"), "{out}");

    let out = ok(&["ls", "/"]);
    assert_eq!(out.trim_end(), "Internal Storage");

    let out = ok(&["tree"]);
    for name in [
        "DCIM",
        "202601_a",
        "202601_b",
        "IMG_0001.HEIC",
        "IMG_0002.MOV",
    ] {
        assert!(out.contains(name), "{name} missing:\n{out}");
    }
    assert_eq!(out.matches("IMG_0001.HEIC").count(), 2, "{out}");
}

#[test]
fn no_isolate_gives_the_same_results() {
    for args in [&["ls", "-R", DCIM][..], &["tree", DCIM], &["devices"]] {
        let mut no_isolate = vec!["--no-isolate"];
        no_isolate.extend_from_slice(args);
        assert_eq!(ok(args), ok(&no_isolate), "{args:?}");
    }
}

#[test]
fn copy_twice_then_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().to_str().unwrap();

    let out = ok(&["cp", "-r", "--manifest", DCIM, dest]);
    assert_eq!(out.matches("[copy] ").count(), 3, "{out}");
    assert!(out.contains("copied=3 skipped=0"), "{out}");
    let root = tmp.path().join("DCIM");
    assert_eq!(read(&root.join("202601_a/IMG_0001.HEIC")), b"heic-a");
    assert_eq!(read(&root.join("202601_a/IMG_0002.MOV")), vec![0u8; 2048]);
    assert_eq!(read(&root.join("202601_b/IMG_0001.HEIC")), b"heic-b");

    let out = ok(&["cp", "-r", "--manifest", DCIM, dest]);
    assert!(!out.contains("[copy] "), "{out}");
    assert_eq!(out.matches("  verified").count(), 3, "{out}");
    assert!(
        out.contains("copied=0 skipped=3 exists=0 failed=0"),
        "{out}"
    );

    let out = ok(&["verify", dest]);
    assert_eq!(out.matches("[ok] ").count(), 3, "{out}");
}

#[test]
fn default_copy_writes_no_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().to_str().unwrap();

    let out = ok(&["cp", "-r", DCIM, dest]);
    assert!(out.contains("copied=3 skipped=0"), "{out}");
    assert!(!tmp.path().join(".win-iphone-dcim").exists());

    let out = ok(&["cp", "-r", DCIM, dest]);
    assert_eq!(out.matches("  exists, same size").count(), 3, "{out}");
    assert!(
        out.contains("copied=0 skipped=3 exists=0 failed=0"),
        "{out}"
    );
    assert!(!tmp.path().join(".win-iphone-dcim").exists());

    let (_, _, err) = output(&mut command(&["verify", dest]));
    assert!(
        err.contains("run cp with --manifest to record copies"),
        "{err}"
    );
    assert!(!tmp.path().join(".win-iphone-dcim").exists());
}

/// SPEC.md section 11, "WPD Read() hangs": the parent kills the worker
/// after `--timeout`, prints a retry line, and the retry in a new worker
/// completes the file.
#[test]
fn hung_read_is_killed_and_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("hang-once");
    let target = tmp.path().join("IMG_0001.HEIC");
    let started = Instant::now();
    let (code, out, err) = output(
        command(&["--timeout", "1", "cp", HEIC_B, target.to_str().unwrap()])
            .env("WIN_IPHONE_DCIM_FAKE_HANG_ONCE", &marker),
    );
    let elapsed = started.elapsed();
    assert_eq!(code, 0, "stdout:\n{out}\nstderr:\n{err}");
    assert!(marker.exists(), "the fake did not hang");
    let retry = out
        .lines()
        .find(|l| l.starts_with("[retry 1/3] "))
        .unwrap_or_else(|| panic!("no retry line:\n{out}"));
    assert!(retry.contains(HEIC_B), "{retry}");
    assert!(retry.contains("worker was restarted"), "{retry}");
    assert!(retry.contains("no activity"), "{retry}");
    assert!(!out.contains("[retry 2/3]"), "{out}");
    assert!(out.contains("copied=1"), "{out}");
    assert_eq!(read(&target), b"heic-b");
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");
}

/// The worker's stdout carries the protocol only. Debug logs go to stderr.
#[cfg(unix)]
#[test]
fn worker_stdout_is_jsonl_only() {
    use std::io::Write;
    use std::process::Stdio;

    // The data pipe is not used without a read. Hand the worker its stderr.
    let mut child = command(&["worker", "--data-pipe", "2"])
        .env("RUST_LOG", "debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            b"{\"type\":\"hello\",\"protocol_version\":1}\n\
              {\"type\":\"list_devices\"}\n\
              {\"type\":\"shutdown\"}\n",
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("worker request"), "{stderr}");
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l:?}")))
        .collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert_eq!(lines[0]["type"], "hello");
    assert_eq!(lines[1]["type"], "devices");
}
