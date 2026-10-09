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
