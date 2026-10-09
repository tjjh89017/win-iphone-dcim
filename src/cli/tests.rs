use super::*;
use clap::CommandFactory;

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("win-iphone-dcim").chain(args.iter().copied()))
}

#[test]
fn cli_definition_is_valid() {
    Cli::command().debug_assert();
}

#[test]
fn devices_with_defaults() {
    let cli = parse(&["devices"]).unwrap();
    assert_eq!(cli.log_format, LogFormat::Text);
    assert_eq!(cli.device, None);
    assert!(matches!(cli.command, Command::Devices));
}

#[test]
fn ls_flags_and_global_device() {
    let cli = parse(&[
        "ls",
        "-lR",
        "-d",
        "1",
        "--json",
        "/Internal Storage/DCIM",
        "/",
    ])
    .unwrap();
    assert_eq!(cli.device, Some(1));
    let Command::Ls {
        long,
        recursive,
        json,
        paths,
        ..
    } = cli.command
    else {
        panic!("not ls");
    };
    assert!(long && recursive && json);
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0].components(), ["Internal Storage", "DCIM"]);
    assert!(paths[1].is_root());
}

#[test]
fn ls_sort_flags_conflict() {
    assert!(parse(&["ls", "-S", "-t"]).is_err());
    assert!(parse(&["ls", "-U", "-S"]).is_err());
    assert!(parse(&["ls", "-U", "-t"]).is_err());
    assert!(parse(&["ls", "-U", "-r"]).is_err());
    assert!(parse(&["ls", "-St"]).is_err());
    assert!(parse(&["ls", "-Sr"]).is_ok());
    assert!(parse(&["ls", "-tr"]).is_ok());
    assert!(parse(&["tree", "--dirs-first"]).is_ok());
}

#[test]
fn ls_without_path() {
    let cli = parse(&["ls"]).unwrap();
    assert!(matches!(cli.command, Command::Ls { ref paths, .. } if paths.is_empty()));
}

#[test]
fn ls_rejects_relative_path() {
    let err = parse(&["ls", "DCIM"]).unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn tree_with_depth_and_log_format() {
    let cli = parse(&[
        "--log-format",
        "json",
        "tree",
        "-L",
        "2",
        "/Internal Storage",
    ])
    .unwrap();
    assert_eq!(cli.log_format, LogFormat::Json);
    let Command::Tree {
        level, json, path, ..
    } = cli.command
    else {
        panic!("not tree");
    };
    assert_eq!(level, Some(2));
    assert!(!json);
    assert_eq!(path.unwrap().normalized(), "/Internal Storage");
}

#[test]
fn cp_splits_sources_and_dest_and_keeps_trailing_slash() {
    let cli = parse(&[
        "cp",
        "-r",
        "--dry-run",
        "/Internal Storage/DCIM/202601_a/",
        "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC",
        "D:\\iPhoneBackup",
    ])
    .unwrap();
    let Command::Cp {
        recursive,
        dry_run,
        sources,
        dest,
        ..
    } = cli.command
    else {
        panic!("not cp");
    };
    assert!(recursive && dry_run);
    assert_eq!(sources.len(), 2);
    assert!(sources[0].trailing_slash());
    assert!(!sources[1].trailing_slash());
    assert_eq!(dest, PathBuf::from("D:\\iPhoneBackup"));
}

#[test]
fn cp_conflict_and_archive_flags() {
    let cli = parse(&["cp", "-a", "-f", "/a", "D:\\x"]).unwrap();
    let Command::Cp {
        archive,
        force,
        no_clobber,
        ..
    } = cli.command
    else {
        panic!("not cp");
    };
    assert!(archive && force && !no_clobber);
    assert!(parse(&["cp", "-n", "/a", "x"]).is_ok());
    assert!(parse(&["cp", "--no-clobber", "-p", "/a", "x"]).is_ok());
    let err = parse(&["cp", "-f", "-n", "/a", "x"]).unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn cp_verify_values() {
    let cli = parse(&["cp", "--verify", "local-hash", "/a", "x"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Cp {
            verify: Some(VerifyMode::LocalHash),
            ..
        }
    ));
    assert!(parse(&["cp", "--verify", "size", "/a", "x"]).is_ok());
    assert!(parse(&["cp", "--verify", "md5", "/a", "x"]).is_err());
}

#[test]
fn cp_needs_src_and_dest() {
    assert!(parse(&["cp", "/a"]).is_err());
    assert!(parse(&["cp"]).is_err());
}

#[test]
fn fetch_and_object_flag_are_gone() {
    assert!(parse(&["fetch"]).is_err());
    assert!(parse(&["ls", "--object", "o1"]).is_err());
}

#[test]
fn retries_and_diagnostic_are_global() {
    let cli = parse(&["cp", "--retries", "5", "--diagnostic", "/a", "x"]).unwrap();
    assert_eq!(cli.retries, 5);
    assert!(cli.diagnostic);
    let cli = parse(&["devices"]).unwrap();
    assert_eq!(cli.retries, 3);
    assert!(!cli.diagnostic);
    assert!(parse(&["--retries", "-1", "devices"]).is_err());
}

#[test]
fn verify_command() {
    let cli = parse(&["verify", "--hash", "D:\\Backup"]).unwrap();
    let Command::Verify { hash, dest } = cli.command else {
        panic!("not verify");
    };
    assert!(hash);
    assert_eq!(dest, PathBuf::from("D:\\Backup"));
    assert!(parse(&["verify"]).is_err());
}

#[test]
fn timeout_and_no_isolate_are_global() {
    let cli = parse(&["devices"]).unwrap();
    assert_eq!(cli.timeout.0, DEFAULT_TIMEOUT);
    assert!(!cli.no_isolate);
    let cli = parse(&["ls", "--timeout", "5", "--no-isolate"]).unwrap();
    assert_eq!(cli.timeout.0, Duration::from_secs(5));
    assert!(cli.no_isolate);
    for (arg, secs) in [("90s", 90), ("2m", 120), ("1h", 3600)] {
        assert_eq!(
            parse(&["--timeout", arg, "devices"]).unwrap().timeout.0,
            Duration::from_secs(secs)
        );
    }
    for bad in ["0", "0s", "-1", "1.5", "2d", "s", ""] {
        assert!(parse(&["--timeout", bad, "devices"]).is_err(), "{bad}");
    }
}

#[test]
fn worker_is_hidden_and_needs_the_pipe() {
    let cli = parse(&["worker", "--data-pipe", "7"]).unwrap();
    assert!(matches!(cli.command, Command::Worker { data_pipe: 7 }));
    assert!(parse(&["worker"]).is_err());
    let help = Cli::command().render_help().to_string();
    assert!(
        !help.lines().any(|l| l.trim_start().starts_with("worker")),
        "{help}"
    );
}

#[test]
fn rejects_unknown_log_format() {
    assert!(parse(&["--log-format", "xml", "devices"]).is_err());
}
