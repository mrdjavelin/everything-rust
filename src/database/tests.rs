use crate::core::{EntryKind, FsEntry};
use crate::database::schema::initialize_database;
use crate::database::*;
use crate::journal::{
    ATTRIBUTE_DIRECTORY, JournalRecord, REASON_FILE_DELETE, REASON_RENAME_NEW_NAME,
    REASON_RENAME_OLD_NAME,
};
use rusqlite::{Connection, OptionalExtension, Result};
use std::path::PathBuf;

fn test_connection() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    initialize_database(&connection).unwrap();
    connection
}

fn record(
    file: u64,
    parent: u64,
    usn: i64,
    reason: u32,
    name: &str,
    attributes: u32,
) -> JournalRecord {
    JournalRecord {
        file_reference_number: file,
        parent_file_reference_number: parent,
        usn,
        reason,
        attributes,
        name: name.to_owned(),
    }
}

fn path_for(connection: &Connection, path: &str) -> bool {
    connection
        .query_row("SELECT 1 FROM entries WHERE path = ?1", [path], |_| Ok(()))
        .optional()
        .unwrap()
        .is_some()
}

fn count_for_volume(connection: &Connection, table: &str, volume_id: &str) -> i64 {
    connection
        .query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE volume_id = ?1"),
            [volume_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn rebuild_and_incremental_rename_delete_are_consistent() {
    let mut connection = test_connection();
    let volume_id = "volume";
    let root = r"D:\";
    let records = [
        record(1, 1, 10, 0, "", ATTRIBUTE_DIRECTORY),
        record(2, 1, 11, 0, "folder", ATTRIBUTE_DIRECTORY),
        record(3, 2, 12, 0, "file.txt", 0),
    ];
    begin_volume_rebuild(&connection, volume_id).unwrap();
    stage_mft_records(&mut connection, volume_id, &records).unwrap();
    finish_volume_rebuild(&mut connection, volume_id, root, "42", 100).unwrap();

    assert!(path_for(&connection, r"D:\folder"));
    assert!(path_for(&connection, r"D:\folder\file.txt"));

    let rename = [
        record(
            2,
            1,
            101,
            REASON_RENAME_OLD_NAME,
            "folder",
            ATTRIBUTE_DIRECTORY,
        ),
        record(
            2,
            1,
            102,
            REASON_RENAME_NEW_NAME,
            "renamed",
            ATTRIBUTE_DIRECTORY,
        ),
    ];
    apply_usn_batch(&mut connection, volume_id, root, "42", 100, 103, &rename).unwrap();
    assert!(!path_for(&connection, r"D:\folder"));
    assert!(path_for(&connection, r"D:\renamed"));
    assert!(path_for(&connection, r"D:\renamed\file.txt"));

    let deletion = [record(3, 2, 103, REASON_FILE_DELETE, "file.txt", 0)];
    apply_usn_batch(&mut connection, volume_id, root, "42", 103, 104, &deletion).unwrap();
    assert!(!path_for(&connection, r"D:\renamed\file.txt"));
    assert_eq!(
        load_volume_sync_state(&connection, volume_id)
            .unwrap()
            .unwrap()
            .next_usn,
        104
    );
}

#[test]
fn invalid_journal_batch_rolls_back_index_and_cursor() {
    let mut connection = test_connection();
    let volume_id = "volume";
    let root = r"D:\";
    begin_volume_rebuild(&connection, volume_id).unwrap();
    stage_mft_records(
        &mut connection,
        volume_id,
        &[
            record(1, 1, 10, 0, "", ATTRIBUTE_DIRECTORY),
            record(2, 1, 11, 0, "existing.txt", 0),
        ],
    )
    .unwrap();
    finish_volume_rebuild(&mut connection, volume_id, root, "42", 100).unwrap();

    let out_of_order = [
        record(2, 1, 105, 0, "first.txt", 0),
        record(2, 1, 104, 0, "second.txt", 0),
    ];
    assert!(
        apply_usn_batch(
            &mut connection,
            volume_id,
            root,
            "42",
            100,
            106,
            &out_of_order
        )
        .is_err()
    );
    assert!(path_for(&connection, r"D:\existing.txt"));
    assert!(!path_for(&connection, r"D:\first.txt"));
    assert_eq!(
        load_volume_sync_state(&connection, volume_id)
            .unwrap()
            .unwrap()
            .next_usn,
        100
    );
}

#[test]
fn incomplete_fallback_scan_keeps_unseen_entries() {
    let mut connection = test_connection();
    let root = PathBuf::from(r"D:\");
    let existing = FsEntry {
        name: "keep.txt".to_owned(),
        path: root.join("keep.txt"),
        kind: EntryKind::File,
        size: Some(5),
        item_count: None,
    };
    let scan_id = begin_scanned_root(&mut connection).unwrap();
    save_scanned_batch(&mut connection, scan_id, &[existing]).unwrap();
    finish_scanned_root(&mut connection, &root, scan_id, true).unwrap();

    let partial_scan_id = begin_scanned_root(&mut connection).unwrap();
    finish_scanned_root(&mut connection, &root, partial_scan_id, false).unwrap();
    assert!(path_for(&connection, r"D:\keep.txt"));
}

#[test]
fn completed_fallback_scan_is_persisted_and_partial_scan_invalidates_it() {
    let mut connection = test_connection();
    let root = PathBuf::from(r"D:\");

    let scan_id = begin_scanned_root(&mut connection).unwrap();
    finish_scanned_root(&mut connection, &root, scan_id, true).unwrap();
    assert!(has_completed_fallback_scan(&connection, &root).unwrap());

    let partial_scan_id = begin_scanned_root(&mut connection).unwrap();
    finish_scanned_root(&mut connection, &root, partial_scan_id, false).unwrap();
    assert!(!has_completed_fallback_scan(&connection, &root).unwrap());
}

#[cfg(unix)]
#[test]
fn complete_unix_fallback_scan_removes_unseen_entries() {
    let mut connection = test_connection();
    let root = PathBuf::from("/tmp/everything-rust-unix-scan-root");
    let entry = FsEntry {
        name: "stale.txt".to_owned(),
        path: root.join("stale.txt"),
        kind: EntryKind::File,
        size: Some(5),
        item_count: None,
    };

    let scan_id = begin_scanned_root(&mut connection).unwrap();
    save_scanned_batch(&mut connection, scan_id, &[entry]).unwrap();
    finish_scanned_root(&mut connection, &root, scan_id, true).unwrap();

    let empty_scan_id = begin_scanned_root(&mut connection).unwrap();
    finish_scanned_root(&mut connection, &root, empty_scan_id, true).unwrap();
    assert!(!path_for(
        &connection,
        "/tmp/everything-rust-unix-scan-root/stale.txt"
    ));
}

#[test]
fn existing_database_schema_is_migrated() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE entries (
                    path TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    kind INTEGER NOT NULL,
                    size INTEGER,
                    item_count INTEGER
                );
                CREATE TABLE metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );",
        )
        .unwrap();

    initialize_database(&connection).unwrap();
    let columns = connection
        .prepare("PRAGMA table_info(entries)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>>>()
        .unwrap();

    assert!(columns.iter().any(|column| column == "last_seen"));
    assert!(columns.iter().any(|column| column == "volume_id"));
    let table_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table'
                   AND name IN ('volume_sync', 'ntfs_files', 'ntfs_staging', 'volume_rebuild')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 4);
}

#[test]
fn opening_disk_database_uses_wal_mode() {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("everything-rust-db-{suffix}.sqlite"));
    let path_string = path.to_string_lossy().into_owned();
    let connection = open_database(&path_string).unwrap();
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    drop(connection);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn search_combines_filters_and_treats_like_metacharacters_literally() {
    let mut connection = test_connection();
    let root = PathBuf::from(r"C:\docs");
    let entries = [
        FsEntry {
            name: "Report%.TXT".to_owned(),
            path: root.join("Report%.TXT"),
            kind: EntryKind::File,
            size: Some(12),
            item_count: None,
        },
        FsEntry {
            name: "ReportX.txt".to_owned(),
            path: root.join("ReportX.txt"),
            kind: EntryKind::File,
            size: Some(13),
            item_count: None,
        },
        FsEntry {
            name: "Report%.txt".to_owned(),
            path: root.join("Report%.txt"),
            kind: EntryKind::Directory,
            size: None,
            item_count: None,
        },
    ];
    let scan_id = begin_scanned_root(&mut connection).unwrap();
    save_scanned_batch(&mut connection, scan_id, &entries).unwrap();
    finish_scanned_root(&mut connection, &root, scan_id, true).unwrap();

    let results = search_entries(&connection, "% *.txt #file").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "Report%.TXT");
    assert!(matches!(results[0].kind, EntryKind::File));

    let extension_results = search_entries(&connection, "*.txt #file").unwrap();
    assert_eq!(extension_results.len(), 2);
}

