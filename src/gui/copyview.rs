//! What the copy progress area shows: one speed meter for the whole run,
//! one for the current file, and the last lines after the run ends.

use std::time::Instant;

use super::device::CopyProgress;
use crate::backup::engine::CopySummary;
use crate::model::human_size;
use crate::speed::{SpeedMeter, format_eta, format_speed};

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
        self.end_file();
        self.final_line = Some(format!(
            "{}: copied {}, skipped {}, exists {}, failed {}  {}  elapsed {}  avg {}",
            if summary.cancelled {
                "Cancelled"
            } else {
                "Done"
            },
            summary.copied,
            summary.skipped,
            summary.exists,
            summary.failed,
            human_size(summary.bytes),
            format_eta(now.saturating_duration_since(self.meter_start(now))),
            format_speed(self.meter.average(now)),
        ));
    }

    fn meter_start(&self, now: Instant) -> Instant {
        now.checked_sub(self.meter.elapsed(now)).unwrap_or(now)
    }

    /// Bytes done, including the file in transfer.
    pub fn bytes_done(&self) -> u64 {
        self.progress.bytes_done + self.progress.current.as_ref().map_or(0, |c| c.bytes)
    }

    /// 0..1 of the bytes, or of the files if no size is known.
    pub fn fraction(&self) -> f32 {
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
}
