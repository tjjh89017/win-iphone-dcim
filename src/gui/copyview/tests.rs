use super::*;
use crate::gui::device::CurrentFile;
use std::time::Duration;

fn progress(done: u64, transferred: u64, current: Option<(&str, u64)>, t: Instant) -> CopyProgress {
    CopyProgress {
        files_found: 2,
        files_done: u64::from(current.is_none()),
        bytes_found: 2000,
        bytes_done: done,
        bytes_transferred: transferred,
        scanning: false,
        current: current.map(|(s, b)| CurrentFile {
            source: s.into(),
            size: Some(1000),
            bytes: b,
            started: t,
        }),
    }
}

#[test]
fn one_meter_lives_across_file_boundaries() {
    let t0 = Instant::now();
    let at = |ms| t0 + Duration::from_millis(ms);
    let mut v = CopyTracker::new(t0);
    v.start(t0);
    v.update(progress(0, 500, Some(("/a", 500)), t0), at(500));
    v.update(progress(1000, 1000, None, t0), at(1000));
    // Between files the last file stays on the bar.
    let (f, text) = v.file_line(at(1000));
    assert_eq!(f, 1.0);
    assert!(text.contains("(done)") && text.contains("/ 1000 B") || text.contains("(done)"));
    v.update(progress(1000, 1500, Some(("/b", 500)), t0), at(1500));
    v.update(progress(2000, 2000, None, t0), at(2000));
    assert_eq!(v.total(), 2000);
    assert!(v.meter.current(at(2000)) > 0.0);
    assert_eq!(v.fraction(), 1.0);
    v.finish(
        &CopySummary {
            copied: 2,
            bytes: 2000,
            ..CopySummary::default()
        },
        at(2000),
    );
    let line = v.overall_text(false, at(2000));
    assert!(line.starts_with("Done: copied 2"), "{line}");
    // The final line stays until the next run starts.
    assert!(v.final_line.is_some());
    v.start(at(3000));
    assert!(v.final_line.is_none());
}

#[test]
fn scan_state_lasts_until_the_first_progress() {
    let t0 = Instant::now();
    let mut v = CopyTracker::new(t0);
    assert!(!v.scanning);
    assert_eq!(v.scan_text(), None);
    v.scan(0, 0);
    assert!(v.scanning);
    assert_eq!(v.scan_text().unwrap(), "Scanning 0 files, 0 B");
    v.scan(3, 2048);
    assert_eq!(v.scanned, (3, 2048));
    assert_eq!(v.scan_text().unwrap(), "Scanning 3 files, 2.0 KiB");
    v.update(progress(0, 0, None, t0), t0);
    assert!(!v.scanning);
    assert_eq!(v.scan_text(), None);
    // A new run starts without the scan state.
    v.scan(1, 1);
    v.start(t0);
    assert_eq!((v.scanning, v.scanned), (false, (0, 0)));
    // A run cancelled during the scan ends the scan state.
    v.scan(1, 1);
    v.finish(
        &CopySummary {
            cancelled: true,
            ..CopySummary::default()
        },
        t0,
    );
    assert!(!v.scanning);
}

#[test]
fn fraction_stays_at_most_one_when_totals_are_too_small() {
    let t0 = Instant::now();
    let mut v = CopyTracker::new(t0);
    let mut p = progress(3000, 3000, None, t0);
    p.files_done = 3;
    v.update(p, t0);
    assert_eq!(v.fraction(), 1.0);
}
fn paste_file(number: usize, bytes: u64, size: Option<u64>) -> PasteFile {
    PasteFile {
        number,
        files: 4,
        path: "/DCIM/100APPLE/IMG_0001.HEIC".into(),
        bytes,
        size,
    }
}

#[test]
fn paste_file_bar_with_known_size() {
    let t0 = Instant::now();
    let mut p = PasteTracker::new(t0);
    assert_eq!(p.file_line(), None);
    p.progress(paste_file(1, 512, Some(2048)), t0);
    let (f, text) = p.file_line().unwrap();
    assert_eq!(f, Some(0.25));
    assert_eq!(text, "IMG_0001.HEIC  512 B / 2.0 KiB  25%");
}

#[test]
fn paste_file_bar_animates_with_unknown_size() {
    let t0 = Instant::now();
    let mut p = PasteTracker::new(t0);
    p.progress(paste_file(1, 512, None), t0);
    assert_eq!(
        p.file_line().unwrap(),
        (None, "IMG_0001.HEIC  512 B / ?".into())
    );
    p.progress(paste_file(1, 512, Some(0)), t0);
    assert_eq!(p.file_line().unwrap().0, None);
}

#[test]
fn paste_overall_uses_bytes_when_the_total_is_known() {
    let t0 = Instant::now();
    let at = |ms| t0 + Duration::from_millis(ms);
    let mut p = PasteTracker::new(t0);
    p.total = 4000;
    p.progress(paste_file(1, 500, Some(1000)), at(100));
    p.progress(paste_file(1, 900, Some(1000)), at(200));
    assert_eq!(p.bytes_done(), 900);
    // The bytes after the last progress reply count when the file ends.
    p.file_done(1, 4, "/a".into(), 1000, at(300));
    assert_eq!(p.fraction(), 0.25);
    p.progress(paste_file(2, 1000, Some(1000)), at(400));
    assert_eq!(p.fraction(), 0.5);
    let text = p.overall_text(at(400)).unwrap();
    assert!(
        text.starts_with("Explorer is reading 2 of 4 \u{b7} "),
        "{text}"
    );
    assert!(text.contains("2.0 KiB left"), "{text}");
}

