use crate::entry::{EntryKind, FsEntry};
use rusqlite::{Connection, Result};

fn insert_entry_for_scan(connection: &Connection, entry: &FsEntry, scan_id: i64) -> Result<()> {
    let kind = match entry.kind {
        EntryKind::File => 0,
        EntryKind::Directory => 1,
    };

    let size = entry.size.map(i64::try_from).transpose().map_err(|_| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
            "File size exceeds SQLite INTEGER range",
        )))
    })?;

    let item_count = entry
        .item_count
        .map(i64::try_from)
        .transpose()
        .map_err(|_| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                "Item count exceeds SQLite INTEGER range",
            )))
        })?;

    let mut statement = connection.prepare_cached(
        "INSERT INTO entries (
            path, name, kind, size, item_count, last_seen
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(path) DO UPDATE SET
            name = excluded.name,
            kind = excluded.kind,
            size = excluded.size,
            item_count = excluded.item_count,
            last_seen = excluded.last_seen",
    )?;

    statement.execute(rusqlite::params![
        entry.path.to_string_lossy(),
        entry.name,
        kind,
        size,
        item_count,
        scan_id
    ])?;

    Ok(())
}

fn next_scan_id(connection: &Connection) -> Result<i64> {
    connection.execute(
        "INSERT INTO metadata (key, value)
         VALUES ('scan_id', '0')
         ON CONFLICT(key) DO NOTHING",
        [],
    )?;

    let current: i64 = connection.query_row(
        "SELECT CAST(value AS INTEGER)
         FROM metadata
         WHERE key = 'scan_id'",
        [],
        |row| row.get(0),
    )?;

    let next = current.checked_add(1).ok_or_else(|| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
            "Scan ID limit reached",
        )))
    })?;

    connection.execute(
        "UPDATE metadata SET value = ?1 WHERE key = 'scan_id'",
        [next.to_string()],
    )?;

    Ok(next)
}

pub fn begin_scanned_root(connection: &mut Connection) -> Result<i64> {
    let transaction = connection.transaction()?;
    let scan_id = next_scan_id(&transaction)?;
    transaction.commit()?;
    Ok(scan_id)
}

pub fn save_scanned_batch(
    connection: &mut Connection,
    scan_id: i64,
    entries: &[FsEntry],
) -> Result<()> {
    let transaction = connection.transaction()?;
    for entry in entries {
        insert_entry_for_scan(&transaction, entry, scan_id)?;
    }
    transaction.commit()
}

pub fn finish_scanned_root(
    connection: &mut Connection,
    root: &std::path::Path,
    scan_id: i64,
    complete: bool,
) -> Result<()> {
    if !complete {
        return Ok(());
    }

    let root = root.to_string_lossy().into_owned();
    let prefix = if root.ends_with(['\\', '/']) {
        root.clone()
    } else {
        format!("{root}\\")
    };
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM ntfs_files
         WHERE volume_id IN (SELECT volume_id FROM volume_sync WHERE root_path = ?1)",
        [&root],
    )?;
    transaction.execute(
        "DELETE FROM ntfs_staging
         WHERE volume_id IN (SELECT volume_id FROM volume_sync WHERE root_path = ?1)",
        [&root],
    )?;
    transaction.execute(
        "UPDATE entries SET volume_id = NULL
         WHERE volume_id IN (SELECT volume_id FROM volume_sync WHERE root_path = ?1)",
        [&root],
    )?;
    transaction.execute("DELETE FROM volume_sync WHERE root_path = ?1", [&root])?;
    transaction.execute(
        "UPDATE entries SET volume_id = NULL
         WHERE path = ?1 OR substr(path, 1, length(?2)) = ?2",
        rusqlite::params![root, prefix],
    )?;
    transaction.execute(
        "DELETE FROM entries
         WHERE (path = ?1 OR substr(path, 1, length(?2)) = ?2)
           AND last_seen != ?3",
        rusqlite::params![root, prefix, scan_id],
    )?;
    transaction.commit()
}
