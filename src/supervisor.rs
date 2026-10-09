//! Parent side of the worker process (SPEC.md section 8).
//!
//! A WPD COM call can block in the driver, and no thread timeout can cancel
//! it. So the COM objects live in a child process: `<exe> worker`. The
//! supervisor talks to it with the protocol in `ipc`, tracks the time of
//! the last worker output, and kills the worker when a request shows no
//! activity for the timeout. A worker that exits unexpectedly is handled
//! the same way.
//!
//! The request that was in flight fails with `Error::WorkerRestarted`. The
//! next request starts a new worker and opens the device again. Object ids
//! from the old worker are never sent to the new one.

// The binary uses these items after the integration step wires
// `open_device_fs` into main.rs. Until then only tests use them.
#![allow(dead_code)]

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, PipeReader, PipeWriter, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::ipc::{self, PROTOCOL_VERSION, Request, Response, Target, WireNode};
use crate::model::DeviceInfo;

/// Default for `--timeout`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// How often the watchdog checks the last activity time.
const WATCHDOG_TICK: Duration = Duration::from_millis(250);
/// How long a worker may take to exit after `Shutdown`.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const E_FAIL: u32 = 0x8000_4005;
/// Lines before the first message that are not JSON are skipped. The test
/// harness prints a line before the fake worker starts.
const PREAMBLE_LINES: usize = 16;

/// How to start a worker process.
#[derive(Debug, Clone)]
pub struct WorkerCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub envs: Vec<(OsString, OsString)>,
    /// Pass the data pipe in `ipc::DATA_PIPE_ENV` instead of `--data-pipe <handle>`.
    pub pipe_in_env: bool,
}

impl WorkerCommand {
    /// `<this exe> worker --data-pipe <handle>`.
    pub fn current_exe() -> Result<Self> {
        let program = std::env::current_exe().map_err(|source| Error::Io {
            context: "find the program file to start the worker".into(),
            source,
        })?;
        Ok(Self {
            program,
            args: vec!["worker".into()],
            envs: Vec::new(),
            pipe_in_env: false,
        })
    }
}

/// Why a worker request failed.
enum Fault {
    /// The worker answered with an error. The worker is still usable.
    Remote(Error),
    /// The worker hung, exited, or broke the protocol. It must be replaced.
    Lost(String),
    /// A local write failed during a read. The data stream is out of step,
    /// so the worker must be replaced, but the caller gets this error.
    Local(Error),
}

enum Event {
    Message(Response),
    Closed(String),
}

/// State that the watchdog and the output reader share with the parent.
struct Shared {
    child: Mutex<Child>,
    last_activity: Mutex<Instant>,
    /// What the worker does now, for example `read /DCIM/x.MOV`. `None` when idle.
    in_flight: Mutex<Option<String>>,
    /// Why the watchdog killed the worker.
    killed: Mutex<Option<String>>,
    stop: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn touch(&self) {
        *lock(&self.last_activity) = Instant::now();
    }
}

/// One running worker process.
struct Worker {
    shared: Arc<Shared>,
    stdin: Option<ChildStdin>,
    events: Receiver<Event>,
    data: PipeReader,
}

/// Only one thread at a time makes the data pipe inheritable and spawns.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Spawn `cmd` so that the child inherits `pipe` and no other child does.
#[cfg(unix)]
fn spawn_with_pipe(cmd: &mut Command, pipe: &PipeWriter) -> io::Result<Child> {
    use std::os::fd::AsRawFd;
    use std::os::raw::c_int;
    use std::os::unix::process::CommandExt;

    unsafe extern "C" {
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    }
    // Same value on Linux, macOS and the BSDs.
    const F_SETFD: c_int = 2;

    let fd = pipe.as_raw_fd();
    let _guard = lock(&SPAWN_LOCK);
    // SAFETY: fcntl is async-signal-safe. It clears FD_CLOEXEC on `fd` in
    // the forked child only, so the parent keeps it close-on-exec.
    unsafe {
        cmd.pre_exec(move || {
            if fcntl(fd, F_SETFD, 0 as c_int) == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    cmd.spawn()
}

/// Spawn `cmd` so that the child inherits `pipe`. The handle is
/// inheritable only while the spawn lock is held.
#[cfg(windows)]
fn spawn_with_pipe(cmd: &mut Command, pipe: &PipeWriter) -> io::Result<Child> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
    };

    let handle = HANDLE(pipe.as_raw_handle());
    let _guard = lock(&SPAWN_LOCK);
    // SAFETY: `handle` is a valid pipe handle that `pipe` owns.
    unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
        .map_err(io::Error::other)?;
    let child = cmd.spawn();
    // SAFETY: as above.
    let _ = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) };
    child
}

