//! What the copy progress area shows: one speed meter for the whole run,
//! one for the current file, and the last lines after the run ends. It also
//! shows the bars of a File Explorer paste.

use std::time::Instant;

use super::chunks::CLOSED;
use super::device::CopyProgress;
use crate::backup::engine::CopySummary;
use crate::model::human_size;
use crate::speed::{SpeedMeter, format_eta, format_speed};

/// How a copy or paste ended. The overall bar keeps this state until the
/// next run starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndState {
    Done,
    Cancelled,
    Failed,
}

/// The last file that was in transfer.
#[derive(Debug, Clone)]
pub struct FileLine {
    pub source: String,
    pub bytes: u64,
    pub size: Option<u64>,
    /// The speed of the file at its last update, in bytes per second.
    pub speed: f64,
    pub finished: bool,
}

pub struct CopyTracker {
    /// Lives for the whole run, across all files.
    meter: SpeedMeter,
    file_meter: SpeedMeter,
    seen: u64,
    pub progress: CopyProgress,
    pub last_file: Option<FileLine>,
    /// The result line of the last run. Stays until the next run starts.
    pub final_line: Option<String>,
    /// The device thread walks the set for the totals. No file is copied yet.
    pub scanning: bool,
    /// Files and bytes that the scan found so far.
    pub scanned: (u64, u64),
    /// How the last run ended. `None` while it runs.
    pub end: Option<EndState>,
}

impl CopyTracker {
    pub fn new(now: Instant) -> Self {
        Self {
            meter: SpeedMeter::new(now),
            file_meter: SpeedMeter::new(now),
            seen: 0,
            progress: CopyProgress::default(),
            last_file: None,
            final_line: None,
            scanning: false,
            scanned: (0, 0),
            end: None,
        }
    }

    /// A scan reply of the device thread. The scan runs until the first
    /// progress reply.
    pub fn scan(&mut self, files: u64, bytes: u64) {
        self.scanning = true;
        self.scanned = (files, bytes);
    }

    /// The text of the overall bar during the scan.
    pub fn scan_text(&self) -> Option<String> {
        self.scanning.then(|| {
            format!(
                "Scanning {} files, {}",
                self.scanned.0,
                human_size(self.scanned.1)
            )
        })
    }

    /// A new run starts.
    pub fn start(&mut self, now: Instant) {
        *self = Self::new(now);
    }

    /// A progress reply of the device thread.
    pub fn update(&mut self, p: CopyProgress, now: Instant) {
        self.scanning = false;
        self.meter
            .add(p.bytes_transferred.saturating_sub(self.seen), now);
        let delta = p.bytes_transferred.saturating_sub(self.seen);
        self.seen = p.bytes_transferred;
        match &p.current {
            Some(c) => {
                let same = self
                    .last_file
                    .as_ref()
                    .is_some_and(|l| !l.finished && l.source == c.source && l.bytes <= c.bytes);
                if same {
                    self.file_meter.add(delta, now);
                } else {
                    self.file_meter.reset(now);
                    self.file_meter.add(c.bytes, now);
                }
                self.last_file = Some(FileLine {
                    source: c.source.clone(),
                    bytes: c.bytes,
                    size: c.size,
                    speed: self.file_meter.current(now),
                    finished: false,
                });
            }
            None => self.end_file(),
        }
        self.progress = p;
    }

    /// The file is done: keep its line, complete.
    fn end_file(&mut self) {
        if let Some(l) = self.last_file.as_mut() {
            l.finished = true;
            if let Some(size) = l.size {
                l.bytes = size;
            }
        }
    }

    /// The run ended. Keep the overall line.
    pub fn finish(&mut self, summary: &CopySummary, now: Instant) {
        self.scanning = false;
        let detail = format!(
            "copied {}, skipped {}, exists {}, failed {}  {}  elapsed {}  avg {}",
            summary.copied,
            summary.skipped,
            summary.exists,
            summary.failed,
            human_size(summary.bytes),
            format_eta(now.saturating_duration_since(self.meter_start(now))),
            format_speed(self.meter.average(now)),
        );
        let (end, line) = if summary.cancelled {
            (
                EndState::Cancelled,
                format!("Cancelled: {} \u{b7} {detail}", self.files_text()),
            )
        } else if summary.failed > 0 {
            (
                EndState::Failed,
                format!(
                    "Failed: {} \u{b7} {} failed  {detail}",
                    self.files_text(),
                    summary.failed
                ),
            )
        } else {
            self.end_file();
            (EndState::Done, format!("Done: {detail}"))
        };
        self.end = Some(end);
        self.final_line = Some(line);
    }

