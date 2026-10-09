//! Backend-neutral view of a device as a tree of objects.
//!
//! WPD implements this trait on Windows. Tests use an in-memory fake. A
//! future AFC backend can implement it too.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use crate::devpath::DevicePath;
use crate::error::{Error, Result};
use crate::ipc::{Target, WireNode};
use crate::model::{DeviceInfo, Match, Node, ObjectId, join_device_path};
use crate::supervisor::{Supervisor, WorkerCommand};

pub trait DeviceFs {
    /// The device object. Its children are the top level, for example `Internal Storage`.
    fn root(&self) -> Node;

    /// The direct children of `dir`, in device order.
    fn list(&self, dir: &Node) -> Result<Vec<Node>>;

    /// Stream the data of `file` to `out` and return the number of bytes written.
    fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64>;

    /// The raw backend device ID, for example the WPD PnP device ID. It is
    /// sensitive: callers hash it and never log or store the raw value.
    /// `None` if the backend cannot give one.
    fn device_id(&self) -> Option<String> {
        None
    }
}

/// Resolve `path` one level at a time from the root.
///
/// At each level a child whose original file name matches wins. If no
/// child matches that way, a child whose object name matches is used.
pub fn resolve(fs: &dyn DeviceFs, path: &DevicePath) -> Result<Node> {
    let mut current = fs.root();
    for component in path.components() {
        if !current.is_folder {
            return Err(Error::NotAFolder(current.display_name()));
        }
        let children = fs.list(&current)?;
        current = find_child(&children, component)
            .cloned()
            .ok_or_else(|| Error::PathNotFound {
                path: path.to_string(),
                component: component.clone(),
            })?;
    }
    if path.trailing_slash() && !current.is_folder {
        return Err(Error::NotAFolder(path.to_string()));
    }
    Ok(current)
}

/// The child that the path component `component` names: a match on the
/// original file name wins over a match on the object name.
pub fn find_child<'a>(children: &'a [Node], component: &str) -> Option<&'a Node> {
    [Match::OriginalFileName, Match::Name]
        .into_iter()
        .find_map(|by| children.iter().find(|c| c.matches(component, by)))
}

/// Open the device. With `isolate` the WPD COM objects live in a worker
/// process that the supervisor restarts after a hang (`timeout` without
/// activity) or a crash. Without it they live in this process, which is
/// for debugging only.
///
/// With the `fake-device` feature or in unit tests,
/// `WIN_IPHONE_DCIM_FAKE_FS=1` gives the in-memory fake device instead of
/// WPD, in the worker or, without `isolate`, in this process.
pub fn open_device_fs(
    selection: Option<usize>,
    timeout: Duration,
    isolate: bool,
) -> Result<Box<dyn DeviceFs>> {
    if !isolate {
        #[cfg(any(test, feature = "fake-device"))]
        if fake::requested() {
            return Ok(Box::new(fake::from_env()));
        }
        return crate::wpd::open(selection);
    }
    open_remote(WorkerCommand::current_exe()?, selection, timeout)
}

/// Open the device in a worker process that `worker` starts.
pub fn open_remote(
    worker: WorkerCommand,
    selection: Option<usize>,
    timeout: Duration,
) -> Result<Box<dyn DeviceFs>> {
    let supervisor = Supervisor::start(worker, timeout, selection)?;
    Ok(Box::new(RemoteFs::new(supervisor)))
}

/// List the devices, in a short-lived worker process with `isolate`.
pub fn list_devices(timeout: Duration, isolate: bool) -> Result<Vec<DeviceInfo>> {
    if !isolate {
        #[cfg(any(test, feature = "fake-device"))]
        if fake::requested() {
            return Ok(fake::devices());
        }
        return crate::wpd::list_devices();
    }
    crate::supervisor::list_devices(&WorkerCommand::current_exe()?, timeout)
}

/// Largest data frame `RemoteFs` asks for. Same as the WPD buffer limit.
const REMOTE_CHUNK: u32 = crate::wpd::MAX_BUFFER as u32;

