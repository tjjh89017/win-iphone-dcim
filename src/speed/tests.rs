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
