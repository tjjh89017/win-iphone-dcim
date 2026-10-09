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
    let (x, y) = (a.display_name(), b.display_name());
    x.to_lowercase()
        .cmp(&y.to_lowercase())
        .then_with(|| x.cmp(&y))
}

/// Larger value first, `None` last.
fn desc_none_last<T: Ord>(a: &Option<T>, b: &Option<T>) -> Ordering {
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
mod tests {
    use super::*;
    use crate::model::ObjectId;

    fn node(name: &str, folder: bool, size: Option<u64>, modified: Option<&str>) -> Entry {
        Entry {
            id: ObjectId::new(name),
            name: Some(name.into()),
            original_file_name: None,
            is_folder: folder,
            size,
            content_type: None,
            modified: modified.map(crate::model::LocalTime::parse),
            created: None,
            raw_file_name: None,
        }
    }

    fn names(v: &[Entry]) -> Vec<String> {
        v.iter().map(Entry::display_name).collect()
    }

    #[test]
    fn name_is_case_insensitive_with_case_tie_break() {
        let mut v = vec![
            node("b", false, None, None),
            node("B", false, None, None),
            node("a", false, None, None),
            node("C", true, None, None),
        ];
        sort_entries(&mut v, SortKey::Name, false);
        assert_eq!(names(&v), ["a", "B", "b", "C"]);
    }

    #[test]
    fn size_largest_first_missing_last() {
        let mut v = vec![
            node("small", false, Some(1), None),
            node("none", true, None, None),
            node("big", false, Some(9), None),
        ];
        sort_entries(&mut v, SortKey::Size, false);
        assert_eq!(names(&v), ["big", "small", "none"]);
    }

    #[test]
    fn time_newest_first_missing_last() {
        let mut v = vec![
            node("old", false, None, Some("2020-01-01 00:00:00")),
            node("none", false, None, None),
            node("new", false, None, Some("2024-01-01 00:00:00")),
        ];
        sort_entries(&mut v, SortKey::Time, false);
        assert_eq!(names(&v), ["new", "old", "none"]);
    }

    #[test]
    fn reverse_flips_and_none_keeps_order() {
        let mut v = vec![node("b", false, None, None), node("a", false, None, None)];
        sort_entries(&mut v, SortKey::Name, true);
        assert_eq!(names(&v), ["b", "a"]);
        let mut v = vec![node("b", false, None, None), node("a", false, None, None)];
        sort_entries(&mut v, SortKey::None, true);
        assert_eq!(names(&v), ["b", "a"]);
    }

    #[test]
    fn folders_first_is_stable() {
        let mut v = vec![
            node("a", false, None, None),
            node("z", true, None, None),
            node("m", true, None, None),
        ];
        folders_first(&mut v);
        assert_eq!(names(&v), ["z", "m", "a"]);
    }
}