#[test]
fn paste_overall_uses_files_when_the_total_is_unknown() {
    let t0 = Instant::now();
    let mut p = PasteTracker::new(t0);
    assert_eq!(p.fraction(), 0.0);
    p.progress(paste_file(3, 100, None), t0);
    assert_eq!(p.fraction(), 0.5);
    assert!(!p.overall_text(t0).unwrap().contains("left"));
}

#[test]
fn paste_fraction_stays_at_most_one() {
    let t0 = Instant::now();
    let mut p = PasteTracker::new(t0);
    p.total = 100;
    p.progress(paste_file(2, 5000, Some(1000)), t0);
    assert_eq!(p.fraction(), 1.0);
    assert_eq!(p.file_line().unwrap().0, Some(1.0));
}

#[test]
fn paste_ends_done_at_full_and_a_new_paste_runs_again() {
    let t0 = Instant::now();
    let at = |ms| t0 + Duration::from_millis(ms);
    let mut p = PasteTracker::new(t0);
    p.progress(paste_file(3, 100, None), t0);
    p.file_done(3, 4, "/c".into(), 100, at(500));
    assert!(p.running());
    p.file_done(4, 4, "/d".into(), 100, at(1000));
    assert!(!p.running());
    assert_eq!(p.end(), Some(EndState::Done));
    assert_eq!(p.fraction(), 1.0);
    assert_eq!(
        p.overall_text(at(1000)).unwrap(),
        "Done: 4 files, 200 B in 0:01"
    );
    // The next paste starts at file 1 and clears the end state.
    p.progress(paste_file(1, 10, None), at(2000));
    assert!(p.running());
    assert_eq!(p.end(), None);
    assert_eq!(p.bytes_done(), 10);
    p.clear();
    assert_eq!((p.live.as_ref(), p.end()), (None, None));
    assert_eq!(p.overall_text(t0), None);
}

#[test]
fn paste_cancel_and_failure_keep_the_fraction() {
    let t0 = Instant::now();
    let mut p = PasteTracker::new(t0);
    p.progress(paste_file(3, 100, None), t0);
    p.file_failed(
        3,
        4,
        "/c".into(),
        &format!("send /c to File Explorer: {CLOSED}"),
    );
    assert_eq!(p.end(), Some(EndState::Cancelled));
    assert_eq!(p.fraction(), 0.5);
    assert_eq!(p.overall_text(t0).unwrap(), "Cancelled: 2 of 4 files");
    p.progress(paste_file(1, 0, None), t0);
    p.file_failed(1, 4, "/a".into(), "device gone");
    assert_eq!(p.end(), Some(EndState::Failed));
    assert_eq!(p.fraction(), 0.0);
    assert_eq!(
        p.overall_text(t0).unwrap(),
        "Failed: 0 of 4 files \u{b7} device gone"
    );
    // A failure before any progress reply still shows the bars.
    let mut q = PasteTracker::new(t0);
    q.file_failed(2, 4, "/b".into(), "x");
    assert_eq!(q.live.as_ref().map(|l| l.number), Some(2));
    assert_eq!(q.fraction(), 0.25);
}

#[test]
fn copy_end_states() {
    let t0 = Instant::now();
    let mut v = CopyTracker::new(t0);
    v.start(t0);
    v.update(progress(500, 500, Some(("/a", 500)), t0), t0);
    let half = v.fraction();
    v.finish(
        &CopySummary {
            copied: 0,
            cancelled: true,
            ..CopySummary::default()
        },
        t0,
    );
    assert_eq!(v.end, Some(EndState::Cancelled));
    assert_eq!(v.fraction(), half);
    let line = v.overall_text(false, t0);
    assert!(line.starts_with("Cancelled: 0 of 2 files"), "{line}");
    v.start(t0);
    assert_eq!(v.end, None);
    v.update(progress(1000, 1000, None, t0), t0);
    v.fail("disk full");
    assert_eq!(v.end, Some(EndState::Failed));
    assert_eq!(v.fraction(), 0.5);
    assert_eq!(
        v.overall_text(false, t0),
        "Failed: 1 of 2 files \u{b7} disk full"
    );
    v.start(t0);
    v.update(progress(1000, 1000, None, t0), t0);
    v.finish(
        &CopySummary {
            copied: 1,
            failed: 1,
            ..CopySummary::default()
        },
        t0,
    );
    assert_eq!(v.end, Some(EndState::Failed));
    v.start(t0);
    v.update(progress(1000, 1000, None, t0), t0);
    v.finish(
        &CopySummary {
            copied: 1,
            bytes: 1000,
            ..CopySummary::default()
        },
        t0,
    );
    assert_eq!(v.end, Some(EndState::Done));
    assert_eq!(v.fraction(), 1.0);
}