    /// The run stopped with `error`. Keep the overall line.
    pub fn fail(&mut self, error: &str) {
        self.scanning = false;
        self.end = Some(EndState::Failed);
        self.final_line = Some(format!("Failed: {} \u{b7} {error}", self.files_text()));
    }

    /// `N of M files` for the end lines.
    fn files_text(&self) -> String {
        let p = &self.progress;
        format!(
            "{} of {} files",
            p.files_done.min(p.files_found),
            p.files_found
        )
    }

    fn meter_start(&self, now: Instant) -> Instant {
        now.checked_sub(self.meter.elapsed(now)).unwrap_or(now)
    }

    /// Bytes done, including the file in transfer.
    pub fn bytes_done(&self) -> u64 {
        self.progress.bytes_done + self.progress.current.as_ref().map_or(0, |c| c.bytes)
    }

    /// 0..1 of the bytes, or of the files if no size is known. A finished
    /// run shows 1.0; a cancelled or failed run keeps the point where it
    /// stopped.
    pub fn fraction(&self) -> f32 {
        if self.end == Some(EndState::Done) {
            return 1.0;
        }
        let p = &self.progress;
        let f = if p.bytes_found > 0 {
            self.bytes_done() as f32 / p.bytes_found as f32
        } else if p.files_found > 0 {
            p.files_done as f32 / p.files_found as f32
        } else {
            0.0
        };
        f.clamp(0.0, 1.0)
    }

    /// The text of the overall bar.
    pub fn overall_text(&self, running: bool, now: Instant) -> String {
        if !running && let Some(line) = &self.final_line {
            return line.clone();
        }
        let p = &self.progress;
        let mut text = format!(
            "Overall: files {}/{}  {} / {}  {} (avg {})  elapsed {}",
            p.files_done.min(p.files_found),
            p.files_found,
            human_size(self.bytes_done().min(p.bytes_found.max(self.bytes_done()))),
            human_size(p.bytes_found),
            format_speed(self.meter.current(now)),
            format_speed(self.meter.average(now)),
            format_eta(self.meter.elapsed(now)),
        );
        let left = p.bytes_found.saturating_sub(self.bytes_done());
        if running && let Some(eta) = self.meter.eta(left, now) {
            text.push_str(&format!("  ETA {}", format_eta(eta)));
        }
        text
    }

    /// The fraction and text of the file bar. Between files it shows the
    /// last file.
    pub fn file_line(&self, now: Instant) -> (f32, String) {
        let Some(l) = &self.last_file else {
            return (0.0, "No file in transfer".into());
        };
        let name = l.source.rsplit('/').next().unwrap_or(&l.source);
        let fraction = l
            .size
            .filter(|&s| s > 0)
            .map_or(0.0, |s| (l.bytes as f32 / s as f32).clamp(0.0, 1.0));
        let size = l.size.map(human_size).unwrap_or_else(|| "?".into());
        let mut text = format!(
            "{name}  {} / {size}  {:.0}%  ",
            human_size(l.bytes),
            fraction * 100.0
        );
        if l.finished {
            text.push_str(&format!("{} (done)", format_speed(l.speed)));
        } else {
            text.push_str(&format_speed(self.file_meter.current(now)));
            if let Some(eta) = l
                .size
                .and_then(|s| self.file_meter.eta(s.saturating_sub(l.bytes), now))
            {
                text.push_str(&format!("  ETA {}", format_eta(eta)));
            }
        }
        (fraction, text)
    }

    /// Bytes counted by the run meter.
    pub fn total(&self) -> u64 {
        self.meter.total()
    }
}

/// The file that File Explorer reads now, or the last file it read.
#[derive(Debug, Clone, PartialEq)]
pub struct PasteFile {
    pub number: usize,
    pub files: usize,
    pub path: String,
    pub bytes: u64,
    pub size: Option<u64>,
}

/// What the paste bars show while File Explorer reads files.
pub struct PasteTracker {
    /// Lives for the whole paste, across all files.
    meter: SpeedMeter,
    /// Between files it holds the last file read.
    pub live: Option<PasteFile>,
    /// Bytes of the files on the clipboard, 0 if unknown.
    pub total: u64,
    /// How the last paste ended, and its overall line. `None` while it runs.
    end: Option<(EndState, String)>,
}

impl PasteTracker {
    pub fn new(now: Instant) -> Self {
        Self {
            meter: SpeedMeter::new(now),
            live: None,
            total: 0,
            end: None,
        }
    }

    /// True while File Explorer reads; false after the paste ended.
    pub fn running(&self) -> bool {
        self.live.is_some() && self.end.is_none()
    }

    pub fn end(&self) -> Option<EndState> {
        self.end.as_ref().map(|(e, _)| *e)
    }