#[test]
fn trigram_search_finds_substrings_and_tracks_entry_updates() {
    let mut connection = test_connection();
    let path = PathBuf::from(r"C:\docs\secret-folder\Report%.TXT");
    let entry = FsEntry {
        name: "Report%.TXT".to_owned(),
        path: path.clone(),
        kind: EntryKind::File,
        size: Some(12),
        item_count: None,
    };
    let scan_id = begin_scanned_root(&mut connection).unwrap();
    save_scanned_batch(&mut connection, scan_id, &[entry]).unwrap();

    let results = search_entries(&connection, "port%.").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "Report%.TXT");

    let results = search_entries(&connection, "secret-folder").unwrap();
    assert_eq!(results.len(), 1);

    connection
        .execute(
            "UPDATE entries SET name = 'renamed-report.txt',
             path = 'C:\\docs\\secret-folder\\renamed-report.txt'
             WHERE path = ?1",
            [path.to_string_lossy().as_ref()],
        )
        .unwrap_or_else(|error| panic!("updating the entry failed: {error:?}"));
    assert!(search_entries(&connection, "port%.").unwrap().is_empty());
    assert_eq!(
        search_entries(&connection, "renamed-report").unwrap().len(),
        1
    );

    connection
        .execute(
            "DELETE FROM entries WHERE path = 'C:\\docs\\secret-folder\\renamed-report.txt'",
            [],
        )
        .unwrap();
    assert!(
        search_entries(&connection, "renamed-report")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn empty_query_respects_kind_filter() {
    let mut connection = test_connection();
    let root = PathBuf::from(r"C:\docs");
    let entries = [
        FsEntry {
            name: "notes.txt".to_owned(),
            path: root.join("notes.txt"),
            kind: EntryKind::File,
            size: Some(4),
            item_count: None,
        },
        FsEntry {
            name: "archive".to_owned(),
            path: root.join("archive"),
            kind: EntryKind::Directory,
            size: None,
            item_count: None,
        },
    ];
    let scan_id = begin_scanned_root(&mut connection).unwrap();
    save_scanned_batch(&mut connection, scan_id, &entries).unwrap();
    finish_scanned_root(&mut connection, &root, scan_id, true).unwrap();

    let results = search_entries(&connection, "#dir").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "archive");
}