impl Worker {
    /// Start a worker and check the protocol version.
    fn spawn(cmd: &WorkerCommand, timeout: Duration) -> Result<Self> {
        let spawn_err = |source| Error::Io {
            context: format!("start the worker process {}", cmd.program.display()),
            source,
        };
        let (data, data_writer) = io::pipe().map_err(spawn_err)?;
        let raw = ipc::raw_pipe(&data_writer).to_string();
        let mut command = Command::new(&cmd.program);
        command.args(&cmd.args);
        if cmd.pipe_in_env {
            command.env(ipc::DATA_PIPE_ENV, &raw);
        } else {
            command.arg("--data-pipe").arg(&raw);
        }
        command
            .envs(cmd.envs.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = spawn_with_pipe(&mut command, &data_writer).map_err(spawn_err)?;
        // Only the worker may hold the write end, so its exit ends the stream.
        drop(data_writer);
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let shared = Arc::new(Shared {
            child: Mutex::new(child),
            last_activity: Mutex::new(Instant::now()),
            in_flight: Mutex::new(None),
            killed: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let (tx, events) = mpsc::channel();
        if let Some(stdout) = stdout {
            let shared = Arc::clone(&shared);
            thread::spawn(move || read_lines(&mut BufReader::new(stdout), &|| shared.touch(), &tx));
        }
        {
            let shared = Arc::clone(&shared);
            thread::spawn(move || watchdog(&shared, timeout));
        }
        let mut worker = Self {
            shared,
            stdin,
            events,
            data,
        };
        let hello = Request::Hello {
            protocol_version: PROTOCOL_VERSION,
        };
        match worker.request(&hello, "start the worker".into()) {
            Ok(Response::Hello { protocol_version }) if protocol_version == PROTOCOL_VERSION => {
                Ok(worker)
            }
            Ok(other) => {
                worker.kill();
                Err(lost_error(
                    "start the worker",
                    format!("unexpected handshake {other:?}"),
                ))
            }
            Err(Fault::Remote(e)) | Err(Fault::Local(e)) => {
                worker.kill();
                Err(e)
            }
            Err(Fault::Lost(reason)) => {
                worker.kill();
                Err(lost_error("start the worker", reason))
            }
        }
    }

    fn begin(&self, what: String) {
        self.shared.touch();
        *lock(&self.shared.in_flight) = Some(what);
    }

    fn end(&self) {
        *lock(&self.shared.in_flight) = None;
    }

    /// The reason for a lost worker. The watchdog reason wins over the
    /// symptom that the parent saw.
    fn lost(&self, symptom: String) -> Fault {
        Fault::Lost(lock(&self.shared.killed).clone().unwrap_or(symptom))
    }

    fn send(&mut self, req: &Request) -> std::result::Result<(), Fault> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(self.lost("worker input is closed".into()));
        };
        ipc::write_message(stdin, req).map_err(|e| self.lost(format!("write to the worker: {e}")))
    }

    /// Wait for the final response. Skip heartbeats.
    fn wait(&mut self) -> std::result::Result<Response, Fault> {
        loop {
            match self.events.recv() {
                Ok(Event::Message(Response::Progress { .. })) => continue,
                Ok(Event::Message(Response::Error(e))) => {
                    return Err(Fault::Remote(e.into_error()));
                }
                Ok(Event::Message(m)) => return Ok(m),
                Ok(Event::Closed(reason)) => return Err(self.lost(reason)),
                Err(_) => return Err(self.lost("worker output reader stopped".into())),
            }
        }
    }

    fn request(&mut self, req: &Request, what: String) -> std::result::Result<Response, Fault> {
        self.begin(what);
        let result = self.send(req).and_then(|_| self.wait());
        self.end();
        result
    }

    fn read(
        &mut self,
        target: Target,
        chunk_size: u32,
        out: &mut dyn Write,
    ) -> std::result::Result<u64, Fault> {
        self.begin(format!("read {}", target.path));
        let result = self.read_inner(target, chunk_size, out);
        self.end();
        result
    }

    fn read_inner(
        &mut self,
        target: Target,
        chunk_size: u32,
        out: &mut dyn Write,
    ) -> std::result::Result<u64, Fault> {
        self.send(&Request::Read { target, chunk_size })?;
        let mut buf = Vec::with_capacity(chunk_size as usize);
        let mut received: u64 = 0;
        loop {
            match ipc::read_frame(&mut self.data, chunk_size, &mut buf) {
                Ok(true) => {
                    self.shared.touch();
                    out.write_all(&buf).map_err(|source| {
                        Fault::Local(Error::Io {
                            context: "write local file".into(),
                            source,
                        })
                    })?;
                    received += buf.len() as u64;
                    // A slow local disk is not a worker hang.
                    self.shared.touch();
                }
                Ok(false) => break,
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    return Err(Fault::Lost(format!("protocol error: {e}")));
                }
                Err(e) => return Err(self.lost(format!("data pipe closed: {e}"))),
            }
        }
        match self.wait()? {
            Response::ReadDone { bytes, .. } if bytes != received => Err(Fault::Lost(format!(
                "worker reported {bytes} bytes but sent {received}"
            ))),
            Response::ReadDone { ok: true, .. } => Ok(received),
            Response::ReadDone { error, .. } => Err(Fault::Remote(
                error
                    .unwrap_or_else(|| ipc::WireError::other("read failed"))
                    .into_error(),
            )),
            other => Err(Fault::Lost(format!(
                "unexpected response to read: {other:?}"
            ))),
        }
    }

