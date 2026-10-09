//! Device paths such as `/Internal Storage/DCIM/202601_a`.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// An absolute path on the device. The root is `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePath {
    components: Vec<String>,
    trailing_slash: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DevicePathError {
    #[error("device path must start with '/': {0:?}")]
    NotAbsolute(String),
    #[error("device path has an empty component: {0:?}")]
    EmptyComponent(String),
    #[error("device path component '.' or '..' is not supported: {0:?}")]
    DotComponent(String),
}

impl DevicePath {
    pub fn root() -> Self {
        Self {
            components: Vec::new(),
            trailing_slash: false,
        }
    }

    pub fn parse(s: &str) -> Result<Self, DevicePathError> {
        let rest = s
            .strip_prefix('/')
            .ok_or_else(|| DevicePathError::NotAbsolute(s.to_owned()))?;
        if rest.is_empty() {
            return Ok(Self::root());
        }
        let (body, trailing_slash) = match rest.strip_suffix('/') {
            Some(b) => (b, true),
            None => (rest, false),
        };
        let mut components = Vec::new();
        for c in body.split('/') {
            match c {
                "" => return Err(DevicePathError::EmptyComponent(s.to_owned())),
                "." | ".." => return Err(DevicePathError::DotComponent(s.to_owned())),
                _ => components.push(c.to_owned()),
            }
        }
        Ok(Self {
            components,
            trailing_slash,
        })
    }

    pub fn components(&self) -> &[String] {
        &self.components
    }

    pub fn is_root(&self) -> bool {
        self.components.is_empty()
    }

    /// True if the path was given with a trailing `/`. Phase 1 `cp -r` uses
    /// this like rsync: `SRC` copies the folder, `SRC/` copies its contents.
    pub fn trailing_slash(&self) -> bool {
        self.trailing_slash
    }

    /// The path without the trailing slash, for example `/DCIM/202601_a`.
    pub fn normalized(&self) -> String {
        if self.is_root() {
            "/".to_owned()
        } else {
            format!("/{}", self.components.join("/"))
        }
    }
}

impl fmt::Display for DevicePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.normalized())?;
        if self.trailing_slash {
            f.write_str("/")?;
        }
        Ok(())
    }
}

impl FromStr for DevicePath {
    type Err = DevicePathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root() {
        let p = DevicePath::parse("/").unwrap();
        assert!(p.is_root());
        assert!(!p.trailing_slash());
        assert_eq!(p.to_string(), "/");
    }

    #[test]
    fn components_keep_spaces_and_unicode() {
        let p = DevicePath::parse("/Internal Storage/DCIM/202601_a/照片.HEIC").unwrap();
        assert_eq!(
            p.components(),
            ["Internal Storage", "DCIM", "202601_a", "照片.HEIC"]
        );
        assert!(!p.trailing_slash());
    }

    #[test]
    fn trailing_slash_is_kept() {
        let p = DevicePath::parse("/Internal Storage/DCIM/").unwrap();
        assert_eq!(p.components(), ["Internal Storage", "DCIM"]);
        assert!(p.trailing_slash());
        assert_eq!(p.normalized(), "/Internal Storage/DCIM");
        assert_eq!(p.to_string(), "/Internal Storage/DCIM/");
    }

    #[test]
    fn leading_slash_is_required() {
        assert_eq!(
            DevicePath::parse("DCIM/202601_a"),
            Err(DevicePathError::NotAbsolute("DCIM/202601_a".into()))
        );
        assert!(DevicePath::parse("").is_err());
    }

    #[test]
    fn empty_components_are_rejected() {
        for bad in ["//", "/a//b", "//a", "/a//"] {
            assert!(
                matches!(
                    DevicePath::parse(bad),
                    Err(DevicePathError::EmptyComponent(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn dot_components_are_rejected() {
        assert!(matches!(
            DevicePath::parse("/a/../b"),
            Err(DevicePathError::DotComponent(_))
        ));
        assert!(matches!(
            DevicePath::parse("/./a"),
            Err(DevicePathError::DotComponent(_))
        ));
    }
}
