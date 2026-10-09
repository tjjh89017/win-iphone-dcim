//! The fake worker is this test binary: `test_worker_main` serves the
//! in-memory device when `WORKER_MODE_ENV` is set. It runs in its own
//! process, so kills and crashes are real.

use super::*;
use crate::device_fs::fake::{FakeFs, dcim};
use crate::device_fs::{DeviceFs, RemoteFs, resolve};
use crate::devpath::DevicePath;
use crate::ipc::Backend;
use crate::model::Node;
use std::ffi::OsStr;

const WORKER_MODE_ENV: &str = "WIN_IPHONE_DCIM_TEST_WORKER";
const MOV: &str = "/Internal Storage/DCIM/202601_a/IMG_0002.MOV";
const HEIC_B: &str = "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC";
const BIG: &str = "/Internal Storage/DCIM/202601_b/BIG.MOV";

/// The fake device, plus a file that reports 5 GiB.
fn device() -> FakeFs {
    let mut fs = dcim();
    let big = fs.file(6, "BIG.MOV", b"x");
    fs.node_mut(big).size = Some(5 << 30);
    fs
}

/// Reads of `IMG_0002.MOV` send 1000 bytes, then do what the mode says.
struct TestFs {
    inner: FakeFs,
    mode: String,
}

impl DeviceFs for TestFs {
    fn root(&self) -> Node {
        self.inner.root()
    }

    fn list(&self, dir: &Node) -> Result<Vec<Node>> {
        self.inner.list(dir)
    }

    fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64> {
        if file.name.as_deref() != Some("IMG_0002.MOV") || self.mode == "normal" {
            return self.inner.read_to(file, out);
        }
        let mut all = Vec::new();
        self.inner.read_to(file, &mut all)?;
        out.write_all(&all[..1000]).unwrap();
        out.flush().unwrap();
        match self.mode.as_str() {
            "exit_mid_read" => std::process::exit(7),
            // A blocked COM Read: no frames and no heartbeats.
            _ => thread::sleep(Duration::from_secs(60)),
        }
        unreachable!()
    }

    fn device_id(&self) -> Option<String> {
        Some(match self.mode.as_str() {
            "changing_id" => format!("USB\\VID_05AC&PID_12A8\\{}", std::process::id()),
            _ => "USB\\VID_05AC&PID_12A8\\fake".into(),
        })
    }
}

struct TestBackend(String);

impl Backend for TestBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        Ok(vec![DeviceInfo {
            index: 0,
            friendly_name: Some("Apple iPhone".into()),
            manufacturer: None,
            description: None,
        }])
    }

    fn open(&self, _: Option<usize>) -> Result<Box<dyn DeviceFs>> {
        if self.0 == "no_device" {
            return Err(Error::NoDevice);
        }
        Ok(Box::new(TestFs {
            inner: device(),
            mode: self.0.clone(),
        }))
    }
}

/// Answers like a worker but sends a frame longer than the chunk size.
fn big_frame_worker(data: &mut dyn Write) {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut out = io::stdout().lock();
    while let Some(line) = ipc::read_line(&mut input).unwrap() {
        let response = match ipc::parse::<Request>(&line).unwrap() {
            Request::Hello { .. } => Response::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
            Request::Open { .. } => Response::Opened {
                root: (&device().root()).into(),
                device_id: None,
            },
            Request::Read { chunk_size, .. } => {
                ipc::write_frame(data, &vec![1; chunk_size as usize + 1]).unwrap();
                data.flush().unwrap();
                continue;
            }
            _ => return,
        };
        ipc::write_message(&mut out, &response).unwrap();
    }
}

/// Entry point of the fake worker process. A no-op in a normal test run.
#[test]
fn test_worker_main() {
    let Ok(mode) = std::env::var(WORKER_MODE_ENV) else {
        return;
    };
    let raw: u64 = std::env::var(ipc::DATA_PIPE_ENV).unwrap().parse().unwrap();
    // SAFETY: the supervisor passed this inherited pipe end to us only.
    let mut data = unsafe { ipc::data_pipe_from_raw(raw) }.unwrap();
    if mode == "big_frame" {
        big_frame_worker(&mut data);
        std::process::exit(0);
    }
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut out = io::stdout().lock();
    let code = match ipc::serve(&TestBackend(mode), &mut input, &mut out, &mut data) {
        Ok(()) => 0,
        Err(_) => 1,
    };
    std::process::exit(code);
}

