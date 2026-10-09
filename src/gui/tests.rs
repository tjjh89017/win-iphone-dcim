use std::io::Write;
use std::sync::{Arc, Mutex};

use tracing::level_filters::LevelFilter;

use super::*;

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

#[test]
fn log_file_arg_reads_both_forms_and_resolves_against_cwd() {
    let cwd = Path::new("/work");
    assert_eq!(log_file_arg(args(&[]), cwd), Ok(None));
    assert_eq!(log_file_arg(args(&["--other", "x"]), cwd), Ok(None));
    assert_eq!(
        log_file_arg(args(&["--log-file", "gui.log"]), cwd),
        Ok(Some(PathBuf::from("/work/gui.log")))
    );
    assert_eq!(
        log_file_arg(args(&["--log-file=/tmp/a.log"]), cwd),
        Ok(Some(PathBuf::from("/tmp/a.log")))
    );
    assert_eq!(
        log_file_arg(args(&["--log-file", "a", "--log-file=b"]), cwd),
        Ok(Some(PathBuf::from("/work/b")))
    );
}

#[test]
fn log_file_arg_without_a_value_is_an_error() {
    let cwd = Path::new("/work");
    assert!(log_file_arg(args(&["--log-file"]), cwd).is_err());
    assert!(log_file_arg(args(&["--log-file="]), cwd).is_err());
}

#[test]
fn argument_beats_env_and_env_beats_config() {
    let cwd = Path::new("/work");
    let arg = Some(PathBuf::from("/a.log"));
    let env = Some(OsString::from("e.log"));
    let config = Some(PathBuf::from("/c.log"));
    assert_eq!(
        pick_log_file(arg.clone(), env.clone(), config.clone(), cwd),
        arg
    );
    assert_eq!(
        pick_log_file(None, env, config.clone(), cwd),
        Some(PathBuf::from("/work/e.log"))
    );
    assert_eq!(
        pick_log_file(None, Some(OsString::from(" ")), config.clone(), cwd),
        config
    );
    assert_eq!(pick_log_file(None, None, None, cwd), None);
}

#[test]
fn open_log_file_creates_the_file_and_appends() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("gui.log");
    open_log_file(&path).unwrap().write_all(b"one\n").unwrap();
    assert!(path.exists());
    open_log_file(&path).unwrap().write_all(b"two\n").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
    assert!(open_log_file(&tmp.path().join("missing/gui.log")).is_err());
}

#[test]
fn log_filter_uses_rust_log_or_a_default() {
    let (filter, warning) = log_filter(Some("win_iphone_dcim=trace"), false);
    assert_eq!(filter.max_level_hint(), Some(LevelFilter::TRACE));
    assert_eq!(warning, None);

    let (filter, warning) = log_filter(None, true);
    assert_eq!(filter.max_level_hint(), Some(LevelFilter::DEBUG));
    assert_eq!(warning, None);

    let (filter, warning) = log_filter(Some(" "), false);
    assert_eq!(filter.max_level_hint(), Some(LevelFilter::INFO));
    assert_eq!(warning, None);
}

#[test]
fn invalid_rust_log_falls_back_to_info_with_a_warning() {
    let (filter, warning) = log_filter(Some("win_iphone_dcim=loud"), true);
    assert_eq!(filter.max_level_hint(), Some(LevelFilter::INFO));
    let warning = warning.expect("a warning");
    assert!(warning.contains("RUST_LOG"), "{warning}");

    // The warning reaches the log once the subscriber is up.
    let out = Arc::new(Mutex::new(Vec::new()));
    let writer = {
        let out = Arc::clone(&out);
        move || SharedBuf(Arc::clone(&out))
    };
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(writer)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        log_started(Some(Path::new("/work/gui.log")), Some(&warning));
        tracing::debug!("hidden at info");
    });
    let text = String::from_utf8(out.lock().unwrap().clone()).unwrap();
    assert!(text.contains("log started: "), "{text}");
    assert!(text.contains("file /work/gui.log"), "{text}");
    assert!(text.contains("WARN"), "{text}");
    assert!(!text.contains("hidden at info"), "{text}");
}

struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn logging_error_is_none_without_a_failure() {
    // Only `main` sets it; tests never call `main`.
    assert_eq!(logging_error(), None);
}
