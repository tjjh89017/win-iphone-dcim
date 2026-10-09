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
