//! Transfer progress for `cp`.
//!
//! On a terminal (stderr) with text logs, `cp` draws an overall line and a
//! per-file bar with indicatif. Otherwise it emits progress as tracing
//! events: one at file start, one at file end, and at most one every two
//! seconds during a transfer. Log lines go through `LogWriter`, which
//! suspends the bars while it writes, so the two do not mix.

use std::io::{self, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

use crate::model::{human_size, human_speed};
use crate::speed::{SpeedMeter, format_eta, format_speed};

/// The bars that are on screen now. `LogWriter` suspends them.
static ACTIVE: Mutex<Option<MultiProgress>> = Mutex::new(None);

const EVENT_INTERVAL: Duration = Duration::from_secs(2);
const OVERALL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    /// indicatif bars on stderr.
    Bar,
    /// tracing events.
    Events,
    /// Nothing. Dry runs and tests.
    Off,
}

/// stderr writer for tracing-subscriber that does not break the bars.
pub struct LogWriter;

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let active = ACTIVE.lock().ok().and_then(|g| g.clone());
        match active {
            Some(multi) => multi.suspend(|| io::stderr().write_all(buf))?,
            None => io::stderr().write_all(buf)?,
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}

pub fn log_writer() -> LogWriter {
    LogWriter
}

struct Current {
    path: String,
    size: Option<u64>,
    bytes: u64,
    start: Instant,
    last_event: Instant,
    last_bar: Instant,
    meter: SpeedMeter,
    bar: Option<ProgressBar>,
}

pub struct Progress {
    mode: ProgressMode,
    multi: Option<MultiProgress>,
    overall: Option<ProgressBar>,
    /// Speed of the whole run.
    meter: SpeedMeter,
    last_overall: Instant,
    files_found: u64,
    files_done: u64,
    bytes_found: u64,
    /// Bytes of finished files (copied, skipped or failed) plus the current file.
    bytes_settled: u64,
    /// Bytes read from the device.
    bytes_transferred: u64,
    scanning: bool,
    current: Option<Current>,
}

impl Progress {
    pub fn new(mode: ProgressMode) -> Self {
        let now = Instant::now();
        let (multi, overall) = if mode == ProgressMode::Bar {
            let multi = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
            let overall = multi.add(ProgressBar::new_spinner());
            overall.set_style(
                ProgressStyle::with_template("{msg}  elapsed {elapsed_precise}")
                    .expect("valid template"),
            );
            overall.enable_steady_tick(Duration::from_millis(500));
            if let Ok(mut g) = ACTIVE.lock() {
                *g = Some(multi.clone());
            }
            (Some(multi), Some(overall))
        } else {
            (None, None)
        };
        let mut p = Self {
            mode,
            multi,
            overall,
            meter: SpeedMeter::new(now),
            last_overall: now,
            files_found: 0,
            files_done: 0,
            bytes_found: 0,
            bytes_settled: 0,
            bytes_transferred: 0,
            scanning: true,
            current: None,
        };
        p.update_overall();
        p
    }