    /// Kill the process and wait for it.
    fn kill(self) {
        drop(self);
    }

    /// Send `Shutdown`, wait up to `SHUTDOWN_GRACE`, then kill.
    fn shutdown(mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(mut stdin) = self.stdin.take() {
            let _ = ipc::write_message(&mut stdin, &Request::Shutdown);
        }
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        let mut child = lock(&self.shared.child);
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => thread::sleep(Duration::from_millis(20)),
            }
        }
        tracing::warn!("worker did not exit after Shutdown; killing it");
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let mut child = lock(&self.shared.child);
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Forward worker output lines as events. Every line counts as activity.
fn read_lines(r: &mut dyn BufRead, touch: &dyn Fn(), tx: &Sender<Event>) {
    let mut started = false;
    let mut skipped = 0;
    loop {
        let event = match ipc::read_line(r) {
            Ok(None) => Event::Closed("worker exited".into()),
            Err(e) => Event::Closed(format!("cannot read worker output: {e}")),
            Ok(Some(line)) => {
                touch();
                match ipc::parse::<Response>(&line) {
                    Ok(m) => {
                        started = true;
                        Event::Message(m)
                    }
                    Err(_) if !started && skipped < PREAMBLE_LINES => {
                        skipped += 1;
                        continue;
                    }
                    Err(e) => Event::Closed(format!("protocol error: {e}")),
                }
            }
        };
        let closed = matches!(event, Event::Closed(_));
        if tx.send(event).is_err() || closed {
            return;
        }
    }
}

/// Kill the worker when a request shows no activity for `timeout`.
fn watchdog(shared: &Shared, timeout: Duration) {
    while !shared.stop.load(Ordering::Relaxed) {
        thread::sleep(WATCHDOG_TICK);
        let Some(what) = lock(&shared.in_flight).clone() else {
            continue;
        };
        let idle = lock(&shared.last_activity).elapsed();
        if idle < timeout {
            continue;
        }
        let reason = format!(
            "no activity for {:.1} s during {what}; worker killed",
            idle.as_secs_f64()
        );
        tracing::warn!("{reason}");
        *lock(&shared.killed) = Some(reason);
        let _ = lock(&shared.child).kill();
        return;
    }
}

fn lost_error(context: &str, reason: String) -> Error {
    Error::WorkerRestarted {
        context: context.to_owned(),
        reason,
    }
}

/// Owns the current worker and replaces it after a failure.
pub struct Supervisor {
    command: WorkerCommand,
    timeout: Duration,
    selection: Option<usize>,
    worker: Option<Worker>,
    /// Number of workers that opened the device. Object ids are valid only
    /// within one generation.
    generation: u64,
    /// Device root from the newest worker.
    root: WireNode,
    /// Device ID from the first worker. Sensitive: never logged.
    device_id: Option<String>,
}

impl Supervisor {
    /// Start a worker and open the device.
    pub fn start(
        command: WorkerCommand,
        timeout: Duration,
        selection: Option<usize>,
    ) -> Result<Self> {
        let (worker, root, device_id) = Self::open(&command, timeout, selection)?;
        Ok(Self {
            command,
            timeout,
            selection,
            worker: Some(worker),
            generation: 1,
            root,
            device_id,
        })
    }

    fn open(
        command: &WorkerCommand,
        timeout: Duration,
        selection: Option<usize>,
    ) -> Result<(Worker, WireNode, Option<String>)> {
        let mut worker = Worker::spawn(command, timeout)?;
        let what = "open the device";
        match worker.request(&Request::Open { index: selection }, what.into()) {
            Ok(Response::Opened { root, device_id }) => Ok((worker, root, device_id)),
            Ok(other) => {
                worker.kill();
                Err(lost_error(what, format!("unexpected response {other:?}")))
            }
            Err(Fault::Remote(e)) | Err(Fault::Local(e)) => {
                worker.shutdown();
                Err(e)
            }
            Err(Fault::Lost(reason)) => {
                worker.kill();
                Err(lost_error(what, reason))
            }
        }
    }

