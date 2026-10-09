//! The read-only GUI config: `win-iphone-dcim.toml` next to the GUI exe,
//! then environment variables. The app never writes the file. The CLI does
//! not read it.
//!
//! ```toml
//! cache_dir = "win-iphone-dcim-cache"  # relative to the exe folder
//! cache_max = "512MiB"         # or an integer of bytes
//! clear_cache_on_exit = true
//! manifest = false             # write DEST/.win-iphone-dcim/manifest.jsonl
//! log_file = "gui.log"         # relative to the exe folder
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The config file name. It is read from the folder of the GUI exe.
pub const CONFIG_FILE: &str = "win-iphone-dcim.toml";
/// The default soft limit of the open-file cache.
pub const DEFAULT_CACHE_MAX: u64 = 512 * 1024 * 1024;

pub const ENV_CACHE_DIR: &str = "WIN_IPHONE_DCIM_CACHE_DIR";
pub const ENV_CACHE_MAX: &str = "WIN_IPHONE_DCIM_CACHE_MAX";
pub const ENV_CLEAR_CACHE_ON_EXIT: &str = "WIN_IPHONE_DCIM_CLEAR_CACHE_ON_EXIT";
pub const ENV_MANIFEST: &str = "WIN_IPHONE_DCIM_MANIFEST";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The cache base folder. `None` means the default
    /// `win-iphone-dcim-cache` folder next to the exe.
    pub cache_dir: Option<PathBuf>,
    /// The soft size limit of the cache, in bytes.
    pub cache_max: u64,
    /// Delete the cache at start and at exit.
    pub clear_cache_on_exit: bool,
    /// Copies write the manifest of the copy root, like `cp --manifest`.
    pub manifest: bool,
    /// The GUI and its worker append their logs to this file. `None` means
    /// no log file. `WIN_IPHONE_DCIM_LOG_FILE` and `--log-file` override it.
    pub log_file: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cache_dir: None,
            cache_max: DEFAULT_CACHE_MAX,
            clear_cache_on_exit: true,
            manifest: false,
            log_file: None,
        }
    }
}

/// The file keys. Each value is checked on its own, so one bad value does
/// not drop the others.
#[derive(Debug, Default, Deserialize)]
struct RawFile {
    cache_dir: Option<toml::Value>,
    cache_max: Option<toml::Value>,
    clear_cache_on_exit: Option<toml::Value>,
    manifest: Option<toml::Value>,
    log_file: Option<toml::Value>,
}

/// The folder of the running exe.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

impl Config {
    /// Read the config file in `exe_dir` and the environment. Problems are
    /// logged as warnings and the value keeps its default.
    pub fn load(exe_dir: Option<&Path>) -> Self {
        let mut warnings = Vec::new();
        let text = exe_dir.and_then(|dir| {
            let path = dir.join(CONFIG_FILE);
            match std::fs::read_to_string(&path) {
                Ok(text) => Some(text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    warnings.push(format!("{}: {e}", path.display()));
                    None
                }
            }
        });
        let env = |name: &str| std::env::var(name).ok();
        let config = Self::resolve(text.as_deref(), &env, exe_dir, &mut warnings);
        for w in &warnings {
            tracing::warn!("config: {w}");
        }
        config
    }

    /// The config from the file text and the environment. The environment
    /// overrides the file. Relative paths resolve against `exe_dir`.
    pub fn resolve(
        file: Option<&str>,
        env: &dyn Fn(&str) -> Option<String>,
        exe_dir: Option<&Path>,
        warnings: &mut Vec<String>,
    ) -> Self {
        let mut config = Self::default();
        if let Some(text) = file {
            match toml::from_str::<RawFile>(text) {
                Ok(raw) => config.apply_file(raw, exe_dir, warnings),
                Err(e) => warnings.push(format!("{CONFIG_FILE}: {e}")),
            }
        }
        let env = |name: &str| env(name).filter(|v| !v.trim().is_empty());
        if let Some(v) = env(ENV_CACHE_DIR) {
            config.cache_dir = Some(resolve_dir(Path::new(v.trim()), exe_dir));
        }
        if let Some(v) = env(ENV_CACHE_MAX) {
            match parse_size(&v) {
                Some(n) => config.cache_max = n,
                None => warnings.push(format!("{ENV_CACHE_MAX}: bad size {v:?}")),
            }
        }
        if let Some(v) = env(ENV_CLEAR_CACHE_ON_EXIT) {
            match parse_bool(&v) {
                Some(b) => config.clear_cache_on_exit = b,
                None => warnings.push(format!("{ENV_CLEAR_CACHE_ON_EXIT}: bad value {v:?}")),
            }
        }
        if let Some(v) = env(ENV_MANIFEST) {
            match parse_bool(&v) {
                Some(b) => config.manifest = b,
                None => warnings.push(format!("{ENV_MANIFEST}: bad value {v:?}")),
            }
        }
        config
    }