#[test]
fn worker_process_passes_the_data_pipe() {
    let mut cmd = command("normal");
    let c = worker_process(&cmd, "42");
    assert!(
        c.get_envs()
            .any(|(k, v)| k == ipc::DATA_PIPE_ENV && v == Some(OsStr::new("42")))
    );
    cmd.pipe_in_env = false;
    let c = worker_process(&cmd, "42");
    let args: Vec<&OsStr> = c.get_args().collect();
    assert_eq!(
        args[args.len() - 2..],
        [OsStr::new("--data-pipe"), OsStr::new("42")]
    );
    // std cannot read creation flags back; check the flag value instead.
    #[cfg(windows)]
    assert_eq!(NO_WINDOW, 0x0800_0000);
}

fn command(mode: &str) -> WorkerCommand {
    WorkerCommand {
        program: std::env::current_exe().unwrap(),
        args: [
            "supervisor::tests::test_worker_main",
            "--exact",
            "--nocapture",
            "--quiet",
            "--test-threads=1",
        ]
        .map(OsString::from)
        .to_vec(),
        envs: vec![(WORKER_MODE_ENV.into(), mode.into())],
        pipe_in_env: true,
    }
}

#[test]
fn worker_is_the_cli_next_to_the_gui() {
    let tmp = tempfile::tempdir().unwrap();
    let gui = tmp.path().join("win-iphone-dcim-gui.exe");
    let err = cli_next_to(&gui).unwrap_err();
    match &err {
        Error::WorkerMissing(path) => assert_eq!(path, &tmp.path().join(CLI_EXE_NAME)),
        other => panic!("unexpected: {other}"),
    }
    assert!(err.to_string().contains("same folder"), "{err}");
    std::fs::write(tmp.path().join(CLI_EXE_NAME), b"").unwrap();
    assert_eq!(cli_next_to(&gui).unwrap(), tmp.path().join(CLI_EXE_NAME));
    let cmd = WorkerCommand::program(tmp.path().join(CLI_EXE_NAME));
    assert_eq!(cmd.args, [OsString::from("worker")]);
}

fn remote(mode: &str, timeout: Duration) -> RemoteFs {
    RemoteFs::new(Supervisor::start(command(mode), timeout, None).unwrap())
}

fn at(fs: &dyn DeviceFs, path: &str) -> Node {
    resolve(fs, &DevicePath::parse(path).unwrap()).unwrap()
}

#[test]
fn normal_read_streams_the_exact_bytes() {
    let fs = remote("normal", DEFAULT_TIMEOUT);
    let mut out = Vec::new();
    assert_eq!(fs.read_to(&at(&fs, HEIC_B), &mut out).unwrap(), 6);
    assert_eq!(out, b"heic-b");
    let mut out = Vec::new();
    assert_eq!(fs.read_to(&at(&fs, MOV), &mut out).unwrap(), 2048);
    assert_eq!(out, vec![0u8; 2048]);
}

#[test]
fn size_over_4_gib_is_reported_as_u64() {
    let fs = remote("normal", DEFAULT_TIMEOUT);
    let big = at(&fs, BIG);
    assert_eq!(big.size, Some(5 * 1024 * 1024 * 1024));
    assert!(!big.is_folder);
}

#[test]
fn listing_matches_the_in_process_device() {
    let fs = remote("normal", DEFAULT_TIMEOUT);
    let local = device();
    let names = |fs: &dyn DeviceFs| -> Vec<String> {
        let dir = resolve(
            fs,
            &DevicePath::parse("/Internal Storage/DCIM/202601_b").unwrap(),
        )
        .unwrap();
        fs.list(&dir)
            .unwrap()
            .into_iter()
            .map(|n| n.display_name())
            .collect()
    };
    assert_eq!(names(&fs), names(&local));
    assert_eq!(fs.root().name.as_deref(), Some("Apple iPhone"));
}

#[test]
fn device_errors_cross_the_process_boundary() {
    let err = Supervisor::start(command("no_device"), DEFAULT_TIMEOUT, None)
        .err()
        .unwrap();
    assert!(matches!(err, Error::NoDevice), "{err}");

    let fs = remote("normal", DEFAULT_TIMEOUT);
    let dcim = at(&fs, "/Internal Storage/DCIM");
    let err = resolve(&fs, &DevicePath::parse("/Internal Storage/DCIM/x").unwrap()).unwrap_err();
    assert!(matches!(err, Error::PathNotFound { .. }), "{err}");
    // The worker is still the same after a device error.
    assert_eq!(fs.list(&dcim).unwrap().len(), 2);
    assert_eq!(fs.supervisor().generation(), 1);
}

#[test]
fn device_id_comes_from_the_worker() {
    let fs = remote("normal", DEFAULT_TIMEOUT);
    assert_eq!(
        fs.device_id().as_deref(),
        Some("USB\\VID_05AC&PID_12A8\\fake")
    );
}

