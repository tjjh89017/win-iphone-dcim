//! Backend-neutral view of a device as a tree of objects.
//!
//! WPD implements this trait on Windows. Tests use an in-memory fake. A
//! future AFC backend can implement it too.

use std::io::Write;

use crate::devpath::DevicePath;
use crate::error::{Error, Result};
use crate::model::{Match, Node};

pub trait DeviceFs {
    /// The device object. Its children are the top level, for example `Internal Storage`.
    fn root(&self) -> Node;

    /// The direct children of `dir`, in device order.
    fn list(&self, dir: &Node) -> Result<Vec<Node>>;

    /// Stream the data of `file` to `out` and return the number of bytes written.
    fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64>;
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
        let found = [Match::OriginalFileName, Match::Name]
            .into_iter()
            .find_map(|by| children.iter().find(|c| c.matches(component, by)));
        current = found.cloned().ok_or_else(|| Error::PathNotFound {
            path: path.to_string(),
            component: component.clone(),
        })?;
    }
    if path.trailing_slash() && !current.is_folder {
        return Err(Error::NotAFolder(path.to_string()));
    }
    Ok(current)
}

#[cfg(test)]
pub mod fake {
    //! In-memory device for tests.

    use super::*;
    use crate::model::ObjectId;

    pub struct FakeFs {
        nodes: Vec<(Node, Option<usize>, Vec<u8>)>,
        /// (node index, bytes before the error, fatal).
        failures: Vec<(usize, usize, bool)>,
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
        pub fn node_mut(&mut self, index: usize) -> &mut Node {
            &mut self.nodes[index].0
        }

        /// Make `read_to` of node `index` fail after `after` bytes. A fatal
        /// failure looks like a disconnected device.
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
