use super::*;
use crate::device_fs::fake::{FakeFs, dcim};

struct FakeBackend(fn() -> FakeFs);

impl Backend for FakeBackend {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        Ok(vec![DeviceInfo {
            index: 0,
            friendly_name: Some("Apple iPhone".into()),
            manufacturer: Some("Apple Inc.".into()),
            description: None,
        }])
    }

    fn open(&self, index: Option<usize>) -> Result<Box<dyn DeviceFs>> {
        match index {
            None | Some(0) => Ok(Box::new((self.0)())),
            Some(index) => Err(Error::DeviceIndexOutOfRange { index, count: 1 }),
        }
    }
}

fn script(reqs: &[Request]) -> Vec<u8> {
    let mut input = Vec::new();
    for r in reqs {
        write_message(&mut input, r).unwrap();
    }
    input
}

/// Run `serve` and return (responses, data pipe bytes).
fn run(backend: &dyn Backend, reqs: &[Request]) -> (Vec<Response>, Vec<u8>) {
    let input = script(reqs);
    let mut control = Vec::new();
    let mut data = Vec::new();
    serve(backend, &mut input.as_slice(), &mut control, &mut data).unwrap();
    let mut r = control.as_slice();
    let mut out = Vec::new();
    while let Some(line) = read_line(&mut r).unwrap() {
        out.push(parse(&line).unwrap());
    }
    (out, data)
}

const MOV: &str = "/Internal Storage/DCIM/202601_a/IMG_0002.MOV";