    /// Make sure a worker runs. Return the current generation.
    pub fn ensure(&mut self) -> Result<u64> {
        if self.worker.is_none() {
            tracing::info!("starting a new worker and opening the device again");
            let (worker, root, device_id) =
                Self::open(&self.command, self.timeout, self.selection)?;
            if device_id != self.device_id {
                // The device index now names another device. Do not mix its
                // files into this copy.
                worker.shutdown();
                return Err(Error::DeviceOpen {
                    context: "open the device again after a worker restart".into(),
                    code: E_FAIL,
                    message: "a different device is now at the selected index".into(),
                });
            }
            self.worker = Some(worker);
            self.generation += 1;
            self.root = root;
        }
        Ok(self.generation)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn root(&self) -> &WireNode {
        &self.root
    }

    /// The raw device ID. Sensitive: hash it, never log it.
    pub fn device_id(&self) -> Option<String> {
        self.device_id.clone()
    }

    /// Turn a fault into the caller's error. Drop the worker if it is lost.
    fn fail(&mut self, fault: Fault, what: &str) -> Error {
        match fault {
            Fault::Remote(e) => e,
            Fault::Lost(reason) => {
                if let Some(w) = self.worker.take() {
                    w.kill();
                }
                tracing::warn!("{what}: worker lost: {reason}");
                lost_error(what, reason)
            }
            Fault::Local(e) => {
                if let Some(w) = self.worker.take() {
                    w.kill();
                }
                e
            }
        }
    }

    /// Send one request and map the final response with `pick`. A response
    /// that `pick` rejects is a protocol error.
    pub fn call<T>(
        &mut self,
        req: &Request,
        what: &str,
        pick: impl FnOnce(Response) -> std::result::Result<T, Box<Response>>,
    ) -> Result<T> {
        self.ensure()?;
        let worker = self.worker.as_mut().expect("ensure starts a worker");
        let fault = match worker.request(req, what.to_owned()) {
            Ok(response) => match pick(response) {
                Ok(value) => return Ok(value),
                Err(other) => Fault::Lost(format!("unexpected response {other:?}")),
            },
            Err(fault) => fault,
        };
        Err(self.fail(fault, what))
    }

    pub fn list(&mut self, target: Target) -> Result<Vec<WireNode>> {
        let what = format!("list {}", target.path);
        self.call(&Request::List { target }, &what, |r| match r {
            Response::Nodes { nodes } => Ok(nodes),
            other => Err(Box::new(other)),
        })
    }

    pub fn resolve(&mut self, path: &str) -> Result<WireNode> {
        let what = format!("resolve {path}");
        let req = Request::Resolve {
            path: path.to_owned(),
        };
        self.call(&req, &what, |r| match r {
            Response::Node { node } => Ok(node),
            other => Err(Box::new(other)),
        })
    }

    /// Stream a file to `out`. On error `out` can hold partial data.
    pub fn read(&mut self, target: Target, chunk_size: u32, out: &mut dyn Write) -> Result<u64> {
        self.ensure()?;
        let what = format!("read {}", target.path);
        let worker = self.worker.as_mut().expect("ensure starts a worker");
        match worker.read(target, chunk_size, out) {
            Ok(n) => Ok(n),
            Err(fault) => Err(self.fail(fault, &what)),
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(w) = self.worker.take() {
            w.shutdown();
        }
    }
}

/// List the devices from a short-lived worker.
pub fn list_devices(command: &WorkerCommand, timeout: Duration) -> Result<Vec<DeviceInfo>> {
    let mut worker = Worker::spawn(command, timeout)?;
    let what = "list devices";
    match worker.request(&Request::ListDevices, what.into()) {
        Ok(Response::Devices { devices }) => {
            worker.shutdown();
            Ok(devices.into_iter().map(Into::into).collect())
        }
        Ok(other) => {
            worker.kill();
            Err(lost_error(what, format!("unexpected response {other:?}")))
        }
        Err(Fault::Remote(e)) | Err(Fault::Local(e)) => {
            worker.shutdown();
            Err(e)
        }
        Err(Fault::Lost(reason)) => {
            worker.kill();
            Err(lost_error(what, reason))
        }
    }
}

#[cfg(test)]
mod tests {
    //! The fake worker is this test binary: `test_worker_main` serves the
    //! in-memory device when `WORKER_MODE_ENV` is set. It runs in its own
    //! process, so kills and crashes are real.

    use super::*;
    use crate::device_fs::fake::{FakeFs, dcim};
    use crate::device_fs::{DeviceFs, RemoteFs, resolve};
    use crate::devpath::DevicePath;
    use crate::ipc::Backend;
    use crate::model::Node;

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
        let err =
            resolve(&fs, &DevicePath::parse("/Internal Storage/DCIM/x").unwrap()).unwrap_err();
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
}
