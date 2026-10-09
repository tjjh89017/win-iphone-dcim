//! Command-line interface definition.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use crate::devpath::DevicePath;
use crate::supervisor::DEFAULT_TIMEOUT;

#[derive(Debug, Parser)]
#[command(
    name = "win-iphone-dcim",
    version = crate::VERSION,
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

    /// Additional attempts for a file after a transient failure (device
    /// busy or gone, I/O timeout, network write error). Backoff: 1s, 3s, 10s.
    #[arg(long, global = true, value_name = "N", default_value_t = 3)]
    pub retries: u32,

    /// Diagnostic mode: also log the raw device ID. By default logs show
    /// only a short hash of it.
    #[arg(long, global = true)]
    pub diagnostic: bool,

    /// Kill and restart the device worker after this long without activity
    /// during a device call. Plain seconds, or a number with the unit `s`,
    /// `m` or `h`, for example `90`, `90s` or `2m`.
    #[arg(long, global = true, value_name = "DURATION", default_value_t = Timeout(DEFAULT_TIMEOUT))]
    pub timeout: Timeout,

    /// Run the WPD calls in this process instead of a worker process. A
    /// hung device call then cannot be stopped. For debugging only.
    #[arg(long, global = true)]
    pub no_isolate: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// Value of `--timeout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeout(pub Duration);

impl fmt::Display for Timeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}s", self.0.as_secs())
    }
}

impl FromStr for Timeout {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        parse_timeout(s).map(Timeout)
    }
}

/// Parse `--timeout`: whole seconds, optionally with the unit `s`, `m` or `h`.
fn parse_timeout(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (digits, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => s.split_at(i),
        None => (s, "s"),
    };
    let factor = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return Err(format!("unknown unit {unit:?}; use s, m or h")),
    };
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("{s:?} is not a duration like 120, 90s or 2m"))?;
    match n.checked_mul(factor) {
        Some(0) => Err("the timeout must be at least 1 s".into()),
        Some(secs) => Ok(Duration::from_secs(secs)),
        None => Err(format!("{s:?} is too large")),
    }
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
    /// existing local file of the same size is skipped. Another existing
    /// local file is skipped with a warning unless -f is given.
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

        /// Verification mode. `size` compares sizes with the device.
        /// `local-hash` also stores a BLAKE3 hash of each new copy in the
        /// manifest and checks it before a skip. `local-hash` requires
        /// --manifest.
        #[arg(
            long,
            value_enum,
            value_name = "MODE",
            requires_if("local-hash", "manifest")
        )]
        verify: Option<VerifyMode>,

        /// Write DEST/.win-iphone-dcim/manifest.jsonl and use it for the
        /// incremental rules and for `verify`. Off by default.
        #[arg(long)]
        manifest: bool,

        /// Device paths. A trailing `/` copies the folder contents only.
        #[arg(value_name = "SRC", required = true, num_args = 1..)]
        sources: Vec<DevicePath>,

        /// Local destination file or folder.
        #[arg(value_name = "DEST", required = true)]
        dest: PathBuf,
    },

    /// Check the files in DEST against DEST/.win-iphone-dcim/manifest.jsonl.
    ///
    /// Each record is checked for a file with the recorded size. Files
    /// without a record are listed as unrecorded. Exit code 0 if all files
    /// are ok, 1 otherwise.
    Verify {
        /// Also recompute the BLAKE3 hash where the manifest has one.
        #[arg(long)]
        hash: bool,

        /// The copy root that holds `.win-iphone-dcim/manifest.jsonl`.
        #[arg(value_name = "DEST")]
        dest: PathBuf,
    },

    /// Internal: the worker process that owns the WPD COM objects. The
    /// parent starts it. It speaks JSONL on stdout.
    #[command(hide = true)]
    Worker {
        /// Handle number of the inherited write end of the data pipe.
        #[arg(long)]
        data_pipe: u64,
    },
}

#[cfg(test)]
mod tests;
