use super::*;

fn file(rel: &str, size: Option<u64>) -> FileItem {
    FileItem {
        path: format!("/DCIM/{}", rel.replace('\\', "/")),
        rel: rel.into(),
        is_folder: false,
        size,
        modified: None,
        created: None,
    }
}

fn folder(rel: &str) -> FileItem {
    FileItem {
        is_folder: true,
        ..file(rel, None)
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn name_at(b: &[u8]) -> String {
    let units: Vec<u16> = b[NAME_OFFSET..DESCRIPTOR_SIZE]
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16(&units).unwrap()
}

fn no_time(_: LocalTime) -> Option<u64> {
    None
}

#[test]
fn relative_paths_use_backslashes() {
    let base = "/Internal Storage/DCIM";
    assert_eq!(
        relative(base, "/Internal Storage/DCIM/202601_a/IMG_0001.HEIC").as_deref(),
        Some("202601_a\\IMG_0001.HEIC")
    );
    assert_eq!(
        relative("/", "/Internal Storage").as_deref(),
        Some("Internal Storage")
    );
    assert_eq!(relative(base, "/Internal Storage/DCIMX/a"), None);
    assert_eq!(relative(base, base), None);
    assert_eq!(
        common_parent(&[
            "/Internal Storage/DCIM/202601_a".into(),
            "/Internal Storage/DCIM/202601_b/IMG_0001.HEIC".into(),
        ]),
        base
    );
    assert_eq!(
        common_parent(&["/Internal Storage/DCIM/202601_a".into()]),
        base
    );
    assert_eq!(common_parent(&["/Internal Storage".into()]), "/");
    assert_eq!(parent("/Internal Storage/DCIM/"), "/Internal Storage");
}

#[test]
fn group_layout_and_flags() {
    let items = [
        folder("202601_a"),
        FileItem {
            modified: Some(LocalTime(0)),
            created: Some(LocalTime(86_400)),
            ..file("202601_a\\IMG_0001.HEIC", Some(6))
        },
    ];
    let to_ft = |t: LocalTime| filetime(local_to_system_time(t).ok()?);
    let listing = Listing::new(&items, to_ft);
    let b = &listing.group;
    assert_eq!(b.len(), 4 + 2 * DESCRIPTOR_SIZE);
    assert_eq!(u32_at(b, 0), 2);
    assert_eq!((listing.len(), listing.files), (2, 1));

    let d0 = &b[4..4 + DESCRIPTOR_SIZE];
    assert_eq!(u32_at(d0, 0), FD_ATTRIBUTES | FD_PROGRESSUI | FD_UNICODE);
    assert_eq!(u32_at(d0, 36), FILE_ATTRIBUTE_DIRECTORY);
    assert_eq!((u32_at(d0, 64), u32_at(d0, 68)), (0, 0));
    assert_eq!(name_at(d0), "202601_a");
    assert!(listing.file(0).is_none());

    let d1 = &b[4 + DESCRIPTOR_SIZE..];
    assert_eq!(
        u32_at(d1, 0),
        FD_ATTRIBUTES | FD_PROGRESSUI | FD_UNICODE | FD_FILESIZE | FD_WRITESTIME | FD_CREATETIME
    );
    assert_eq!(u32_at(d1, 36), FILE_ATTRIBUTE_NORMAL);
    assert_eq!(u32_at(d1, 68), 6);
    assert_eq!(name_at(d1), "202601_a\\IMG_0001.HEIC");
    // Linux treats device time as UTC: 1970-01-01 is the epoch offset.
    let low = u32_at(d1, 56) as u64;
    let high = u32_at(d1, 60) as u64;
    assert_eq!(high << 32 | low, EPOCH_DIFF_100NS as u64);
    let created = (u32_at(d1, 44) as u64) << 32 | u32_at(d1, 40) as u64;
    assert_eq!(created, EPOCH_DIFF_100NS as u64 + 86_400 * 10_000_000);

    let f = listing.file(1).unwrap();
    assert_eq!(
        (f.name.as_str(), f.number, f.size),
        ("IMG_0001.HEIC", 1, Some(6))
    );
    assert!(listing.file(2).is_none() && listing.file(-1).is_none());
}

#[test]
fn five_gib_size_splits_into_high_and_low() {
    let size = 5 * 1024 * 1024 * 1024u64;
    assert_eq!(split_size(size), (1, 0x4000_0000));
    let listing = Listing::new(&[file("BIG.MOV", Some(size))], no_time);
    let d = &listing.group[4..];
    assert_eq!((u32_at(d, 64), u32_at(d, 68)), (1, 0x4000_0000));
}

#[test]
fn missing_size_omits_the_size_flag() {
    let d = descriptor(&file("IMG.HEIC", None), no_time).unwrap();
    assert_eq!(d.flags & FD_FILESIZE, 0);
    assert_eq!(d.flags & (FD_WRITESTIME | FD_CREATETIME), 0);
    assert_eq!(d.attributes, FILE_ATTRIBUTE_NORMAL);
    // A folder never has a size, even if the device gives one.
    let mut f = folder("A");
    f.size = Some(10);
    let d = descriptor(&f, no_time).unwrap();
    assert_eq!((d.flags & FD_FILESIZE, d.size), (0, None));
}

#[test]
fn overlong_paths_are_left_out() {
    let fits = "a".repeat(MAX_PATH - 1);
    let long = "b".repeat(MAX_PATH);
    let listing = Listing::new(
        &[
            file(&fits, Some(1)),
            file(&long, Some(2)),
            file("c", Some(3)),
        ],
        no_time,
    );
    assert_eq!((listing.len(), listing.files), (2, 2));
    assert_eq!(listing.skipped.len(), 1);
    assert!(listing.skipped[0].contains("longer than 259"));
    assert_eq!(u32_at(&listing.group, 0), 2);
    assert_eq!(name_at(&listing.group[4..]), fits);
    let second = listing.file(1).unwrap();
    assert_eq!((second.name.as_str(), second.number), ("c", 2));
    // Non-BMP characters count as two UTF-16 units.
    let emoji = "\u{1F600}".repeat(130);
    assert!(descriptor(&file(&emoji, None), no_time).is_err());
}
