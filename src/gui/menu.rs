//! Which items of the menu bar and the top bar are enabled, and the text
//! of the About window. The window fills a `MenuState` once per frame; both
//! bars read it.

/// The window facts that the enabled items depend on.
#[derive(Clone, Copy, Debug, Default)]
pub struct MenuState {
    /// A device tree is open.
    pub device_open: bool,
    /// A destination folder is set.
    pub has_dest: bool,
    /// The tree has checked items.
    pub checked: bool,
    /// The number of highlighted rows in the file list.
    pub highlighted: usize,
    /// The only highlighted row is a file with a complete cached copy.
    pub one_cached_file: bool,
    /// The number of rows in the file list.
    pub rows: usize,
    pub copying: bool,
    /// Explorer copy is available (the worker runs).
    pub explorer: bool,
}

impl MenuState {
    /// "Copy to folder": the checked items into the destination.
    pub fn copy_to_folder(&self) -> bool {
        !self.copying && self.has_dest && self.checked
    }

    /// "Copy to...": the highlighted rows, or the checked items.
    pub fn copy_to(&self) -> bool {
        !self.copying && self.has_targets()
    }

    /// "Copy (paste in Explorer)": the highlighted rows, or the checked items.
    pub fn explorer_copy(&self) -> bool {
        self.explorer && self.has_targets()
    }

    pub fn cancel(&self) -> bool {
        self.copying
    }

    /// Open and Properties act on one highlighted row.
    pub fn one_row(&self) -> bool {
        self.highlighted == 1
    }

    pub fn open_cache_folder(&self) -> bool {
        self.one_row() && self.one_cached_file
    }

    pub fn clear_cache(&self) -> bool {
        self.device_open
    }

    pub fn select_all(&self) -> bool {
        self.rows > 0
    }

    pub fn clear_selection(&self) -> bool {
        self.highlighted > 0
    }

    pub fn check_all(&self) -> bool {
        self.device_open
    }

    pub fn uncheck_all(&self) -> bool {
        self.checked
    }

    fn has_targets(&self) -> bool {
        self.highlighted > 0 || self.checked
    }
}

/// The year in the copyright line of LICENSE.
pub const COPYRIGHT_YEAR: u16 = 2026;

/// The long name of an SPDX license id, for the About window.
pub fn license_name(spdx: &str) -> String {
    match spdx {
        "Apache-2.0" => "Apache License 2.0".into(),
        "MIT" => "MIT License".into(),
        other => other.into(),
    }
}

/// The names in `CARGO_PKG_AUTHORS` (`:` separated), without emails.
pub fn author_names(authors: &str) -> String {
    authors
        .split(':')
        .map(|a| a.split('<').next().unwrap_or(a).trim())
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The copyright line of the About window.
pub fn copyright(authors: &str) -> String {
    format!("Copyright (c) {COPYRIGHT_YEAR} {}", author_names(authors))
}

#[cfg(test)]
mod tests;
