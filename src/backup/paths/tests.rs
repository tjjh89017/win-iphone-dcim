use super::*;

fn reason(name: &str) -> &'static str {
    check_name(name).unwrap_err()
}

#[test]
fn normal_names_pass() {
    for ok in [
        "IMG_0001.HEIC",
        "202601_a",
        ".hidden",
        "a.b.c",
        "照片 001.HEIC",
        "旅行🗾",
        "CONSOLE.txt",
        "COM10",
        "LPT0",
        "com",
        "NULL",
    ] {
        assert_eq!(check_name(ok), Ok(()), "{ok:?}");
    }
}

#[test]
fn empty_and_dot_names() {
    assert_eq!(reason(""), "empty name");
    assert_eq!(reason("."), "'.' or '..'");
    assert_eq!(reason(".."), "'.' or '..'");
}

#[test]
fn separators() {
    for bad in ["a/b", "a\\b", "/abs", "\\\\server\\share", "../x", "..\\x"] {
        assert_eq!(reason(bad), "path separator", "{bad:?}");
    }
}

#[test]
fn drive_prefix() {
    assert_eq!(reason("C:x"), "drive prefix");
    assert_eq!(reason("d:"), "drive prefix");
}

#[test]
fn illegal_characters() {
    for c in ['<', '>', ':', '"', '|', '?', '*'] {
        assert_eq!(reason(&format!("ab{c}cd")), "illegal character", "{c:?}");
    }
}

#[test]
fn control_characters() {
    for c in ['\0', '\u{1}', '\n', '\t', '\u{1f}', '\u{7f}'] {
        assert_eq!(reason(&format!("a{c}b")), "control character", "{c:?}");
    }
}

#[test]
fn trailing_dot_or_space() {
    assert_eq!(reason("name."), "trailing dot or space");
    assert_eq!(reason("name "), "trailing dot or space");
    assert_eq!(reason("..."), "trailing dot or space");
}

#[test]
fn reserved_names() {
    for bad in [
        "CON",
        "con",
        "Prn",
        "AUX",
        "nul",
        "NUL.txt",
        "con.tar.gz",
        "COM1",
        "com9.log",
        "LPT1",
        "lpt9",
        "COM¹",
        "LPT³.x",
        "AUX .txt",
    ] {
        assert_eq!(reason(bad), "reserved Windows name", "{bad:?}");
    }
}

#[test]
fn safe_file_name_wraps_the_reason() {
    let err = safe_file_name("CON").unwrap_err();
    assert!(matches!(
        err,
        Error::UnsafeFileName {
            reason: "reserved Windows name",
            ..
        }
    ));
    assert_eq!(safe_file_name("IMG.HEIC").unwrap(), "IMG.HEIC");
}

#[test]
fn case_collisions_mark_both_entries() {
    let names = ["IMG_0001.HEIC", "img_0001.heic", "IMG_0002.MOV", "Ä", "ä"];
    let c = case_collisions(names);
    assert_eq!(c, vec![Some(1), Some(0), None, Some(4), Some(3)]);
}

#[test]
fn identical_names_collide() {
    assert_eq!(
        case_collisions(["a", "a", "b"]),
        vec![Some(1), Some(0), None]
    );
}

#[test]
fn three_way_collision_marks_all() {
    let c = case_collisions(["x", "X", "x"]);
    assert!(c.iter().all(Option::is_some));
}