    /// Run `f` with the bars hidden, for example to print a result line.
    pub fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        match &self.multi {
            Some(m) => m.suspend(f),
            None => f(),
        }
    }

    /// The planner found one more file. Totals grow while folders are listed.
    pub fn found(&mut self, size: Option<u64>) {
        self.files_found += 1;
        self.bytes_found += size.unwrap_or(0);
        self.update_overall();
    }

    /// A planned file is finished without a transfer (skipped or failed).
    pub fn settled(&mut self, size: Option<u64>) {
        self.files_done += 1;
        self.bytes_settled += size.unwrap_or(0);
        self.update_overall();
    }

    pub fn file_start(&mut self, path: &str, size: Option<u64>) {
        let now = Instant::now();
        let bar = self.multi.as_ref().map(|m| {
            let name = path.rsplit('/').next().unwrap_or(path).to_owned();
            let (bar, template) = match size {
                Some(len) => (
                    ProgressBar::new(len),
                    "{wide_msg} [{bar:24}] {bytes}/{total_bytes} {percent:>3}% {prefix}",
                ),
                None => (
                    ProgressBar::new_spinner(),
                    "{spinner} {wide_msg} {bytes} {prefix}",
                ),
            };
            bar.set_style(
                ProgressStyle::with_template(template)
                    .expect("valid template")
                    .progress_chars("=> "),
            );
            bar.set_message(name);
            m.add(bar)
        });
        if self.mode == ProgressMode::Events {
            tracing::info!(target: "progress", event = "file_start", path, size, "start {path}");
        }
        self.current = Some(Current {
            path: path.to_owned(),
            size,
            bytes: 0,
            start: now,
            last_event: now,
            last_bar: now,
            meter: SpeedMeter::new(now),
            bar,
        });
    }

    pub fn bytes(&mut self, n: u64) {
        self.bytes_transferred += n;
        let now = Instant::now();
        self.meter.add(n, now);
        let Some(cur) = self.current.as_mut() else {
            return;
        };
        cur.bytes += n;
        cur.meter.add(n, now);
        if let Some(bar) = &cur.bar {
            bar.inc(n);
            if now.duration_since(cur.last_bar) >= OVERALL_INTERVAL {
                cur.last_bar = now;
                bar.set_prefix(file_speed_text(cur, now));
            }
        }
        if self.mode == ProgressMode::Events && cur.last_event.elapsed() >= EVENT_INTERVAL {
            cur.last_event = now;
            tracing::info!(
                target: "progress",
                event = "file_progress",
                path = cur.path.as_str(),
                bytes = cur.bytes,
                size = cur.size,
                speed = human_speed(cur.bytes, cur.start.elapsed()).as_str(),
                speed_bps = cur.meter.current(now) as u64,
                avg_bps = cur.meter.average(now) as u64,
                "{} {}",
                cur.path,
                human_size(cur.bytes)
            );
        }
        if self.last_overall.elapsed() >= OVERALL_INTERVAL {
            self.update_overall();
        }
    }

    /// The current file starts again from the first byte after a failure.
    pub fn restart_file(&mut self) {
        if let Some(cur) = self.current.as_mut() {
            cur.bytes = 0;
            cur.start = Instant::now();
            cur.meter.reset(cur.start);
            if let Some(bar) = &cur.bar {
                bar.reset();
            }
        }
    }

    /// The current file is finished. `ok` is false for a failed transfer.
    pub fn file_end(&mut self, ok: bool) {
        let Some(cur) = self.current.take() else {
            return;
        };
        if let Some(bar) = cur.bar {
            bar.finish_and_clear();
            if let Some(m) = &self.multi {
                m.remove(&bar);
            }
        }
        if self.mode == ProgressMode::Events {
            let elapsed = cur.start.elapsed();
            tracing::info!(
                target: "progress",
                event = "file_end",
                path = cur.path.as_str(),
                ok,
                bytes = cur.bytes,
                elapsed_ms = elapsed.as_millis() as u64,
                speed = human_speed(cur.bytes, elapsed).as_str(),
                "end {}",
                cur.path
            );
        }
        self.files_done += 1;
        self.bytes_settled += cur.size.unwrap_or(cur.bytes);
        self.update_overall();
    }

    /// The planner has no more folders to list. The totals are final.
    pub fn scan_done(&mut self) {
        self.scanning = false;
        self.update_overall();
    }

    fn update_overall(&mut self) {
        self.last_overall = Instant::now();
        let Some(bar) = &self.overall else {
            return;
        };
        let more = if self.scanning { "+" } else { "" };
        let current = self.current.as_ref().map_or(0, |c| c.bytes);
        let now = Instant::now();
        let done = self.bytes_settled + current;
        let eta = if self.scanning || self.bytes_found == 0 {
            String::new()
        } else {
            self.meter
                .eta(self.bytes_found.saturating_sub(done), now)
                .map(|d| format!("  ETA {}", format_eta(d)))
                .unwrap_or_default()
        };
        bar.set_message(format!(
            "files {}/{}{more}  {}/{}{more}  {} (avg {}){eta}",
            self.files_done,
            self.files_found,
            human_size(done),
            human_size(self.bytes_found),
            format_speed(self.meter.current(now)),
            format_speed(self.meter.average(now)),
        ));
        if let Some(cur) = self.current.as_mut()
            && let Some(bar) = &cur.bar
        {
            cur.last_bar = now;
            bar.set_prefix(file_speed_text(cur, now));
        }
    }

    /// Remove the bars from the screen.
    pub fn finish(&mut self) {
        if let Some(cur) = self.current.take()
            && let Some(bar) = cur.bar
        {
            bar.finish_and_clear();
        }
        if let Some(bar) = self.overall.take() {
            bar.finish_and_clear();
        }
        if let Some(m) = self.multi.take() {
            let _ = m.clear();
        }
        if self.mode == ProgressMode::Bar
            && let Ok(mut g) = ACTIVE.lock()
        {
            *g = None;
        }
    }
}

/// `12.3 MiB/s ETA 0:42` for the bar of the current file.
fn file_speed_text(cur: &Current, now: Instant) -> String {
    let speed = format_speed(cur.meter.current(now));
    match cur
        .size
        .and_then(|s| cur.meter.eta(s.saturating_sub(cur.bytes), now))
    {
        Some(eta) => format!("{speed} ETA {}", format_eta(eta)),
        None => speed,
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_follow_the_run() {
        let mut p = Progress::new(ProgressMode::Off);
        p.found(Some(10));
        p.found(None);
        p.file_start("/a/b.jpg", Some(10));
        p.bytes(4);
        p.bytes(6);
        p.file_end(true);
        p.settled(None);
        p.scan_done();
        assert_eq!((p.files_found, p.files_done), (2, 2));
        assert_eq!(
            (p.bytes_found, p.bytes_settled, p.bytes_transferred),
            (10, 10, 10)
        );
    }

    #[test]
    fn hidden_bar_mode_works_without_a_terminal() {
        let mut p = Progress::new(ProgressMode::Events);
        p.found(Some(3));
        p.file_start("/x", Some(3));
        p.bytes(3);
        p.file_end(true);
        assert_eq!(p.suspend(|| 7), 7);
        p.finish();
    }
}
