//! Copy planner: turns `cp` sources and DEST into copy items, lazily.
//!
//! The planner applies the Unix `cp` and `rsync` rules.
//! It lists one device folder only when the caller reaches it, so the whole
//! tree is never held in memory.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::paths::case_collisions;
use crate::device_fs::{DeviceFs, resolve};
use crate::devpath::DevicePath;
use crate::error::{Error, Result};
use crate::model::{Node, join_device_path};
use crate::paths::device_name_to_os;

#[derive(Debug, Clone, Copy, Default)]
pub struct PlanOptions {
    pub recursive: bool,
}

/// One device file and its local target path.
#[derive(Debug, Clone)]
pub struct CopyItem {
    /// Device path, for example `/Internal Storage/DCIM/202601_a/IMG_0001.HEIC`.
    pub source: String,
    pub node: Node,
    pub target: PathBuf,
}

#[derive(Debug)]
pub enum PlanItem {
    /// A local folder to create, or to reuse if it exists. Its contents
    /// follow. Call `Planner::skip_dir` to drop them.
    Dir {
        source: String,
        target: PathBuf,
    },
    Copy(CopyItem),
    /// A source that cannot be copied. Nothing is written for it.
    Error {
        source: String,
        error: Error,
    },
}

struct Child {
    node: Node,
    source: String,
    name: Result<OsString>,
}

struct Frame {
    source: String,
    node: Node,
    target: PathBuf,
    /// `None` until the folder is listed.
    children: Option<std::vec::IntoIter<Child>>,
}

pub struct Planner<'a> {
    fs: &'a dyn DeviceFs,
    dest: PathBuf,
    dest_is_dir: bool,
    opts: PlanOptions,
    sources: std::slice::Iter<'a, DevicePath>,
    stack: Vec<Frame>,
}

impl<'a> Planner<'a> {
    /// `dest` must already be absolute. With two or more sources, `dest`
    /// must be an existing folder.
    pub fn new(
        fs: &'a dyn DeviceFs,
        sources: &'a [DevicePath],
        dest: &Path,
        opts: PlanOptions,
    ) -> Result<Self> {
        let dest_is_dir = dest.is_dir();
        if sources.len() > 1 && !dest_is_dir {
            return Err(Error::DestNotFolder(dest.to_path_buf()));
        }
        Ok(Self {
            fs,
            dest: dest.to_path_buf(),
            dest_is_dir,
            opts,
            sources: sources.iter(),
            stack: Vec::new(),
        })
    }

    /// Drop the folder of the last `PlanItem::Dir`, for example when the
    /// local folder cannot be created.
    pub fn skip_dir(&mut self) {
        if self.stack.last().is_some_and(|f| f.children.is_none()) {
            self.stack.pop();
        }
    }

    fn plan_source(&mut self, src: &DevicePath) -> PlanItem {
        let source = src.normalized();
        let node = match resolve(self.fs, src) {
            Ok(n) => n,
            Err(error) => return PlanItem::Error { source, error },
        };
        if !node.is_folder {
            let target = if self.dest_is_dir {
                match local_name(&node) {
                    Ok(name) => self.dest.join(name),
                    Err(error) => return PlanItem::Error { source, error },
                }
            } else {
                self.dest.clone()
            };
            return PlanItem::Copy(CopyItem {
                source,
                node,
                target,
            });
        }
        if !self.opts.recursive {
            return PlanItem::Error {
                source,
                error: Error::FolderNeedsRecursive(src.to_string()),
            };
        }
        let target = if self.dest_is_dir {
            // rsync: `SRC/` copies the contents, `SRC` copies the folder.
            if src.trailing_slash() || src.is_root() {
                self.dest.clone()
            } else {
                match local_name(&node) {
                    Ok(name) => self.dest.join(name),
                    Err(error) => return PlanItem::Error { source, error },
                }
            }
        } else if self.dest.symlink_metadata().is_ok() {
            return PlanItem::Error {
                source,
                error: Error::NotAFolderLocal(self.dest.clone()),
            };
        } else {
            // cp -r SRC NEW: NEW gets the contents of SRC.
            self.dest.clone()
        };
        self.push_dir(source, node, target)
    }

    fn push_dir(&mut self, source: String, node: Node, target: PathBuf) -> PlanItem {
        self.stack.push(Frame {
            source: source.clone(),
            node,
            target: target.clone(),
            children: None,
        });
        PlanItem::Dir { source, target }
    }
}

impl Iterator for Planner<'_> {
    type Item = PlanItem;

    fn next(&mut self) -> Option<PlanItem> {
        let fs = self.fs;
        loop {
            let Some(frame) = self.stack.last_mut() else {
                let src = self.sources.next()?;
                return Some(self.plan_source(src));
            };
            if frame.children.is_none() {
                match fs.list(&frame.node) {
                    Ok(list) => frame.children = Some(children(&frame.source, list).into_iter()),
                    Err(error) => {
                        let source = frame.source.clone();
                        self.stack.pop();
                        return Some(PlanItem::Error { source, error });
                    }
                }
            }
            let Some(child) = frame.children.as_mut().and_then(Iterator::next) else {
                self.stack.pop();
                continue;
            };
            let target = match child.name {
                Ok(name) => frame.target.join(name),
                Err(error) => {
                    return Some(PlanItem::Error {
                        source: child.source,
                        error,
                    });
                }
            };
            if child.node.is_folder {
                return Some(self.push_dir(child.source, child.node, target));
            }
            return Some(PlanItem::Copy(CopyItem {
                source: child.source,
                node: child.node,
                target,
            }));
        }
    }
}

/// The local name of a device object, from its raw UTF-16 name.
fn local_name(node: &Node) -> Result<OsString> {
    let units = node
        .raw_file_name
        .as_deref()
        .ok_or_else(|| Error::UnsafeFileName {
            name: node.display_name(),
            reason: "no name",
        })?;
    device_name_to_os(units)
}

/// Check the names of one folder listing. Entries whose names differ only
/// by case get a collision error, and neither is copied.
fn children(parent: &str, list: Vec<Node>) -> Vec<Child> {
    let names: Vec<String> = list
        .iter()
        .enumerate()
        .map(
            |(i, n)| match n.raw_file_name.as_deref().map(String::from_utf16) {
                Some(Ok(s)) => s,
                // A unique key: these entries fail on their own name.
                _ => format!("\0{i}"),
            },
        )
        .collect();
    let partners = case_collisions(names.iter().map(String::as_str));
    list.into_iter()
        .zip(partners)
        .map(|(node, partner)| {
            let source = join_device_path(parent, &node.display_name());
            let name = match partner {
                Some(j) => Err(Error::NameCollision {
                    path: source.clone(),
                    other: names[j].clone(),
                }),
                None => local_name(&node),
            };
            Child { node, source, name }
        })
        .collect()
}

#[cfg(test)]
mod tests;
