use std::cell::Cell;
use std::sync::mpsc::{self, Receiver, Sender};

use super::*;
use crate::cmd::cp::{self, CpOptions};
use crate::device_fs::fake::dcim;

/// Events as a GUI receives them over a channel.
#[derive(Debug, PartialEq)]
enum Event {
    Start(String),
    Bytes(u64),
    End(bool),
    Note(Note, String),
    Summary(CopySummary),
}

struct ChannelSink {
    tx: Sender<Event>,
    /// Answer `cancelled` with true after this many calls.
    cancel_after: Option<usize>,
    calls: Cell<usize>,
}

impl ChannelSink {
    fn new(cancel_after: Option<usize>) -> (Self, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let sink = Self {
            tx,
            cancel_after,
            calls: Cell::new(0),
        };
        (sink, rx)
    }
}

impl ProgressSink for ChannelSink {
    fn file_start(&mut self, source: &str, _size: Option<u64>) {
        self.tx.send(Event::Start(source.into())).unwrap();
    }

    fn bytes(&mut self, n: u64) {
        self.tx.send(Event::Bytes(n)).unwrap();
    }

    fn file_end(&mut self, ok: bool) {
        self.tx.send(Event::End(ok)).unwrap();
    }

    fn note(&mut self, note: Note, text: &str) {
        self.tx.send(Event::Note(note, text.into())).unwrap();
    }

    fn summary(&mut self, summary: &CopySummary) {
        self.tx.send(Event::Summary(summary.clone())).unwrap();
    }

    fn cancelled(&self) -> bool {
        let n = self.calls.get();
        self.calls.set(n + 1);
        self.cancel_after.is_some_and(|limit| n >= limit)
    }
}

fn opts() -> CopyOptions {
    CopyOptions {
        recursive: true,
        sleep: |_| {},
        ..CopyOptions::default()
    }
}

fn dcim_path() -> Vec<DevicePath> {
    vec![DevicePath::parse("/Internal Storage/DCIM").unwrap()]
}

fn summaries(rx: &Receiver<Event>) -> Vec<CopySummary> {
    rx.try_iter()
        .filter_map(|e| match e {
            Event::Summary(s) => Some(s),
            _ => None,
        })
        .collect()
}

#[test]
fn channel_sink_gets_the_same_counts_as_the_cli() {
    let gui = tempfile::tempdir().unwrap();
    let cli = tempfile::tempdir().unwrap();
    let fs = dcim();
    // An existing file of another size: skipped with a warning.
    std::fs::create_dir_all(gui.path().join("DCIM/202601_b")).unwrap();
    std::fs::write(gui.path().join("DCIM/202601_b/IMG_0001.HEIC"), b"x").unwrap();
    std::fs::create_dir_all(cli.path().join("DCIM/202601_b")).unwrap();
    std::fs::write(cli.path().join("DCIM/202601_b/IMG_0001.HEIC"), b"x").unwrap();

    let (mut sink, rx) = ChannelSink::new(None);
    let summary = run(
        &fs,
        &dcim_path(),
        gui.path(),
        opts(),
        &mut sink,
        &mut std::io::sink(),
    )
    .unwrap();
    let mut out = Vec::new();
    let cp_opts = CpOptions {
        recursive: true,
        sleep: |_| {},
        ..CpOptions::default()
    };
    let failed = cp::run(&fs, &dcim_path(), cli.path(), cp_opts, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();

    assert_eq!(failed, summary.failed);
    let done = format!(
        "[done] copied={} skipped={} exists={} failed={}  total=",
        summary.copied, summary.skipped, summary.exists, summary.failed
    );
    assert!(out.contains(&done), "{out}\n{summary:?}");
    assert_eq!((summary.copied, summary.skipped, summary.exists), (2, 1, 1));
    assert_eq!(summary.bytes, 6 + 2048);
    assert!(!summary.cancelled);

    let events: Vec<Event> = rx.try_iter().collect();
    let starts = events
        .iter()
        .filter(|e| matches!(e, Event::Start(_)))
        .count();
    let ends = events
        .iter()
        .filter(|e| matches!(e, Event::End(true)))
        .count();
    let bytes: u64 = events
        .iter()
        .map(|e| match e {
            Event::Bytes(n) => *n,
            _ => 0,
        })
        .sum();
    assert_eq!((starts, ends, bytes), (2, 2, 6 + 2048));
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::Note(Note::Skip, t) if t.contains("202601_b") && t.contains("--force")
        )),
        "{events:?}"
    );
    // Every stdout line of the CLI is also a note.
    let notes: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::Note(Note::Copy | Note::Summary, t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), out.lines().count(), "{notes:?}\n{out}");
    assert!(
        events
            .last()
            .is_some_and(|e| matches!(e, Event::Summary(_)))
    );
}

#[test]
fn cancel_stops_before_the_next_item() {
    let tmp = tempfile::tempdir().unwrap();
    // The first check passes; the run stops before the second item.
    let (mut sink, rx) = ChannelSink::new(Some(1));
    let summary = run(
        &dcim(),
        &dcim_path(),
        tmp.path(),
        opts(),
        &mut sink,
        &mut std::io::sink(),
    )
    .unwrap();
    assert!(summary.cancelled);
    assert_eq!(summary.copied, 0);
    assert_eq!(summaries(&rx), [summary]);
    assert!(!tmp.path().join("DCIM/202601_a/IMG_0001.HEIC").exists());
}
