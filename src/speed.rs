//! Transfer speed over a sliding window, with the time given by the caller.
//!
//! `current` looks at the last three seconds, so it follows stalls and
//! bursts. `average` is the whole run, so it moves slowly.

use std::time::{Duration, Instant};

const BUCKET: Duration = Duration::from_millis(250);
const BUCKETS: usize = 12;
/// The window of `current`: `BUCKETS` buckets, three seconds.
const WINDOW: Duration = Duration::from_millis(250 * BUCKETS as u64);

pub struct SpeedMeter {
    start: Instant,
    total: u64,
    /// Bytes per bucket. Bucket `i` is at `ring[i % BUCKETS]`.
    ring: [u64; BUCKETS],
    /// Index of the newest bucket: time since `start` over `BUCKET`.
    head: u64,
}

impl SpeedMeter {
    pub fn new(now: Instant) -> Self {
        Self {
            start: now,
            total: 0,
            ring: [0; BUCKETS],
            head: 0,
        }
    }

    /// Start again at `now`.
    pub fn reset(&mut self, now: Instant) {
        *self = Self::new(now);
    }

    fn index(&self, now: Instant) -> u64 {
        let since = now.saturating_duration_since(self.start);
        (since.as_millis() / BUCKET.as_millis()) as u64
    }

    /// Move the head to `now` and empty the buckets that it passes.
    fn advance(&mut self, now: Instant) {
        let index = self.index(now);
        if index <= self.head {
            return;
        }
        let steps = (index - self.head).min(BUCKETS as u64);
        for i in 1..=steps {
            self.ring[((self.head + i) % BUCKETS as u64) as usize] = 0;
        }
        self.head = index;
    }

    pub fn add(&mut self, bytes: u64, now: Instant) {
        self.advance(now);
        self.total += bytes;
        self.ring[(self.head % BUCKETS as u64) as usize] += bytes;
    }

    /// Bytes per second over the last three seconds.
    pub fn current(&self, now: Instant) -> f64 {
        let index = self.index(now);
        // Buckets from `index - 11` to `index` are in the window.
        let behind = index.saturating_sub(self.head);
        if behind >= BUCKETS as u64 {
            return 0.0;
        }
        let first = (index + 1).saturating_sub(BUCKETS as u64);
        let sum: u64 = (first..=self.head)
            .map(|i| self.ring[(i % BUCKETS as u64) as usize])
            .sum();
        let span = now.saturating_duration_since(self.start).min(WINDOW);
        sum as f64 / span.max(BUCKET).as_secs_f64()
    }

    /// Bytes per second since the start.
    pub fn average(&self, now: Instant) -> f64 {
        let secs = now.saturating_duration_since(self.start).as_secs_f64();
        if secs <= 0.0 {
            0.0
        } else {
            self.total as f64 / secs
        }
    }

    /// The time since the start.
    pub fn elapsed(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.start)
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// The time for `remaining` bytes at the current speed, or at the
    /// average speed when the current speed is 0. `None` if both are 0.
    pub fn eta(&self, remaining: u64, now: Instant) -> Option<Duration> {
        let mut speed = self.current(now);
        if speed <= 0.0 {
            speed = self.average(now);
        }
        (speed > 0.0).then(|| Duration::from_secs_f64(remaining as f64 / speed))
    }
}

/// `12.3 MiB/s` for a speed in bytes per second.
pub fn format_speed(bps: f64) -> String {
    format!("{:.1} MiB/s", bps / (1024.0 * 1024.0))
}

/// `1:05` or `1:02:03`.
pub fn format_eta(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn steady_stream_gives_the_speed() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::new(t0);
        // 1 MiB every 250 ms for 5 s: 4 MiB/s.
        for i in 1..=20 {
            m.add(1 << 20, ms(t0, i * 250));
        }
        let now = ms(t0, 5000);
        let cur = m.current(now);
        assert!((cur - 4.0 * 1048576.0).abs() < 0.5 * 1048576.0, "{cur}");
        assert!((m.average(now) - 4.0 * 1048576.0).abs() < 1.0);
    }

    #[test]
    fn pause_drops_current_but_average_keeps_falling() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::new(t0);
        for i in 1..=8 {
            m.add(1 << 20, ms(t0, i * 250));
        }
        let at2 = ms(t0, 2000);
        let avg2 = m.average(at2);
        let at7 = ms(t0, 7000);
        assert_eq!(m.current(at7), 0.0);
        assert!(m.average(at7) < avg2);
        assert!(m.average(at7) > 0.0);
    }

    #[test]
    fn eta_falls_back_to_average() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::new(t0);
        assert_eq!(m.eta(100, t0), None);
        m.add(1000, ms(t0, 250));
        let later = ms(t0, 10_000);
        assert_eq!(m.current(later), 0.0);
        let eta = m.eta(1000, later).unwrap();
        assert_eq!(eta, Duration::from_secs(10));
    }

    #[test]
    fn old_buckets_roll_out_of_the_window() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::new(t0);
        m.add(3000, ms(t0, 100));
        // 12 buckets later the ring slot is reused.
        m.add(600, ms(t0, 3100));
        let cur = m.current(ms(t0, 3100));
        assert!((cur - 600.0 / 3.0).abs() < 1.0, "{cur}");
        assert_eq!(m.total(), 3600);
    }

    #[test]
    fn reset_starts_again() {
        let t0 = Instant::now();
        let mut m = SpeedMeter::new(t0);
        m.add(5, t0);
        m.reset(ms(t0, 1000));
        assert_eq!(m.total(), 0);
        assert_eq!(m.current(ms(t0, 1000)), 0.0);
    }

    #[test]
    fn formats() {
        assert_eq!(format_eta(Duration::from_secs(65)), "1:05");
        assert_eq!(format_eta(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(format_speed(1048576.0 * 12.34), "12.3 MiB/s");
    }
}
