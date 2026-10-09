use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/tags");
    println!("cargo:rerun-if-changed=.git/packed-refs");

    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let described = Command::new("git")
        .args(["-C", &dir, "-c", "safe.directory=*"])
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let version = match described {
        Some(s) => s.strip_prefix('v').unwrap_or(&s).to_string(),
        None => std::env::var("CARGO_PKG_VERSION").unwrap_or_default(),
    };
    println!("cargo:rustc-env=WIN_IPHONE_DCIM_VERSION={version}");
}