/// `DeviceFs` over a worker process.
///
/// A node id that this type hands out is `w<generation>:` followed by the
/// worker's object id. Within the generation that made it, the worker gets
/// the object id back. After a worker restart the old id is never sent:
/// the new worker resolves the node by its device path. A path is known
/// only when its last component names exactly this node; a stale node
/// without one fails with `PathNotFound`.
///
/// A worker hang or crash fails the current call with
/// `Error::WorkerRestarted`. The next call starts a new worker.
pub struct RemoteFs {
    supervisor: RefCell<Supervisor>,
    /// Device path of each node this value handed out, by node id.
    paths: RefCell<HashMap<ObjectId, String>>,
}

impl RemoteFs {
    pub fn new(supervisor: Supervisor) -> Self {
        Self {
            supervisor: RefCell::new(supervisor),
            paths: RefCell::new(HashMap::new()),
        }
    }

    /// The supervisor, for tests that drive it directly.
    #[cfg(test)]
    pub fn supervisor(&self) -> std::cell::RefMut<'_, Supervisor> {
        self.supervisor.borrow_mut()
    }

    fn encode(generation: u64, raw: &[u16]) -> ObjectId {
        let mut units: Vec<u16> = format!("w{generation}:").encode_utf16().collect();
        units.extend_from_slice(raw);
        ObjectId(units)
    }

    fn decode(id: &ObjectId) -> Option<(u64, &[u16])> {
        let colon = id.0.iter().position(|&u| u == u16::from(b':'))?;
        let tag = String::from_utf16(&id.0[..colon]).ok()?;
        let generation = tag.strip_prefix('w')?.parse().ok()?;
        Some((generation, &id.0[colon + 1..]))
    }

    fn node(&self, generation: u64, wire: WireNode, path: Option<String>) -> Node {
        let mut node = Node::from(wire);
        node.id = Self::encode(generation, &node.id.0);
        if let Some(path) = path {
            self.paths.borrow_mut().insert(node.id.clone(), path);
        }
        node
    }

    /// How to name `node` to the worker of `generation`.
    fn target(&self, node: &Node, generation: u64) -> Result<Target> {
        let path = self.paths.borrow().get(&node.id).cloned();
        let current = Self::decode(&node.id)
            .filter(|(g, _)| *g == generation)
            .map(|(_, raw)| raw.to_vec());
        match (path, current) {
            (Some(path), id) => Ok(Target { path, id }),
            (None, Some(id)) => Ok(Target {
                path: node.display_name(),
                id: Some(id),
            }),
            (None, None) => Err(Error::PathNotFound {
                path: node.display_name(),
                component: node.display_name(),
            }),
        }
    }
}

impl DeviceFs for RemoteFs {
    fn root(&self) -> Node {
        let supervisor = self.supervisor.borrow();
        self.node(
            supervisor.generation(),
            supervisor.root().clone(),
            Some("/".into()),
        )
    }

    fn list(&self, dir: &Node) -> Result<Vec<Node>> {
        let mut supervisor = self.supervisor.borrow_mut();
        let generation = supervisor.ensure()?;
        let target = self.target(dir, generation)?;
        let parent = target.path.clone();
        let wire = supervisor.list(target)?;
        let plain: Vec<Node> = wire.iter().cloned().map(Node::from).collect();
        Ok(wire
            .into_iter()
            .zip(&plain)
            .map(|(w, n)| {
                let path = n
                    .original_file_name
                    .as_deref()
                    .or(n.name.as_deref())
                    .filter(|c| !c.is_empty() && !c.contains('/'))
                    .filter(|c| find_child(&plain, c).is_some_and(|found| found.id == n.id))
                    .map(|c| join_device_path(&parent, c));
                self.node(generation, w, path)
            })
            .collect())
    }

    fn device_id(&self) -> Option<String> {
        self.supervisor.borrow().device_id()
    }

    fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64> {
        let mut supervisor = self.supervisor.borrow_mut();
        let generation = supervisor.ensure()?;
        let target = self.target(file, generation)?;
        supervisor.read(target, REMOTE_CHUNK, out)
    }
}

#[cfg(any(test, feature = "fake-device"))]
pub mod fake {
    //! In-memory device for tests. Unit tests always have it. A binary has
    //! it only with the `fake-device` feature, which the end-to-end tests
    //! need. Release builds do not enable the feature. Only the test
    //! environment variables below turn it on.

    use std::path::PathBuf;

    use super::*;
    use crate::model::ObjectId;

    /// `1` serves the fake device instead of WPD.
    pub const FAKE_FS_ENV: &str = "WIN_IPHONE_DCIM_FAKE_FS";
    /// A marker file path. The first `read_to` that creates this file
    /// blocks forever, like a hung WPD `Read()`. Later reads, also in
    /// other processes, see the file and work.
    pub const FAKE_HANG_ONCE_ENV: &str = "WIN_IPHONE_DCIM_FAKE_HANG_ONCE";

    /// True when the test environment asks for the fake device.
    pub fn requested() -> bool {
        std::env::var_os(FAKE_FS_ENV).is_some_and(|v| v == "1")
    }

    /// The device list of the fake.
    pub fn devices() -> Vec<DeviceInfo> {
        vec![DeviceInfo {
            index: 0,
            friendly_name: Some("Apple iPhone".into()),
            manufacturer: Some("Apple Inc.".into()),
            description: None,
        }]
    }

    /// `dcim()` with the hooks from the test environment.
    pub fn from_env() -> FakeFs {
        let mut fs = dcim();
        fs.hang_once = std::env::var_os(FAKE_HANG_ONCE_ENV).map(PathBuf::from);
        fs
    }

    pub struct FakeFs {
        nodes: Vec<(Node, Option<usize>, Vec<u8>)>,
        /// (node index, bytes before the error, fatal).
        failures: Vec<(usize, usize, bool)>,
        /// Marker file for the one hung read. See `FAKE_HANG_ONCE_ENV`.
        hang_once: Option<PathBuf>,
    }

    impl FakeFs {
        /// A device with only the root object.
        pub fn new() -> Self {
            let root = Node {
                id: ObjectId::new("DEVICE"),
                name: Some("Apple iPhone".into()),
                original_file_name: None,
                is_folder: true,
                size: None,
                content_type: None,
                modified: None,
                created: None,
                raw_file_name: None,
            };
            Self {
                nodes: vec![(root, None, Vec::new())],
                failures: Vec::new(),
                hang_once: None,
            }
        }

        fn add(&mut self, parent: usize, name: &str, data: Option<&[u8]>) -> usize {
            let index = self.nodes.len();
            let node = Node {
                id: ObjectId::new(&format!("o{index}")),
                name: Some(name.into()),
                original_file_name: data.map(|_| name.to_owned()),
                is_folder: data.is_none(),
                size: data.map(|d| d.len() as u64),
                content_type: None,
                modified: None,
                created: None,
                raw_file_name: Some(name.encode_utf16().collect()),
            };
            self.nodes
                .push((node, Some(parent), data.unwrap_or_default().to_vec()));
            index
        }

        pub fn folder(&mut self, parent: usize, name: &str) -> usize {
            self.add(parent, name, None)
        }

        pub fn file(&mut self, parent: usize, name: &str, data: &[u8]) -> usize {
            self.add(parent, name, Some(data))
        }

        /// Change a node after creation, for example to fake a wrong size.
        #[cfg(test)]
        pub fn node_mut(&mut self, index: usize) -> &mut Node {
            &mut self.nodes[index].0
        }

        /// Make `read_to` of node `index` fail after `after` bytes. A fatal
        /// failure looks like a disconnected device.
        #[cfg(test)]
        pub fn fail_read(&mut self, index: usize, after: usize, fatal: bool) {
            self.failures.push((index, after, fatal));
        }

