//! `cp`: copy device files and folders to a local path.
//!
//! The copy itself is `backup::engine`. This command adds the terminal
//! progress display and the stdout lines.

use std::path::Path;
#[cfg(test)]
use std::{
    io::{ErrorKind, Write},
    time::Duration,
};

use crate::backup::engine::{self, CopyOptions, ProgressSink};
pub use crate::backup::engine::{DEFAULT_RETRIES, OnExists, backoff};
use crate::device_fs::{CachedFs, DeviceFs};
use crate::devpath::DevicePath;
#[cfg(test)]
use crate::error::Error;
use crate::error::Result;
#[cfg(test)]
use crate::model::Node;
use crate::progress::{Progress, ProgressMode};

#[derive(Debug, Clone, Copy)]
pub struct CpOptions {
    pub recursive: bool,
    /// `-p`: set the local file times from the device after the commit.
    pub preserve: bool,
    pub dry_run: bool,
    pub on_exists: OnExists,
    pub progress: ProgressMode,
    /// `--verify local-hash`: hash new copies while they stream, and check
    /// the stored hash before a skip.
    pub local_hash: bool,
    /// Additional attempts after a transient failure.
    pub retries: u32,
    /// Waits between attempts. Tests replace it.
    pub sleep: fn(std::time::Duration),
    /// `--diagnostic`: log the raw device ID.
    pub diagnostic: bool,
    /// `--manifest`: read and write the manifest of the copy root.
    pub manifest: bool,
}

impl Default for CpOptions {
    fn default() -> Self {
        let d = CopyOptions::default();
        Self {
            recursive: d.recursive,
            preserve: d.preserve,
            dry_run: d.dry_run,
            on_exists: d.on_exists,
            progress: ProgressMode::Off,
            local_hash: d.local_hash,
            retries: d.retries,
            sleep: d.sleep,
            diagnostic: d.diagnostic,
            manifest: d.manifest,
        }
    }
}

impl CpOptions {
    fn engine(&self) -> CopyOptions {
        CopyOptions {
            recursive: self.recursive,
            preserve: self.preserve,
            dry_run: self.dry_run,
            on_exists: self.on_exists,
            local_hash: self.local_hash,
            retries: self.retries,
            sleep: self.sleep,
            diagnostic: self.diagnostic,
            manifest: self.manifest,
        }
    }
}

/// Copy `sources` to `dest`. Return the number of failed items.
///
/// A device error that stops all later items (device gone, access denied)
/// ends the run after the summary and is returned as `Err`.
pub fn run(
    fs: &dyn DeviceFs,
    sources: &[DevicePath],
    dest: &Path,
    opts: CpOptions,
    out: &mut dyn std::io::Write,
) -> Result<usize> {
    let mode = if opts.dry_run {
        ProgressMode::Off
    } else {
        opts.progress
    };
    let mut sink = TerminalSink {
        mode,
        progress: None,
    };
    // Each source path resolves from the root: list each folder once.
    let fs = CachedFs::new(fs);
    engine::run(&fs, sources, dest, opts.engine(), &mut sink, out).map(|s| s.failed)
}

/// `Progress` behind the engine's sink. The bars appear when the run begins.
struct TerminalSink {
    mode: ProgressMode,
    progress: Option<Progress>,
}

impl TerminalSink {
    fn with(&mut self, f: impl FnOnce(&mut Progress)) {
        if let Some(p) = self.progress.as_mut() {
            f(p);
        }
    }
}

impl ProgressSink for TerminalSink {
    fn begin(&mut self) {
        self.progress = Some(Progress::new(self.mode));
    }

    fn found(&mut self, size: Option<u64>) {
        self.with(|p| p.found(size));
    }

    fn settled(&mut self, size: Option<u64>) {
        self.with(|p| p.settled(size));
    }

    fn file_start(&mut self, source: &str, size: Option<u64>) {
        self.with(|p| p.file_start(source, size));
    }

    fn bytes(&mut self, n: u64) {
        self.with(|p| p.bytes(n));
    }

    fn restart_file(&mut self) {
        self.with(Progress::restart_file);
    }

    fn file_end(&mut self, ok: bool) {
        self.with(|p| p.file_end(ok));
    }

    fn scan_done(&mut self) {
        self.with(Progress::scan_done);
    }

    fn finish(&mut self) {
        self.with(Progress::finish);
    }

    fn suspend(&mut self, f: &mut dyn FnMut()) {
        match &self.progress {
            Some(p) => p.suspend(f),
            None => f(),
        }
    }
}

#[cfg(test)]
mod tests;
