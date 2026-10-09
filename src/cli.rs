//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::devpath::DevicePath;

#[derive(Debug, Parser)]
#[command(
    name = "win-iphone-dcim",
    version,
    about = "Read-only iPhone DCIM access over Windows Portable Devices (WPD)",
    long_about = "win-iphone-dcim reads photos and videos from an iPhone over WPD. \
                  It never writes to or deletes from the device.\n\n\
                  Device paths start at the device root '/', for example \
                  '/Internal Storage/DCIM/202601_a'. Each component matches the \
                  original file name first, then the object name.\n\n\
                  Results go to stdout. Logs go to stderr."
)]
pub struct Cli {
    /// Device index from `win-iphone-dcim devices`. Automatic if exactly one device is connected.
    #[arg(short = 'd', long, global = true, value_name = "INDEX")]
    pub device: Option<usize>,

    /// Log format on stderr.
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Text)]
    pub log_format: LogFormat,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VerifyMode {
    Size,
    LocalHash,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the WPD devices that Windows can see.
    Devices,

    /// List folder contents, like Unix ls.
    Ls {
        /// Long format: type, size, modification date, name, object ID.
        #[arg(short = 'l')]
        long: bool,

        /// List subfolders recursively.
        #[arg(short = 'R')]
        recursive: bool,

        /// Print one JSON object per line (JSONL).
        #[arg(long)]
        json: bool,

        /// Sort by size, largest first. Entries without a size go last.
        #[arg(short = 'S', conflicts_with_all = ["sort_time", "unsorted"])]
        sort_size: bool,

        /// Sort by modification time, newest first. Entries without a time go last.
        #[arg(short = 't', conflicts_with = "unsorted")]
        sort_time: bool,

        /// Reverse the sort order.
        #[arg(short = 'r', conflicts_with = "unsorted")]
        reverse: bool,

        /// Do not sort. Keep the device order.
        #[arg(short = 'U')]
        unsorted: bool,

        /// Device paths. Default: `/`.
        #[arg(value_name = "PATH")]
        paths: Vec<DevicePath>,
    },

    /// Show folders and files as a tree, like the Unix tree command.
    Tree {
        /// Descend at most DEPTH levels below PATH.
        #[arg(short = 'L', value_name = "DEPTH")]
        level: Option<usize>,

        /// Print one JSON object per line (JSONL).
        #[arg(long)]
        json: bool,

        /// List folders before files. Each group stays sorted by name.
        #[arg(long)]
        dirs_first: bool,

        /// Device path. Default: `/`.
        #[arg(value_name = "PATH")]
        path: Option<DevicePath>,
    },

    /// Copy device files and folders to a local path, like Unix cp and rsync.
    ///
    /// If DEST is an existing folder, each SRC goes into it with its original
    /// name. `SRC/` with a trailing slash copies the folder contents only.
    /// If DEST does not exist and there is one SRC, DEST is the new file or
    /// folder. With two or more SRC, DEST must be an existing folder. An
    /// existing local file is skipped with a warning unless -f is given.
    Cp {
        /// Copy folders recursively. A folder SRC needs this flag.
        #[arg(short = 'r')]
        recursive: bool,

        /// Skip every existing target file silently.
        #[arg(short = 'n', long = "no-clobber", conflicts_with = "force")]
        no_clobber: bool,

        /// Replace an existing target file. The new data goes to a `.part`
        /// file first and replaces the target only when it is complete.
        #[arg(short = 'f', long)]
        force: bool,

        /// Preserve the device modified and created times.
        #[arg(short = 'p')]
        preserve: bool,

        /// Archive mode: same as -r -p.
        #[arg(short = 'a')]
        archive: bool,

        /// Print the copy plan only. Write nothing.
        #[arg(long)]
        dry_run: bool,

        /// Verification mode. Only `size` is available until Phase 2.
        #[arg(long, value_enum, value_name = "MODE")]
        verify: Option<VerifyMode>,

        /// Device paths. A trailing `/` copies the folder contents only.
        #[arg(value_name = "SRC", required = true, num_args = 1..)]
        sources: Vec<DevicePath>,

        /// Local destination file or folder.
        #[arg(value_name = "DEST", required = true)]
        dest: PathBuf,
    },
}

#[cfg(test)]
mod tests {
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
    fn rejects_unknown_log_format() {
        assert!(parse(&["--log-format", "xml", "devices"]).is_err());
    }
}
