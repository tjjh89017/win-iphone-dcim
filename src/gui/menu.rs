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
mod tests {
    use super::*;

    fn open() -> MenuState {
        MenuState {
            device_open: true,
            explorer: true,
            rows: 3,
            ..Default::default()
        }
    }

    #[test]
    fn nothing_is_enabled_without_a_device() {
        let s = MenuState::default();
        assert!(!s.copy_to_folder());
        assert!(!s.copy_to());
        assert!(!s.explorer_copy());
        assert!(!s.cancel());
        assert!(!s.one_row());
        assert!(!s.open_cache_folder());
        assert!(!s.clear_cache());
        assert!(!s.select_all());
        assert!(!s.clear_selection());
        assert!(!s.check_all());
        assert!(!s.uncheck_all());
    }

    #[test]
    fn copy_to_folder_needs_destination_and_checked_items() {
        let mut s = open();
        s.checked = true;
        assert!(!s.copy_to_folder());
        s.has_dest = true;
        assert!(s.copy_to_folder());
        s.checked = false;
        s.highlighted = 2;
        assert!(!s.copy_to_folder());
    }

    #[test]
    fn copies_are_disabled_while_a_copy_runs() {
        let mut s = open();
        s.has_dest = true;
        s.checked = true;
        s.copying = true;
        assert!(!s.copy_to_folder());
        assert!(!s.copy_to());
        assert!(s.cancel());
        // Explorer reads through its own streams, not the copy engine.
        assert!(s.explorer_copy());
    }

    #[test]
    fn copy_targets_are_highlighted_rows_or_checked_items() {
        let mut s = open();
        assert!(!s.copy_to());
        assert!(!s.explorer_copy());
        s.highlighted = 1;
        assert!(s.copy_to());
        assert!(s.explorer_copy());
        s.highlighted = 0;
        s.checked = true;
        assert!(s.copy_to());
        assert!(s.explorer_copy());
        s.explorer = false;
        assert!(!s.explorer_copy());
    }

    #[test]
    fn open_and_cache_folder_need_one_row() {
        let mut s = open();
        s.one_cached_file = true;
        s.highlighted = 2;
        assert!(!s.one_row());
        assert!(!s.open_cache_folder());
        s.highlighted = 1;
        assert!(s.one_row());
        assert!(s.open_cache_folder());
        s.one_cached_file = false;
        assert!(!s.open_cache_folder());
    }

    #[test]
    fn selection_items_follow_the_rows() {
        let mut s = open();
        assert!(s.select_all());
        assert!(!s.clear_selection());
        s.highlighted = 1;
        assert!(s.clear_selection());
        s.rows = 0;
        assert!(!s.select_all());
    }

    #[test]
    fn uncheck_all_needs_checked_items() {
        let mut s = open();
        assert!(s.check_all());
        assert!(!s.uncheck_all());
        s.checked = true;
        assert!(s.uncheck_all());
    }

    #[test]
    fn about_lines_name_the_license_and_the_holder() {
        assert_eq!(license_name("Apache-2.0"), "Apache License 2.0");
        assert_eq!(author_names("Date Huang <a@b.c>"), "Date Huang");
        assert_eq!(author_names("A <a@b.c>:B"), "A, B");
        assert_eq!(
            copyright(env!("CARGO_PKG_AUTHORS")),
            "Copyright (c) 2026 Date Huang"
        );
        assert_eq!(
            license_name(env!("CARGO_PKG_LICENSE")),
            "Apache License 2.0"
        );
    }
}
