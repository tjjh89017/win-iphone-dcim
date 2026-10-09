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
mod tests;