        fn index_of(&self, node: &Node) -> usize {
            self.nodes
                .iter()
                .position(|(n, _, _)| n.id == node.id)
                .unwrap()
        }
    }

    impl Default for FakeFs {
        fn default() -> Self {
            Self::new()
        }
    }

    impl DeviceFs for FakeFs {
        fn root(&self) -> Node {
            self.nodes[0].0.clone()
        }

        fn list(&self, dir: &Node) -> Result<Vec<Node>> {
            let parent = self.index_of(dir);
            Ok(self
                .nodes
                .iter()
                .filter(|(_, p, _)| *p == Some(parent))
                .map(|(n, _, _)| n.clone())
                .collect())
        }

        fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64> {
            if let Some(marker) = &self.hang_once
                && std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(marker)
                    .is_ok()
            {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            }
            let index = self.index_of(file);
            let data = &self.nodes[index].2;
            if let Some(&(_, after, fatal)) = self.failures.iter().find(|f| f.0 == index) {
                let n = after.min(data.len());
                out.write_all(&data[..n]).map_err(|source| Error::Io {
                    context: "write".into(),
                    source,
                })?;
                let code = if fatal { 0x8007_048F } else { 0x8000_4005 };
                return Err(Error::from_hresult(
                    format!("fake read after {n} bytes"),
                    code,
                    "simulated failure".into(),
                    false,
                ));
            }
            out.write_all(data).map_err(|source| Error::Io {
                context: "write".into(),
                source,
            })?;
            Ok(data.len() as u64)
        }
    }

    /// A small DCIM tree: two folders that hold files with the same name.
    pub fn dcim() -> FakeFs {
        let mut fs = FakeFs::new();
        let storage = fs.folder(0, "Internal Storage");
        let dcim = fs.folder(storage, "DCIM");
        let a = fs.folder(dcim, "202601_a");
        fs.file(a, "IMG_0001.HEIC", b"heic-a");
        fs.file(a, "IMG_0002.MOV", &[0u8; 2048]);
        let b = fs.folder(dcim, "202601_b");
        fs.file(b, "IMG_0001.HEIC", b"heic-b");
        fs
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeFs, dcim};
    use super::*;

    fn p(s: &str) -> DevicePath {
        DevicePath::parse(s).unwrap()
    }

    #[test]
    fn resolves_root_and_nested_paths() {
        let fs = dcim();
        assert_eq!(resolve(&fs, &p("/")).unwrap().id, fs.root().id);
        let a = resolve(&fs, &p("/Internal Storage/DCIM/202601_a")).unwrap();
        assert!(a.is_folder);
        let f = resolve(&fs, &p("/Internal Storage/DCIM/202601_b/IMG_0001.HEIC")).unwrap();
        assert_eq!(f.size, Some(6));
    }

    #[test]
    fn not_found_names_the_failing_component() {
        let fs = dcim();
        let err = resolve(&fs, &p("/Internal Storage/DCIM/202612_z/IMG.HEIC")).unwrap_err();
        match err {
            Error::PathNotFound { component, .. } => assert_eq!(component, "202612_z"),
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn original_file_name_wins_over_name() {
        let mut fs = FakeFs::new();
        let x = fs.folder(0, "X");
        fs.node_mut(x).name = Some("IMG_0001".into());
        let y = fs.file(0, "Y", b"y");
        fs.node_mut(y).original_file_name = Some("IMG_0001".into());
        let found = resolve(&fs, &p("/IMG_0001")).unwrap();
        assert_eq!(found.id, fs.node_mut(y).id.clone());
    }

    #[test]
    fn file_with_trailing_slash_is_not_a_folder() {
        let fs = dcim();
        let err = resolve(&fs, &p("/Internal Storage/DCIM/202601_a/IMG_0001.HEIC/")).unwrap_err();
        assert!(matches!(err, Error::NotAFolder(_)));
    }
}