    fn apply_file(&mut self, raw: RawFile, exe_dir: Option<&Path>, warnings: &mut Vec<String>) {
        match raw.cache_dir {
            Some(toml::Value::String(s)) if !s.trim().is_empty() => {
                self.cache_dir = Some(resolve_dir(Path::new(s.trim()), exe_dir));
            }
            Some(v) => warnings.push(format!("cache_dir: not a folder name: {v}")),
            None => {}
        }
        match raw.cache_max {
            Some(toml::Value::Integer(n)) if n >= 0 => self.cache_max = n as u64,
            Some(toml::Value::String(s)) => match parse_size(&s) {
                Some(n) => self.cache_max = n,
                None => warnings.push(format!("cache_max: bad size {s:?}")),
            },
            Some(v) => warnings.push(format!("cache_max: bad size {v}")),
            None => {}
        }
        match raw.clear_cache_on_exit {
            Some(toml::Value::Boolean(b)) => self.clear_cache_on_exit = b,
            Some(v) => warnings.push(format!("clear_cache_on_exit: not true or false: {v}")),
            None => {}
        }
        match raw.manifest {
            Some(toml::Value::Boolean(b)) => self.manifest = b,
            Some(v) => warnings.push(format!("manifest: not true or false: {v}")),
            None => {}
        }
        match raw.log_file {
            Some(toml::Value::String(s)) if !s.trim().is_empty() => {
                self.log_file = Some(resolve_dir(Path::new(s.trim()), exe_dir));
            }
            Some(v) => warnings.push(format!("log_file: not a file name: {v}")),
            None => {}
        }
    }
}

/// The settings of the Preferences window. They last for the session only;
/// `to_toml` gives the config file text that keeps them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preferences {
    /// Copies write the manifest of the copy root.
    pub manifest: bool,
    /// Delete the cache at start and at exit.
    pub clear_cache_on_exit: bool,
    /// Copies replace existing files (`--force`). It has no config key.
    pub force: bool,
    /// The soft size limit of the cache, in bytes.
    pub cache_max: u64,
    /// The cache base folder in use, if any.
    pub cache_dir: Option<PathBuf>,
    /// The `log_file` key of the config file, kept so `to_toml` keeps it.
    pub log_file: Option<PathBuf>,
}

impl Preferences {
    pub fn new(config: &Config, cache_dir: Option<PathBuf>) -> Self {
        Self {
            manifest: config.manifest,
            clear_cache_on_exit: config.clear_cache_on_exit,
            force: false,
            cache_max: config.cache_max,
            cache_dir,
            log_file: config.log_file.clone(),
        }
    }

    /// The config file keys and the current values.
    pub fn to_toml(&self) -> String {
        let mut text = format!(
            "manifest = {}\nclear_cache_on_exit = {}\ncache_max = {}\n",
            self.manifest,
            self.clear_cache_on_exit,
            format_size(self.cache_max)
        );
        if let Some(dir) = &self.cache_dir {
            let value = toml::Value::String(dir.to_string_lossy().into_owned());
            text.push_str(&format!("cache_dir = {value}\n"));
        }
        if let Some(file) = &self.log_file {
            let value = toml::Value::String(file.to_string_lossy().into_owned());
            text.push_str(&format!("log_file = {value}\n"));
        }
        text
    }
}

/// A size as a config value: a quoted size with the largest binary unit
/// that divides it, or an integer of bytes.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [(u64, &str); 4] = [
        (1 << 40, "TiB"),
        (1 << 30, "GiB"),
        (1 << 20, "MiB"),
        (1 << 10, "KiB"),
    ];
    UNITS
        .iter()
        .find(|(factor, _)| bytes > 0 && bytes.is_multiple_of(*factor))
        .map(|(factor, unit)| format!("\"{}{unit}\"", bytes / factor))
        .unwrap_or_else(|| bytes.to_string())
}

fn resolve_dir(dir: &Path, exe_dir: Option<&Path>) -> PathBuf {
    match exe_dir {
        Some(base) if dir.is_relative() => base.join(dir),
        _ => dir.to_path_buf(),
    }
}

/// Parse a size: bytes (`1048576`), or a number with a unit. `KiB`, `MiB`,
/// `GiB`, `TiB` and `K`, `M`, `G`, `T` are powers of 1024. `KB`, `MB`,
/// `GB`, `TB` are powers of 1000. Units ignore case and may follow a space.
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (num, unit) = text.split_at(split);
    let num: u64 = num.parse().ok()?;
    let factor: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kib" => 1 << 10,
        "m" | "mib" => 1 << 20,
        "g" | "gib" => 1 << 30,
        "t" | "tib" => 1 << 40,
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "tb" => 1_000_000_000_000,
        _ => return None,
    };
    num.checked_mul(factor)
}

fn parse_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
