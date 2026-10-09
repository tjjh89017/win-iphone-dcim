//! Entry ordering shared by `ls` and `tree`.

use std::cmp::Ordering;

use crate::model::Node as Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Name, case-insensitive first, then case-sensitive.
    #[default]
    Name,
    /// Size, largest first. Entries without a size go last.
    Size,
    /// Modification time, newest first. Entries without a time go last.
    Time,
    /// Keep the device order.
    None,
}

fn by_name(a: &Entry, b: &Entry) -> Ordering {
    name_order(&a.display_name(), &b.display_name())
}

/// The name order: case-insensitive first, then case-sensitive. The GUI
/// file list uses it too.
pub fn name_order(x: &str, y: &str) -> Ordering {
    x.to_lowercase()
        .cmp(&y.to_lowercase())
        .then_with(|| x.cmp(y))
}

/// Larger value first, `None` last: the `Size` and `Time` order.
pub fn desc_none_last<T: Ord>(a: &Option<T>, b: &Option<T>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => y.cmp(x),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Sort `entries` by `key`. `reverse` flips the whole order.
/// Ties fall back to the name order. `SortKey::None` ignores `reverse`.
pub fn sort_entries(entries: &mut [Entry], key: SortKey, reverse: bool) {
    match key {
        SortKey::None => return,
        SortKey::Name => entries.sort_by(by_name),
        SortKey::Size => {
            entries.sort_by(|a, b| desc_none_last(&a.size, &b.size).then_with(|| by_name(a, b)))
        }
        SortKey::Time => entries
            .sort_by(|a, b| desc_none_last(&a.modified, &b.modified).then_with(|| by_name(a, b))),
    }
    if reverse {
        entries.reverse();
    }
}

/// Move folders before files. The order inside each group stays.
pub fn folders_first(entries: &mut [Entry]) {
    entries.sort_by_key(|e| !e.is_folder);
}

#[cfg(test)]
mod tests;
