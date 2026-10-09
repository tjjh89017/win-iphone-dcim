//! Back, Forward and Up for the file list, and the path bar segments.
//! Portable, like File Explorer's navigation history.

/// The shown folder with a back stack and a forward stack.
#[derive(Debug, Default, Clone)]
pub struct NavHistory {
    current: Option<String>,
    back: Vec<String>,
    forward: Vec<String>,
}

impl NavHistory {
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Start again at `path`, without history (a new device).
    pub fn reset(&mut self, path: &str) {
        *self = Self {
            current: Some(path.to_owned()),
            ..Self::default()
        };
    }

    /// Enter `path`. The current folder goes on the back stack and the
    /// forward stack is cleared. Entering the shown folder does nothing.
    pub fn go(&mut self, path: &str) {
        if self.current.as_deref() == Some(path) {
            return;
        }
        if let Some(c) = self.current.replace(path.to_owned()) {
            self.back.push(c);
        }
        self.forward.clear();
    }

    /// Go back. Returns the new folder.
    pub fn back(&mut self) -> Option<&str> {
        let to = self.back.pop()?;
        if let Some(c) = self.current.replace(to) {
            self.forward.push(c);
        }
        self.current()
    }

    /// Go forward. Returns the new folder.
    pub fn forward(&mut self) -> Option<&str> {
        let to = self.forward.pop()?;
        if let Some(c) = self.current.replace(to) {
            self.back.push(c);
        }
        self.current()
    }

    /// Enter the parent folder. Returns the new folder, or `None` at the root.
    pub fn up(&mut self) -> Option<&str> {
        let parent = parent(self.current.as_deref()?)?.to_owned();
        self.go(&parent);
        self.current()
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn can_up(&self) -> bool {
        self.current.as_deref().and_then(parent).is_some()
    }
}

/// The parent of a device path. `None` for the root.
fn parent(path: &str) -> Option<&str> {
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return None;
    }
    match path.rsplit_once('/') {
        Some(("", _)) | None => Some("/"),
        Some((p, _)) => Some(p),
    }
}

/// The path bar segments of `path`: (label, device path), from the root.
pub fn breadcrumbs(path: &str) -> Vec<(String, String)> {
    let mut out = vec![("/".to_owned(), "/".to_owned())];
    let mut at = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        at.push('/');
        at.push_str(part);
        out.push((part.to_owned(), at.clone()));
    }
    out
}

#[cfg(test)]
mod tests;
