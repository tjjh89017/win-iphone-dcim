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

/// Counts the `list` calls of each folder.
struct Counting<'a> {
    inner: &'a dyn DeviceFs,
    lists: RefCell<HashMap<ObjectId, usize>>,
}

impl DeviceFs for Counting<'_> {
    fn root(&self) -> Node {
        self.inner.root()
    }

    fn list(&self, dir: &Node) -> Result<Vec<Node>> {
        *self.lists.borrow_mut().entry(dir.id.clone()).or_default() += 1;
        self.inner.list(dir)
    }

    fn read_to(&self, file: &Node, out: &mut dyn Write) -> Result<u64> {
        self.inner.read_to(file, out)
    }
}

#[test]
fn cached_fs_lists_each_folder_once() {
    let fs = dcim();
    let counting = Counting {
        inner: &fs,
        lists: RefCell::new(HashMap::new()),
    };
    let cached = CachedFs::new(&counting);
    for path in [
        "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC",
        "/Internal Storage/DCIM/202601_a/IMG_0002.MOV",
        "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC",
    ] {
        resolve(&cached, &p(path)).unwrap();
    }
    let lists = counting.lists.borrow();
    // Root, Internal Storage, DCIM, 202601_a and 202601_b.
    assert_eq!(lists.len(), 5);
    assert!(lists.values().all(|&n| n == 1), "{lists:?}");
}

#[test]
fn cached_fs_gives_the_same_children() {
    let fs = dcim();
    let cached = CachedFs::new(&fs);
    let dir = resolve(&fs, &p("/Internal Storage/DCIM")).unwrap();
    let first = cached.list(&dir).unwrap();
    let second = cached.list(&dir).unwrap();
    let plain = fs.list(&dir).unwrap();
    let ids = |v: &[Node]| v.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&first), ids(&plain));
    assert_eq!(ids(&second), ids(&plain));
}
