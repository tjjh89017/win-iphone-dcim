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