#[test]
fn synchronizes_large_directory_in_bounded_batches() {
    let mut connection = test_connection();
    let volume_id = "large-volume";
    let mut records = Vec::with_capacity(10_002);
    records.push(record(1, 1, 1, 0, "", ATTRIBUTE_DIRECTORY));
    records.push(record(2, 1, 2, 0, "catalog", ATTRIBUTE_DIRECTORY));
    records.extend((0..10_000).map(|index| {
        record(
            index + 3,
            2,
            index as i64 + 3,
            0,
            &format!("file-{index}.dat"),
            0,
        )
    }));

    start_volume_rebuild(&mut connection, volume_id, r"D:\", "journal-1", 50_000).unwrap();
    for batch in records.chunks(512) {
        let next_reference = batch
            .last()
            .map(|record| record.file_reference_number + 1)
            .unwrap_or(0);
        stage_mft_page(&mut connection, volume_id, batch, next_reference).unwrap();
    }
    finish_volume_rebuild(&mut connection, volume_id, r"D:\", "journal-1", 50_000).unwrap();
    let staging_index_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'ntfs_staging_parent_idx'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(staging_index_count, 1);

    assert_eq!(
        count_for_volume(&connection, "ntfs_files", volume_id),
        10_002
    );
    assert_eq!(count_for_volume(&connection, "entries", volume_id), 10_001);
    assert!(path_for(&connection, r"D:\catalog\file-9999.dat"));
}

#[test]
fn rebuild_skips_mft_records_without_a_resolvable_parent() {
    let mut connection = test_connection();
    let volume_id = "orphan-volume";
    begin_volume_rebuild(&connection, volume_id).unwrap();
    stage_mft_records(
        &mut connection,
        volume_id,
        &[
            record(1, 1, 1, 0, "", ATTRIBUTE_DIRECTORY),
            record(2, 1, 2, 0, "reachable.txt", 0),
            record(3, 99, 3, 0, "orphaned.txt", 0),
        ],
    )
    .unwrap();

    let skipped =
        finish_volume_rebuild(&mut connection, volume_id, r"D:\", "journal-1", 100).unwrap();

    assert_eq!(skipped, 1);
    assert!(path_for(&connection, r"D:\reachable.txt"));
    assert!(!path_for(&connection, r"D:\orphaned.txt"));
    assert_eq!(count_for_volume(&connection, "ntfs_files", volume_id), 2);
}

#[test]
fn interrupted_mft_rebuild_resumes_from_persisted_page_cursor() {
    let mut connection = test_connection();
    let volume_id = "resumable-volume";
    let root = r"D:\";
    start_volume_rebuild(&mut connection, volume_id, root, "journal-7", 100).unwrap();
    let staging_index_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'ntfs_staging_parent_idx'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(staging_index_count, 0);
    stage_mft_page(
        &mut connection,
        volume_id,
        &[
            record(1, 1, 1, 0, "", ATTRIBUTE_DIRECTORY),
            record(2, 1, 2, 0, "saved.txt", 0),
        ],
        4096,
    )
    .unwrap();

    let checkpoint = load_volume_rebuild_checkpoint(&connection, volume_id)
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.root_path, root);
    assert_eq!(checkpoint.journal_id, "journal-7");
    assert_eq!(checkpoint.next_usn, 100);
    assert_eq!(checkpoint.next_file_reference, "4096");
    assert_eq!(count_for_volume(&connection, "ntfs_staging", volume_id), 2);

    begin_volume_rebuild(&connection, volume_id).unwrap();
    assert!(
        load_volume_rebuild_checkpoint(&connection, volume_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(count_for_volume(&connection, "ntfs_staging", volume_id), 0);
}

#[test]
fn renaming_directory_updates_all_descendant_paths() {
    let mut connection = test_connection();
    let volume_id = "rename-volume";
    begin_volume_rebuild(&connection, volume_id).unwrap();
    stage_mft_records(
        &mut connection,
        volume_id,
        &[
            record(1, 1, 1, 0, "", ATTRIBUTE_DIRECTORY),
            record(2, 1, 2, 0, "before", ATTRIBUTE_DIRECTORY),
            record(3, 2, 3, 0, "nested", ATTRIBUTE_DIRECTORY),
            record(4, 3, 4, 0, "item.txt", 0),
        ],
    )
    .unwrap();
    finish_volume_rebuild(&mut connection, volume_id, r"D:\", "journal-1", 100).unwrap();

    let rename = [
        record(
            2,
            1,
            100,
            REASON_RENAME_OLD_NAME,
            "before",
            ATTRIBUTE_DIRECTORY,
        ),
        record(
            2,
            1,
            101,
            REASON_RENAME_NEW_NAME,
            "after",
            ATTRIBUTE_DIRECTORY,
        ),
    ];
    apply_usn_batch(
        &mut connection,
        volume_id,
        r"D:\",
        "journal-1",
        100,
        102,
        &rename,
    )
    .unwrap();

    assert!(!path_for(&connection, r"D:\before\nested\item.txt"));
    assert!(path_for(&connection, r"D:\after\nested\item.txt"));
    let path_results = search_entries(&connection, "after/nested/item.txt").unwrap();
    assert_eq!(path_results.len(), 1);
    assert_eq!(path_results[0].name, "item.txt");
    let indexed_path: String = connection
        .query_row(
            "SELECT path FROM ntfs_files WHERE volume_id = ?1 AND file_reference = '4'",
            [volume_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexed_path, r"D:\after\nested\item.txt");
}

#[test]
fn deleting_directory_removes_its_entire_indexed_subtree() {
    let mut connection = test_connection();
    let volume_id = "delete-volume";
    let mut records = Vec::with_capacity(2_002);
    records.push(record(1, 1, 1, 0, "", ATTRIBUTE_DIRECTORY));
    records.push(record(2, 1, 2, 0, "removed", ATTRIBUTE_DIRECTORY));
    records.extend((0..2_000).map(|index| {
        record(
            index + 3,
            2,
            index as i64 + 3,
            0,
            &format!("file-{index}.dat"),
            0,
        )
    }));
    begin_volume_rebuild(&connection, volume_id).unwrap();
    for batch in records.chunks(512) {
        stage_mft_records(&mut connection, volume_id, batch).unwrap();
    }
    finish_volume_rebuild(&mut connection, volume_id, r"D:\", "journal-1", 100).unwrap();

    let deletion = [record(
        2,
        1,
        100,
        REASON_FILE_DELETE,
        "removed",
        ATTRIBUTE_DIRECTORY,
    )];
    apply_usn_batch(
        &mut connection,
        volume_id,
        r"D:\",
        "journal-1",
        100,
        101,
        &deletion,
    )
    .unwrap();

    assert_eq!(count_for_volume(&connection, "ntfs_files", volume_id), 1);
    assert_eq!(count_for_volume(&connection, "entries", volume_id), 0);
}

#[test]
fn interrupted_rebuild_can_be_restarted_without_partial_index() {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("everything-rust-recovery-{suffix}.sqlite"));
    {
        let mut connection = Connection::open(&path).unwrap();
        initialize_database(&connection).unwrap();
        begin_volume_rebuild(&connection, "recovery-volume").unwrap();
        stage_mft_records(
            &mut connection,
            "recovery-volume",
            &[record(1, 1, 1, 0, "", ATTRIBUTE_DIRECTORY)],
        )
        .unwrap();
    }

    let mut connection = Connection::open(&path).unwrap();
    initialize_database(&connection).unwrap();
    begin_volume_rebuild(&connection, "recovery-volume").unwrap();
    stage_mft_records(
        &mut connection,
        "recovery-volume",
        &[
            record(1, 1, 1, 0, "", ATTRIBUTE_DIRECTORY),
            record(2, 1, 2, 0, "recovered.txt", 0),
        ],
    )
    .unwrap();
    finish_volume_rebuild(&mut connection, "recovery-volume", r"D:\", "journal-2", 200).unwrap();

    assert!(path_for(&connection, r"D:\recovered.txt"));
    assert_eq!(
        count_for_volume(&connection, "ntfs_staging", "recovery-volume"),
        0
    );
    assert_eq!(
        load_volume_sync_state(&connection, "recovery-volume")
            .unwrap()
            .unwrap()
            .next_usn,
        200
    );
    drop(connection);
    std::fs::remove_file(path).unwrap();
}
