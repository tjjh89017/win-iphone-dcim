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

#[test]
fn name_order_matches_the_lowercase_strings() {
    let names = [
        "", "a", "A", "a.HEIC", "A.heic", "IMG_0001", "img_0001", "IMG_0002", "Z", "_", "é", "É",
        "İ", "i\u{307}", "ΟΔΟΣ", "οδος", "οδοσ", "ß", "SS", "a\u{0}", "a b",
    ];
    for x in names {
        for y in names {
            let expected = x
                .to_lowercase()
                .cmp(&y.to_lowercase())
                .then_with(|| x.cmp(y));
            assert_eq!(name_order(x, y), expected, "{x:?} vs {y:?}");
        }
    }
}