    /// A progress reply of the device thread.
    pub fn progress(&mut self, f: PasteFile, now: Instant) {
        if self.end.take().is_some() && f.number == 1 {
            // A new paste: the meter starts again.
            self.live = None;
        }
        let delta = match &self.live {
            Some(l) if l.number == f.number && f.bytes >= l.bytes => f.bytes - l.bytes,
            Some(l) if l.number != f.number && f.number != 1 => f.bytes,
            Some(_) => {
                // A new paste starts at file 1 or reads a file again.
                if f.number == 1 {
                    self.meter.reset(now);
                }
                f.bytes
            }
            None => {
                self.meter.reset(now);
                f.bytes
            }
        };
        self.meter.add(delta, now);
        self.live = Some(f);
    }

    /// File `number` of `files` ended after `bytes`. The last file ends the
    /// paste as done.
    pub fn file_done(
        &mut self,
        number: usize,
        files: usize,
        path: String,
        bytes: u64,
        now: Instant,
    ) {
        self.progress(
            PasteFile {
                number,
                files,
                path,
                bytes,
                size: Some(bytes),
            },
            now,
        );
        if number >= files {
            let line = format!(
                "Done: {files} files, {} in {}",
                human_size(self.bytes_done()),
                format_eta(self.meter.elapsed(now))
            );
            self.end = Some((EndState::Done, line));
        }
    }

    /// File `number` of `files` stopped with `error`. A closed reader means
    /// that File Explorer cancelled the paste.
    pub fn file_failed(&mut self, number: usize, files: usize, path: String, error: &str) {
        if self.live.as_ref().is_none_or(|l| l.number != number) {
            self.live = Some(PasteFile {
                number,
                files,
                path,
                bytes: 0,
                size: None,
            });
        }
        let done = number.saturating_sub(1);
        let end = if error.contains(CLOSED) {
            (
                EndState::Cancelled,
                format!("Cancelled: {done} of {files} files"),
            )
        } else {
            (
                EndState::Failed,
                format!("Failed: {done} of {files} files \u{b7} {error}"),
            )
        };
        self.end = Some(end);
    }

    /// Remove the bars: a new clipboard copy or another device.
    pub fn clear(&mut self) {
        self.live = None;
        self.end = None;
    }

    /// Bytes counted by the paste meter.
    pub fn bytes_done(&self) -> u64 {
        self.meter.total()
    }

    /// The fraction and text of the file bar. The fraction is `None` when
    /// the size is not known; the bar then animates.
    pub fn file_line(&self) -> Option<(Option<f32>, String)> {
        let l = self.live.as_ref()?;
        let name = l.path.rsplit('/').next().unwrap_or(&l.path);
        let fraction = l
            .size
            .filter(|&s| s > 0)
            .map(|s| (l.bytes as f32 / s as f32).clamp(0.0, 1.0));
        let size = l.size.map(human_size).unwrap_or_else(|| "?".into());
        let mut text = format!("{name}  {} / {size}", human_size(l.bytes));
        if let Some(f) = fraction {
            text.push_str(&format!("  {:.0}%", f * 100.0));
        }
        Some((fraction, text))
    }

    /// 0..1 of the bytes, or of the files done if the total is not known.
    /// A finished paste shows 1.0.
    pub fn fraction(&self) -> f32 {
        let Some(l) = &self.live else {
            return 0.0;
        };
        if self.end() == Some(EndState::Done) {
            return 1.0;
        }
        let f = if self.total > 0 {
            self.bytes_done() as f32 / self.total as f32
        } else if l.files > 0 {
            l.number.saturating_sub(1) as f32 / l.files as f32
        } else {
            0.0
        };
        f.clamp(0.0, 1.0)
    }

    /// The text of the overall bar: file N of M, speed, bytes left and ETA.
    /// After the end it is the end line.
    pub fn overall_text(&self, now: Instant) -> Option<String> {
        let l = self.live.as_ref()?;
        if let Some((_, line)) = &self.end {
            return Some(line.clone());
        }
        let mut text = format!(
            "Explorer is reading {} of {} \u{b7} {}",
            l.number,
            l.files,
            format_speed(self.meter.current(now))
        );
        if self.total > 0 {
            let left = self.total.saturating_sub(self.bytes_done());
            text.push_str(&format!(" \u{b7} {} left", human_size(left)));
            if let Some(eta) = self.meter.eta(left, now) {
                text.push_str(&format!("  ETA {}", format_eta(eta)));
            }
        }
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::device::CurrentFile;
    use std::time::Duration;

    fn progress(
        done: u64,
        transferred: u64,
        current: Option<(&str, u64)>,
        t: Instant,
    ) -> CopyProgress {
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
}
