use std::io::{Cursor, Read};

use super::*;

#[test]
fn dcp_archive_contains_sorted_complete_replacement_files() {
    let entries = vec![
        DcpEntry {
            name: "SECOND.PVR".to_string(),
            data: vec![4, 5, 6],
        },
        DcpEntry {
            name: "FIRST.PVR".to_string(),
            data: vec![1, 2, 3],
        },
    ];

    let cursor = write_dcp_archive(Cursor::new(Vec::new()), &entries).unwrap();
    let mut archive = zip::ZipArchive::new(Cursor::new(cursor.into_inner())).unwrap();

    assert_eq!(archive.len(), 2);
    let mut first_data = Vec::new();
    let mut first = archive.by_index(0).unwrap();
    assert_eq!(first.name(), "FIRST.PVR");
    first.read_to_end(&mut first_data).unwrap();
    assert_eq!(first_data, [1, 2, 3]);
    drop(first);

    let mut second_data = Vec::new();
    let mut second = archive.by_index(1).unwrap();
    assert_eq!(second.name(), "SECOND.PVR");
    second.read_to_end(&mut second_data).unwrap();
    assert_eq!(second_data, [4, 5, 6]);
}

#[test]
fn dcp_archive_rejects_unsafe_and_duplicate_names() {
    let unsafe_entry = [DcpEntry {
        name: "../TRACK03.BIN".to_string(),
        data: vec![1],
    }];
    assert!(write_dcp_archive(Cursor::new(Vec::new()), &unsafe_entry).is_err());

    let duplicate_entries = [
        DcpEntry {
            name: "FRAME.PVR".to_string(),
            data: vec![1],
        },
        DcpEntry {
            name: "frame.pvr".to_string(),
            data: vec![2],
        },
    ];
    assert!(write_dcp_archive(Cursor::new(Vec::new()), &duplicate_entries).is_err());
}