#[test]
fn messages_are_tagged_json_lines() {
    let line = serde_json::to_string(&Request::Read {
        target: Target::path("/a"),
        chunk_size: 4,
    })
    .unwrap();
    assert_eq!(
        line,
        r#"{"type":"read","target":{"path":"/a"},"chunk_size":4}"#
    );
    let line = serde_json::to_string(&Request::ListDevices).unwrap();
    assert_eq!(line, r#"{"type":"list_devices"}"#);
    let err = Response::Error(WireError::from(&Error::NoDevice));
    let line = serde_json::to_string(&err).unwrap();
    assert!(line.starts_with(r#"{"type":"error","kind":"no_device","message":"#));
    assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), err);
}

#[test]
fn errors_keep_their_variant_across_the_wire() {
    let e = Error::from_hresult("IStream::Read", 0x8007_048F, "gone".into(), false);
    let back = WireError::from(&e).into_error();
    assert!(back.is_fatal());
    assert_eq!(back.exit_code(), e.exit_code());
    assert_eq!(back.to_string(), e.to_string());

    let e = Error::PathNotFound {
        path: "/x/y".into(),
        component: "x".into(),
    };
    assert_eq!(WireError::from(&e).into_error().to_string(), e.to_string());

    let e = Error::DeviceIndexOutOfRange { index: 3, count: 1 };
    assert_eq!(WireError::from(&e).into_error().to_string(), e.to_string());
}

#[test]
fn size_over_4_gib_is_carried_as_u64() {
    let mut fs = FakeFs::new();
    let big = fs.file(0, "BIG.MOV", b"x");
    fs.node_mut(big).size = Some(5 << 30);
    let node = fs.node_mut(big).clone();
    let json = serde_json::to_string(&Response::Node {
        node: (&node).into(),
    })
    .unwrap();
    assert!(json.contains("\"size\":5368709120"), "{json}");
    let Response::Node { node: back } = serde_json::from_str(&json).unwrap() else {
        panic!("wrong response");
    };
    assert_eq!(Node::from(back), node);
}

#[test]
fn frames_round_trip_and_end_with_an_empty_frame() {
    let mut pipe = Vec::new();
    write_frame(&mut pipe, b"abcd").unwrap();
    write_frame(&mut pipe, b"ef").unwrap();
    write_frame(&mut pipe, b"").unwrap();
    let mut r = pipe.as_slice();
    let mut buf = Vec::new();
    assert!(read_frame(&mut r, 4, &mut buf).unwrap());
    assert_eq!(buf, b"abcd");
    assert!(read_frame(&mut r, 4, &mut buf).unwrap());
    assert_eq!(buf, b"ef");
    assert!(!read_frame(&mut r, 4, &mut buf).unwrap());
    let eof = read_frame(&mut r, 4, &mut buf).unwrap_err();
    assert_eq!(eof.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn frame_longer_than_chunk_is_rejected() {
    let mut pipe = Vec::new();
    write_frame(&mut pipe, b"abcde").unwrap();
    let err = read_frame(&mut pipe.as_slice(), 4, &mut Vec::new()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn truncated_frame_is_an_error() {
    let mut pipe = Vec::new();
    write_frame(&mut pipe, b"abcd").unwrap();
    pipe.truncate(6);
    let err = read_frame(&mut pipe.as_slice(), 4, &mut Vec::new()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn serve_streams_exact_bytes_in_chunk_sized_frames() {
    let (out, data) = run(
        &FakeBackend(dcim),
        &[
            Request::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
            Request::Open { index: None },
            Request::Read {
                target: Target::path(MOV),
                chunk_size: 1000,
            },
            Request::Shutdown,
            Request::ListDevices,
        ],
    );
    assert_eq!(
        out[0],
        Response::Hello {
            protocol_version: PROTOCOL_VERSION
        }
    );
    assert!(matches!(&out[1], Response::Opened { root, .. } if root.is_folder));
    assert_eq!(
        out[2],
        Response::ReadDone {
            bytes: 2048,
            ok: true,
            error: None
        }
    );
    // Shutdown stops the loop: ListDevices is not served.
    assert_eq!(out.len(), 3);

    let mut r = data.as_slice();
    let mut buf = Vec::new();
    let mut sizes = Vec::new();
    while read_frame(&mut r, 1000, &mut buf).unwrap() {
        sizes.push(buf.len());
        assert!(buf.iter().all(|&b| b == 0));
    }
    assert_eq!(sizes, [1000, 1000, 48]);
    assert!(r.is_empty());
}

#[test]
fn serve_resolves_lists_and_reports_errors() {
    let (out, data) = run(
        &FakeBackend(dcim),
        &[
            Request::Hello {
                protocol_version: 99,
            },
            Request::List {
                target: Target::path("/"),
            },
            Request::Open { index: Some(1) },
            Request::Open { index: Some(0) },
            Request::Resolve {
                path: "/Internal Storage/DCIM".into(),
            },
            Request::List {
                target: Target::path("/Internal Storage/DCIM"),
            },
            Request::Stat {
                target: Target::path("/Internal Storage/DCIM/202601_b/IMG_0001.HEIC"),
            },
            Request::Read {
                target: Target::path("/Internal Storage/DCIM/nope"),
                chunk_size: 16,
            },
            Request::List {
                target: Target::path(MOV),
            },
        ],
    );
    assert!(matches!(&out[0], Response::Error(e) if e.message.contains("protocol version 99")));
    assert!(matches!(&out[1], Response::Error(e) if e.message.contains("send Open first")));
    assert!(matches!(&out[2], Response::Error(e) if e.kind == ErrorKind::DeviceIndexOutOfRange));
    assert!(matches!(&out[3], Response::Opened { .. }));
    let Response::Node { node: dcim_node } = &out[4] else {
        panic!("{:?}", out[4]);
    };
    let Response::Nodes { nodes } = &out[5] else {
        panic!("{:?}", out[5]);
    };
    let names: Vec<_> = nodes.iter().map(|n| n.name.clone().unwrap()).collect();
    assert_eq!(names, ["202601_a", "202601_b"]);
    assert!(matches!(&out[6], Response::Node { node } if node.size == Some(6)));
    let Response::ReadDone {
        bytes: 0,
        ok: false,
        error: Some(e),
    } = &out[7]
    else {
        panic!("{:?}", out[7]);
    };
    assert_eq!(e.kind, ErrorKind::PathNotFound);
    assert_eq!(e.component.as_deref(), Some("nope"));
    assert!(matches!(&out[8], Response::Error(e) if e.kind == ErrorKind::NotAFolder));
    // The failed read still ends its stream.
    assert_eq!(data, [0, 0, 0, 0]);

    // A known id is used without path resolution.
    let (out, _) = run(
        &FakeBackend(dcim),
        &[
            Request::Open { index: None },
            Request::Resolve {
                path: "/Internal Storage/DCIM".into(),
            },
            Request::List {
                target: Target {
                    path: "/stale/path".into(),
                    id: Some(dcim_node.id.clone()),
                },
            },
        ],
    );
    assert!(matches!(&out[2], Response::Nodes { nodes } if nodes.len() == 2));
}

#[test]
fn failed_device_read_reports_bytes_sent() {
    fn failing() -> FakeFs {
        let mut fs = dcim();
        fs.fail_read(5, 100, true);
        fs
    }
    let (out, data) = run(
        &FakeBackend(failing),
        &[
            Request::Open { index: None },
            Request::Read {
                target: Target::path(MOV),
                chunk_size: 64,
            },
        ],
    );
    let Response::ReadDone {
        bytes: 100,
        ok: false,
        error: Some(e),
    } = &out[1]
    else {
        panic!("{:?}", out[1]);
    };
    assert_eq!(e.kind, ErrorKind::DeviceUnavailable);
    assert!(e.clone().into_error().is_fatal());
    // 64 + 36 bytes in two frames, then the end frame.
    assert_eq!(data.len(), 4 + 64 + 4 + 36 + 4);
}

#[test]
fn long_line_and_missing_line_end_are_errors() {
    let mut r: &[u8] = b"{\"type\":\"shutdown\"}";
    assert_eq!(
        read_line(&mut r).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    let mut r: &[u8] = b"not json\n";
    let line = read_line(&mut r).unwrap().unwrap();
    assert_eq!(
        parse::<Request>(&line).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
