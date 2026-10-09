//! The read-only GUI config: `win-iphone-dcim.toml` next to the GUI exe,
//! then environment variables. The app never writes the file. The CLI does
//! not read it.
//!
//! ```toml
//! cache_dir = "cache"          # relative to the exe folder
//! cache_max = "512MiB"         # or an integer of bytes
//! clear_cache_on_exit = true
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The cache base folder. `None` means the default `cache` folder next
    /// to the exe.
    pub cache_dir: Option<PathBuf>,
    /// The soft size limit of the cache, in bytes.
    pub cache_max: u64,
    /// Delete the cache at start and at exit.
    pub clear_cache_on_exit: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cache_dir: None,
            cache_max: DEFAULT_CACHE_MAX,
            clear_cache_on_exit: true,
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
    }
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
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn resolve(file: Option<&str>, env: &dyn Fn(&str) -> Option<String>) -> (Config, Vec<String>) {
        let mut warnings = Vec::new();
        let config = Config::resolve(file, env, Some(Path::new("/app")), &mut warnings);
        (config, warnings)
    }

    #[test]
    fn missing_file_gives_the_defaults() {
        let (config, warnings) = resolve(None, &no_env);
        assert_eq!(config, Config::default());
        assert_eq!(config.cache_max, 512 * 1024 * 1024);
        assert!(config.clear_cache_on_exit);
        assert!(warnings.is_empty());
    }

    #[test]
    fn load_without_a_file_gives_the_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config::load(Some(tmp.path()));
        // Only the environment of the test run can change the result.
        if std::env::var_os(ENV_CACHE_DIR).is_none() {
            assert_eq!(config.cache_dir, None);
        }
    }

    #[test]
    fn file_values_are_read_and_relative_dirs_follow_the_exe() {
        let text = "cache_dir = \"c\"\ncache_max = \"2GiB\"\nclear_cache_on_exit = false\n";
        let (config, warnings) = resolve(Some(text), &no_env);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(config.cache_dir, Some(PathBuf::from("/app/c")));
        assert_eq!(config.cache_max, 2 << 30);
        assert!(!config.clear_cache_on_exit);

        let (config, _) = resolve(
            Some("cache_dir = \"/abs/cache\"\ncache_max = 1000"),
            &no_env,
        );
        assert_eq!(config.cache_dir, Some(PathBuf::from("/abs/cache")));
        assert_eq!(config.cache_max, 1000);
    }

    #[test]
    fn bad_values_are_ignored_with_a_warning() {
        let text = "cache_dir = 5\ncache_max = \"lots\"\nclear_cache_on_exit = \"no\"\n";
        let (config, warnings) = resolve(Some(text), &no_env);
        assert_eq!(config, Config::default());
        assert_eq!(warnings.len(), 3, "{warnings:?}");

        let (config, warnings) = resolve(Some("cache_max = -1\ncache_dir = \"d\""), &no_env);
        assert_eq!(config.cache_max, DEFAULT_CACHE_MAX);
        assert_eq!(config.cache_dir, Some(PathBuf::from("/app/d")));
        assert_eq!(warnings.len(), 1);

        let (config, warnings) = resolve(Some("this is not toml"), &no_env);
        assert_eq!(config, Config::default());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn environment_overrides_the_file() {
        let text = "cache_dir = \"c\"\ncache_max = \"2GiB\"\nclear_cache_on_exit = false\n";
        let env = |name: &str| match name {
            ENV_CACHE_DIR => Some("other".to_owned()),
            ENV_CACHE_MAX => Some("100 MiB".to_owned()),
            ENV_CLEAR_CACHE_ON_EXIT => Some("1".to_owned()),
            _ => None,
        };
        let (config, warnings) = resolve(Some(text), &env);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(config.cache_dir, Some(PathBuf::from("/app/other")));
        assert_eq!(config.cache_max, 100 << 20);
        assert!(config.clear_cache_on_exit);

        let bad = |name: &str| match name {
            ENV_CACHE_MAX => Some("x".to_owned()),
            ENV_CLEAR_CACHE_ON_EXIT => Some("maybe".to_owned()),
            ENV_CACHE_DIR => Some("  ".to_owned()),
            _ => None,
        };
        let (config, warnings) = resolve(Some(text), &bad);
        assert_eq!(config.cache_max, 2 << 30);
        assert!(!config.clear_cache_on_exit);
        assert_eq!(config.cache_dir, Some(PathBuf::from("/app/c")));
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn sizes_parse_with_and_without_units() {
        assert_eq!(parse_size("1048576"), Some(1 << 20));
        assert_eq!(parse_size("512MiB"), Some(512 << 20));
        assert_eq!(parse_size("2GiB"), Some(2 << 30));
        assert_eq!(parse_size("2 gib"), Some(2 << 30));
        assert_eq!(parse_size("64K"), Some(64 << 10));
        assert_eq!(parse_size("1TiB"), Some(1 << 40));
        assert_eq!(parse_size("5MB"), Some(5_000_000));
        assert_eq!(parse_size("0"), Some(0));
        assert_eq!(parse_size(" 10 B "), Some(10));
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("MiB"), None);
        assert_eq!(parse_size("1.5GiB"), None);
        assert_eq!(parse_size("-1"), None);
        assert_eq!(parse_size("3 parsecs"), None);
        assert_eq!(parse_size("99999999999TiB"), None);
    }
}
