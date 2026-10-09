//! win-iphone-dcim: read-only iPhone DCIM access over Windows Portable Devices.

// Off Windows the WPD backend is a stub, so the portable helpers that only
// it calls (HRESULT mapping, device selection, date and buffer helpers) look
// unused. They are still compiled and unit-tested there.
#![cfg_attr(not(windows), allow(dead_code))]

mod backup;
mod cli;
mod cmd;
mod device_fs;
mod devpath;
mod error;
mod model;
mod paths;
mod progress;
mod wpd;

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command, LogFormat, VerifyMode};
use crate::cmd::{
    cp::{CpOptions, OnExists},
    ls::LsOptions,
    sort::SortKey,
    verify::VerifyOptions,
};
use crate::devpath::DevicePath;
use crate::error::{Error, exit};
use crate::progress::ProgressMode;

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { exit::CLI } else { exit::OK } as u8);
        }
    };
    init_logging(cli.log_format);

    let code = match run(&cli) {
        Ok(0) => exit::OK,
        Ok(_) => exit::FILE_FAILED,
        Err(e) if e.is_broken_pipe() => exit::OK,
        Err(e) => {
            tracing::error!("{e}");
            e.exit_code()
        }
    };
    ExitCode::from(code as u8)
}

fn init_logging(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(progress::log_writer);
    match format {
        LogFormat::Text => builder.with_target(false).init(),
        LogFormat::Json => builder.json().init(),
    }
}

/// Run the command. Return the number of failed items.
fn run(cli: &Cli) -> Result<usize, Error> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let failures = match &cli.command {
        Command::Devices => {
            let list = wpd::list_devices()?;
            cmd::devices(&list, &mut out)?;
            0
        }
        Command::Ls {
            long,
            recursive,
            json,
            sort_size,
            sort_time,
            reverse,
            unsorted,
            paths,
        } => {
            let fs = wpd::open(cli.device)?;
            let opts = LsOptions {
                long: *long,
                recursive: *recursive,
                json: *json,
                sort: if *unsorted {
                    SortKey::None
                } else if *sort_size {
                    SortKey::Size
                } else if *sort_time {
                    SortKey::Time
                } else {
                    SortKey::Name
                },
                reverse: *reverse,
            };
            cmd::ls::run(fs.as_ref(), paths, opts, &mut out)?
        }
        Command::Tree {
            level,
            json,
            dirs_first,
            path,
        } => {
            let fs = wpd::open(cli.device)?;
            let path = path.clone().unwrap_or_else(DevicePath::root);
            cmd::tree::run(fs.as_ref(), &path, *level, *json, *dirs_first, &mut out)?
        }
        Command::Cp {
            recursive,
            no_clobber,
            force,
            preserve,
            archive,
            dry_run,
            verify,
            sources,
            dest,
        } => {
            let fs = wpd::open(cli.device)?;
            let progress = if std::io::stderr().is_terminal() && cli.log_format == LogFormat::Text {
                ProgressMode::Bar
            } else {
                ProgressMode::Events
            };
            let opts = CpOptions {
                recursive: *recursive || *archive,
                preserve: *preserve || *archive,
                dry_run: *dry_run,
                on_exists: if *force {
                    OnExists::Overwrite
                } else if *no_clobber {
                    OnExists::SkipQuiet
                } else {
                    OnExists::SkipWarn
                },
                progress,
                local_hash: *verify == Some(VerifyMode::LocalHash),
                retries: cli.retries,
                sleep: std::thread::sleep,
                diagnostic: cli.diagnostic,
            };
            cmd::cp::run(fs.as_ref(), sources, dest, opts, &mut out)?
        }
        Command::Verify { hash, dest } => {
            cmd::verify::run(dest, VerifyOptions { hash: *hash }, &mut out)?
        }
    };
    out.flush().map_err(error::stdout_err)?;
    Ok(failures)
}