#[test]
fn restart_refuses_a_different_device() {
    let fs = remote("changing_id", Duration::from_secs(1));
    let dcim = at(&fs, "/Internal Storage/DCIM");
    let err = fs
        .supervisor()
        .call(&Request::Hang { ms: 30_000 }, "hang", |_| Ok(()))
        .unwrap_err();
    assert!(matches!(err, Error::WorkerRestarted { .. }), "{err}");
    let err = fs.list(&dcim).unwrap_err();
    assert!(matches!(err, Error::DeviceOpen { .. }), "{err}");
    assert!(err.to_string().contains("different device"), "{err}");
}

#[test]
fn hung_worker_is_killed_and_the_next_call_restarts_it() {
    let fs = remote("normal", Duration::from_secs(1));
    let dcim = at(&fs, "/Internal Storage/DCIM");
    let started = Instant::now();
    let err = fs
        .supervisor()
        .call(&Request::Hang { ms: 30_000 }, "hang", |_| Ok(()))
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "watchdog took {elapsed:?}"
    );
    let Error::WorkerRestarted { context, reason } = &err else {
        panic!("unexpected: {err}");
    };
    assert_eq!(context, "hang");
    assert!(reason.contains("no activity"), "{reason}");
    assert!(reason.contains("hang"), "{reason}");

    // The stale node is resolved by path in the new worker.
    let children = fs.list(&dcim).unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(fs.supervisor().generation(), 2);
    let mut out = Vec::new();
    fs.read_to(&at(&fs, HEIC_B), &mut out).unwrap();
    assert_eq!(out, b"heic-b");
}

#[test]
fn blocked_read_is_killed_after_the_timeout() {
    let fs = remote("hang_read", Duration::from_secs(1));
    let mov = at(&fs, MOV);
    let mut out = Vec::new();
    let started = Instant::now();
    let err = fs.read_to(&mov, &mut out).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10));
    let Error::WorkerRestarted { reason, .. } = &err else {
        panic!("unexpected: {err}");
    };
    assert!(reason.contains(MOV), "{reason}");
    assert_eq!(out.len(), 1000);
    // The node from the killed worker still works by path.
    let mut out = Vec::new();
    let heic = at(&fs, HEIC_B);
    assert_eq!(fs.read_to(&heic, &mut out).unwrap(), 6);
}

#[test]
fn worker_exit_mid_read_is_not_a_complete_file() {
    let fs = remote("exit_mid_read", DEFAULT_TIMEOUT);
    let mov = at(&fs, MOV);
    let heic = at(&fs, HEIC_B);
    let mut out = Vec::new();
    let err = fs.read_to(&mov, &mut out).unwrap_err();
    assert!(matches!(err, Error::WorkerRestarted { .. }), "{err}");
    assert!(out.len() < 2048);
    // The next call starts a new worker; old nodes resolve by path.
    let mut out = Vec::new();
    assert_eq!(fs.read_to(&heic, &mut out).unwrap(), 6);
    assert_eq!(out, b"heic-b");
    assert_eq!(fs.supervisor().generation(), 2);
}

#[test]
fn frame_longer_than_the_chunk_is_rejected() {
    let mut sup = Supervisor::start(command("big_frame"), DEFAULT_TIMEOUT, None).unwrap();
    let mut out = Vec::new();
    let err = sup.read(Target::path(MOV), 16, &mut out).unwrap_err();
    let Error::WorkerRestarted { reason, .. } = &err else {
        panic!("unexpected: {err}");
    };
    assert!(reason.contains("chunk size"), "{reason}");
    assert!(out.is_empty());
}

#[test]
fn list_devices_uses_a_short_lived_worker() {
    let devices = list_devices(&command("normal"), DEFAULT_TIMEOUT).unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].friendly_name.as_deref(), Some("Apple iPhone"));
}

#[test]
fn drop_stops_the_worker_quickly() {
    let fs = remote("normal", DEFAULT_TIMEOUT);
    let started = Instant::now();
    drop(fs);
    assert!(started.elapsed() < SHUTDOWN_GRACE);
}

#[test]
fn preamble_lines_are_skipped_only_before_the_first_message() {
    let input = b"\nrunning 1 test\n{\"type\":\"hung\"}\nnoise\n";
    let (tx, rx) = mpsc::channel();
    let touched = std::cell::Cell::new(0);
    read_lines(
        &mut input.as_slice(),
        &|| touched.set(touched.get() + 1),
        &tx,
    );
    assert!(matches!(rx.recv().unwrap(), Event::Message(Response::Hung)));
    assert!(matches!(rx.recv().unwrap(), Event::Closed(r) if r.contains("protocol error")));
    assert_eq!(touched.get(), 4);
}
