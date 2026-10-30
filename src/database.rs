use std::path::PathBuf;

use crate::entry::{EntryKind, FsEntry};
use crate::journal::{
    ATTRIBUTE_DIRECTORY, JournalRecord, REASON_BASIC_INFO_CHANGE, REASON_CLOSE, REASON_DATA_EXTEND,
    REASON_DATA_OVERWRITE, REASON_DATA_TRUNCATION, REASON_FILE_DELETE, REASON_RENAME_OLD_NAME,
};
use rusqlite::{Connection, OptionalExtension, Result, types::Value};

pub fn get_database_path() -> std::io::Result<std::path::PathBuf> {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE")
    } else {
        std::env::var_os("HOME")
    };

    let home = home.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "User home directory not found",
        )
    })?;

    Ok(std::path::PathBuf::from(home).join(".everything-rust.db"))
}

pub fn open_database(path: &str) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    initialize_database(&connection)?;
    Ok(connection)
}

fn initialize_database(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS entries (
            path TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            kind INTEGER NOT NULL,
            size INTEGER,
            item_count INTEGER,
            last_seen INTEGER NOT NULL DEFAULT 0,
            volume_id TEXT
        );

        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;

    // Migrate databases created before last_seen was introduced.
    let has_last_seen = {
        let mut statement = connection.prepare("PRAGMA table_info(entries)")?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;

        let mut found = false;

        for column in columns {
            if column? == "last_seen" {
                found = true;
                break;
            }
        }

        found
    };

    if !has_last_seen {
        connection.execute(
            "ALTER TABLE entries
             ADD COLUMN last_seen INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }

    let has_volume_id = {
        let mut statement = connection.prepare("PRAGMA table_info(entries)")?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
        let mut found = false;
        for column in columns {
            if column? == "volume_id" {
                found = true;
                break;
            }
        }
        found
    };
    if !has_volume_id {
        connection.execute("ALTER TABLE entries ADD COLUMN volume_id TEXT", [])?;
    }

    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS volume_sync (
            volume_id TEXT PRIMARY KEY,
            journal_id TEXT NOT NULL,
            next_usn INTEGER NOT NULL,
            root_path TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS ntfs_files (
            volume_id TEXT NOT NULL,
            file_reference TEXT NOT NULL,
            parent_reference TEXT NOT NULL,
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            kind INTEGER NOT NULL,
            size INTEGER,
            PRIMARY KEY (volume_id, file_reference)
        );

        CREATE TABLE IF NOT EXISTS ntfs_staging (
            volume_id TEXT NOT NULL,
            file_reference TEXT NOT NULL,
            parent_reference TEXT NOT NULL,
            name TEXT NOT NULL,
            kind INTEGER NOT NULL,
            size INTEGER,
            PRIMARY KEY (volume_id, file_reference)
        );

        CREATE INDEX IF NOT EXISTS entries_name_nocase_idx
            ON entries(name COLLATE NOCASE);
        CREATE INDEX IF NOT EXISTS entries_kind_name_idx
            ON entries(kind, name COLLATE NOCASE);
        CREATE INDEX IF NOT EXISTS entries_volume_id_idx
            ON entries(volume_id);
        CREATE INDEX IF NOT EXISTS volume_sync_root_idx
            ON volume_sync(root_path);
        CREATE INDEX IF NOT EXISTS ntfs_files_parent_idx
            ON ntfs_files(volume_id, parent_reference);
        CREATE INDEX IF NOT EXISTS ntfs_staging_parent_idx
            ON ntfs_staging(volume_id, parent_reference);",
    )?;

    Ok(())
}

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

pub fn search_entries(connection: &Connection, query: &str) -> Result<Vec<FsEntry>> {
    use crate::search::parse_query;

    let parsed = parse_query(query);
    let text = parsed.text.replace('\\', "/");

    let escape_like = |value: &str| {
        value
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_")
    };

    let contains_pattern = format!("%{}%", escape_like(&text));
    let prefix_pattern = format!("{}%", escape_like(&text));

    let mut sql = String::from(
        "SELECT path, name, kind, size, item_count
         FROM entries
         WHERE 1 = 1",
    );

    let mut params: Vec<Value> = Vec::new();

    if let Some(kind) = parsed.kind {
        let kind_value = match kind {
            EntryKind::File => 0,
            EntryKind::Directory => 1,
        };

        sql.push_str(" AND kind = ?");
        params.push(Value::Integer(kind_value));
    }

    if let Some(extension) = parsed.extension {
        let extension = escape_like(&extension.to_lowercase());

        sql.push_str(" AND name LIKE ? ESCAPE '!'");
        params.push(Value::Text(format!("%.{}", extension)));
    }

    if !text.is_empty() {
        sql.push_str(
            " AND (
                name LIKE ? ESCAPE '!'
                OR LOWER(REPLACE(path, char(92), '/')) LIKE ? ESCAPE '!'
            )",
        );

        params.push(Value::Text(contains_pattern.clone()));
        params.push(Value::Text(contains_pattern.clone()));

        sql.push_str(
            " ORDER BY
                CASE
                    WHEN name = ? COLLATE NOCASE THEN 0
                    WHEN name LIKE ? ESCAPE '!' THEN 1
                    WHEN name LIKE ? ESCAPE '!' THEN 2
                    ELSE 3
                END,
                name COLLATE NOCASE ASC",
        );

        params.push(Value::Text(text));
        params.push(Value::Text(prefix_pattern));
        params.push(Value::Text(contains_pattern));
    } else {
        sql.push_str(" ORDER BY name COLLATE NOCASE ASC");
    }

    sql.push_str(" LIMIT 100");

    let mut statement = connection.prepare_cached(&sql)?;
    let entries = statement.query_map(rusqlite::params_from_iter(params.iter()), row_to_entry)?;

    entries.collect()
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<FsEntry> {
    let path: String = row.get(0)?;
    let name: String = row.get(1)?;
    let kind: i64 = row.get(2)?;
    let size: Option<i64> = row.get(3)?;
    let item_count: Option<i64> = row.get(4)?;

    let kind = match kind {
        0 => EntryKind::File,
        1 => EntryKind::Directory,
        _ => {
            return Err(rusqlite::Error::InvalidColumnType(
                2,
                "kind".to_string(),
                rusqlite::types::Type::Integer,
            ));
        }
    };

    let size = size.map(u64::try_from).transpose().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::other("Invalid file size")),
        )
    })?;

    let item_count = item_count.map(usize::try_from).transpose().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::other("Invalid item count")),
        )
    })?;

    Ok(FsEntry {
        path: PathBuf::from(path),
        name,
        kind,
        size,
        item_count,
    })
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeSyncState {
    pub journal_id: String,
    pub next_usn: i64,
    pub root_path: String,
}

pub fn load_volume_sync_state(
    connection: &Connection,
    volume_id: &str,
) -> Result<Option<VolumeSyncState>> {
    connection
        .query_row(
            "SELECT journal_id, next_usn, root_path
             FROM volume_sync WHERE volume_id = ?1",
            [volume_id],
            |row| {
                Ok(VolumeSyncState {
                    journal_id: row.get(0)?,
                    next_usn: row.get(1)?,
                    root_path: row.get(2)?,
                })
            },
        )
        .optional()
}

pub fn begin_volume_rebuild(connection: &Connection, volume_id: &str) -> Result<()> {
    connection.execute("DELETE FROM ntfs_staging WHERE volume_id = ?1", [volume_id])?;
    Ok(())
}

pub fn invalidate_volume_sync_for_root(
    connection: &mut Connection,
    root_path: &std::path::Path,
) -> Result<()> {
    let root_path = root_path.to_string_lossy();
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM ntfs_files
         WHERE volume_id IN (SELECT volume_id FROM volume_sync WHERE root_path = ?1)",
        [&*root_path],
    )?;
    transaction.execute(
        "DELETE FROM ntfs_staging
         WHERE volume_id IN (SELECT volume_id FROM volume_sync WHERE root_path = ?1)",
        [&*root_path],
    )?;
    transaction.execute(
        "DELETE FROM volume_sync WHERE root_path = ?1",
        [&*root_path],
    )?;
    transaction.commit()
}

pub fn stage_mft_records(
    connection: &mut Connection,
    volume_id: &str,
    records: &[JournalRecord],
) -> Result<()> {
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare_cached(
            "INSERT INTO ntfs_staging (
                volume_id, file_reference, parent_reference, name, kind, size
            ) VALUES (?1, ?2, ?3, ?4, ?5, NULL)
            ON CONFLICT(volume_id, file_reference) DO UPDATE SET
                parent_reference = excluded.parent_reference,
                name = excluded.name,
                kind = excluded.kind",
        )?;

        for record in records {
            statement.execute(rusqlite::params![
                volume_id,
                record.file_reference_number.to_string(),
                record.parent_file_reference_number.to_string(),
                record.name,
                i64::from(record.attributes & ATTRIBUTE_DIRECTORY != 0)
            ])?;
        }
    }
    transaction.commit()
}

pub fn finish_volume_rebuild(
    connection: &mut Connection,
    volume_id: &str,
    root_path: &str,
    journal_id: &str,
    next_usn: i64,
) -> Result<()> {
    let transaction = connection.transaction()?;
    let root_prefix = if root_path.ends_with(['\\', '/']) {
        root_path.to_owned()
    } else {
        format!("{root_path}\\")
    };
    transaction.execute_batch("DROP TABLE IF EXISTS temp.usn_resolved;")?;
    transaction.execute(
        "CREATE TEMP TABLE usn_resolved AS
         WITH RECURSIVE resolved(
             file_reference, parent_reference, name, path, kind, size, visited, depth
         ) AS (
             SELECT file_reference, parent_reference, name, ?2, kind, size,
                    '/' || file_reference || '/', 0
             FROM ntfs_staging
             WHERE volume_id = ?1 AND file_reference = parent_reference
             UNION ALL
             SELECT child.file_reference, child.parent_reference, child.name,
                    resolved.path ||
                    CASE WHEN substr(resolved.path, -1) = char(92) THEN '' ELSE char(92) END ||
                    child.name,
                    child.kind, child.size,
                    resolved.visited || child.file_reference || '/',
                    resolved.depth + 1
             FROM ntfs_staging child
             JOIN resolved ON child.parent_reference = resolved.file_reference
             WHERE child.volume_id = ?1
               AND child.file_reference != child.parent_reference
               AND resolved.depth < 4096
               AND instr(resolved.visited, '/' || child.file_reference || '/') = 0
         )
         SELECT file_reference, parent_reference, name, path, kind, size
         FROM resolved",
        rusqlite::params![volume_id, root_path],
    )?;

    let staging_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM ntfs_staging WHERE volume_id = ?1",
        [volume_id],
        |row| row.get(0),
    )?;
    let resolved_count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM usn_resolved", [], |row| row.get(0))?;
    if staging_count == 0 || staging_count != resolved_count {
        return Err(rusqlite::Error::InvalidQuery);
    }

    transaction.execute(
        "DELETE FROM ntfs_files
         WHERE volume_id IN (
             SELECT volume_id FROM volume_sync
             WHERE root_path = ?2 AND volume_id != ?1
         )",
        rusqlite::params![volume_id, root_path],
    )?;
    transaction.execute(
        "DELETE FROM ntfs_staging
         WHERE volume_id IN (
             SELECT volume_id FROM volume_sync
             WHERE root_path = ?2 AND volume_id != ?1
         )",
        rusqlite::params![volume_id, root_path],
    )?;
    transaction.execute(
        "DELETE FROM volume_sync
         WHERE root_path = ?2 AND volume_id != ?1",
        rusqlite::params![volume_id, root_path],
    )?;
    transaction.execute(
        "DELETE FROM entries
         WHERE volume_id = ?1
            OR path = ?2
            OR substr(path, 1, length(?3)) = ?3",
        rusqlite::params![volume_id, root_path, root_prefix],
    )?;
    transaction.execute("DELETE FROM ntfs_files WHERE volume_id = ?1", [volume_id])?;
    transaction.execute(
        "INSERT INTO ntfs_files (
             volume_id, file_reference, parent_reference, name, path, kind, size
         )
         SELECT ?1, file_reference, parent_reference, name, path, kind, size
         FROM usn_resolved",
        [volume_id],
    )?;
    transaction.execute(
        "INSERT INTO entries (path, name, kind, size, item_count, last_seen, volume_id)
         SELECT path, name, kind, size, NULL, 0, ?1 FROM usn_resolved
         WHERE path != ?2
         ON CONFLICT(path) DO UPDATE SET
             name = excluded.name,
             kind = excluded.kind,
             size = excluded.size,
             item_count = NULL,
             volume_id = excluded.volume_id",
        rusqlite::params![volume_id, root_path],
    )?;
    transaction.execute(
        "INSERT INTO volume_sync (volume_id, journal_id, next_usn, root_path)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(volume_id) DO UPDATE SET
             journal_id = excluded.journal_id,
             next_usn = excluded.next_usn,
             root_path = excluded.root_path",
        rusqlite::params![volume_id, journal_id, next_usn, root_path],
    )?;
    transaction.execute("DELETE FROM ntfs_staging WHERE volume_id = ?1", [volume_id])?;
    transaction.execute_batch("DROP TABLE temp.usn_resolved;")?;
    transaction.commit()
}

pub fn apply_usn_batch(
    connection: &mut Connection,
    volume_id: &str,
    root_path: &str,
    journal_id: &str,
    start_usn: i64,
    next_usn: i64,
    records: &[JournalRecord],
) -> Result<()> {
    if start_usn < 0 || next_usn < start_usn {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let transaction = connection.transaction()?;
    let state: VolumeSyncState = transaction.query_row(
        "SELECT journal_id, next_usn, root_path
         FROM volume_sync WHERE volume_id = ?1",
        [volume_id],
        |row| {
            Ok(VolumeSyncState {
                journal_id: row.get(0)?,
                next_usn: row.get(1)?,
                root_path: row.get(2)?,
            })
        },
    )?;
    if state.journal_id != journal_id || state.next_usn != start_usn || state.root_path != root_path
    {
        return Err(rusqlite::Error::InvalidQuery);
    }

    let mut previous_usn = start_usn;
    for record in records {
        if record.usn < previous_usn || record.usn >= next_usn {
            return Err(rusqlite::Error::InvalidQuery);
        }
        previous_usn = record.usn;
        apply_usn_record(&transaction, volume_id, record)?;
    }

    transaction.execute(
        "UPDATE volume_sync SET next_usn = ?2 WHERE volume_id = ?1",
        rusqlite::params![volume_id, next_usn],
    )?;
    transaction.commit()
}

fn apply_usn_record(
    transaction: &rusqlite::Transaction<'_>,
    volume_id: &str,
    record: &JournalRecord,
) -> Result<()> {
    let file_reference = record.file_reference_number.to_string();
    let parent_reference = record.parent_file_reference_number.to_string();
    let old_path: Option<String> = transaction
        .query_row(
            "SELECT path FROM ntfs_files
             WHERE volume_id = ?1 AND file_reference = ?2",
            rusqlite::params![volume_id, file_reference],
            |row| row.get(0),
        )
        .optional()?;

    if record.reason & REASON_RENAME_OLD_NAME != 0
        && record.reason & crate::journal::REASON_RENAME_NEW_NAME == 0
    {
        return Ok(());
    }

    if record.reason & REASON_FILE_DELETE != 0 {
        let deleted_path = if let Some(old_path) = old_path.as_deref() {
            old_path.to_owned()
        } else {
            let parent_path: String = transaction.query_row(
                "SELECT path FROM ntfs_files
                 WHERE volume_id = ?1 AND file_reference = ?2",
                rusqlite::params![volume_id, parent_reference],
                |row| row.get(0),
            )?;
            let separator = if parent_path.ends_with(['\\', '/']) {
                ""
            } else {
                "\\"
            };
            format!("{parent_path}{separator}{}", record.name)
        };
        let subtree_prefix = format!("{deleted_path}\\");
        transaction.execute(
            "DELETE FROM entries
             WHERE volume_id = ?1
               AND (path = ?2 OR substr(path, 1, length(?3)) = ?3)",
            rusqlite::params![volume_id, deleted_path, subtree_prefix],
        )?;
        transaction.execute(
            "DELETE FROM ntfs_files
             WHERE volume_id = ?1 AND file_reference = ?2",
            rusqlite::params![volume_id, file_reference],
        )?;
        return Ok(());
    }

    let parent_path: String = transaction.query_row(
        "SELECT path FROM ntfs_files
         WHERE volume_id = ?1 AND file_reference = ?2",
        rusqlite::params![volume_id, parent_reference],
        |row| row.get(0),
    )?;
    let separator = if parent_path.ends_with(['\\', '/']) {
        ""
    } else {
        "\\"
    };
    let path = format!("{parent_path}{separator}{}", record.name);

    if let Some(old_path) = old_path.as_deref().filter(|old| *old != path) {
        let old_prefix = format!("{old_path}\\");
        let new_prefix = format!("{path}\\");
        transaction.execute(
            "UPDATE entries
             SET path = ?2 || substr(path, length(?3) + 1)
             WHERE volume_id = ?1
               AND substr(path, 1, length(?3)) = ?3",
            rusqlite::params![volume_id, new_prefix, old_prefix],
        )?;
        transaction.execute(
            "UPDATE ntfs_files
             SET path = ?2 || substr(path, length(?3) + 1)
             WHERE volume_id = ?1
               AND substr(path, 1, length(?3)) = ?3",
            rusqlite::params![volume_id, new_prefix, old_prefix],
        )?;
        transaction.execute(
            "UPDATE entries SET path = ?2 WHERE volume_id = ?1 AND path = ?3",
            rusqlite::params![volume_id, path, old_path],
        )?;
        transaction.execute(
            "UPDATE ntfs_files SET path = ?2 WHERE volume_id = ?1 AND file_reference = ?3",
            rusqlite::params![volume_id, path, file_reference],
        )?;
    }

    let kind = i64::from(record.attributes & ATTRIBUTE_DIRECTORY != 0);
    let size_changed = record.reason
        & (REASON_DATA_OVERWRITE
            | REASON_DATA_EXTEND
            | REASON_DATA_TRUNCATION
            | REASON_BASIC_INFO_CHANGE
            | REASON_CLOSE)
        != 0;
    let size = if kind == 0 && (old_path.is_none() || size_changed) {
        std::fs::metadata(&path)
            .ok()
            .filter(|metadata| metadata.is_file())
            .and_then(|metadata| i64::try_from(metadata.len()).ok())
    } else {
        None
    };

    transaction.execute(
        "INSERT INTO ntfs_files (
             volume_id, file_reference, parent_reference, name, path, kind, size
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(volume_id, file_reference) DO UPDATE SET
             parent_reference = excluded.parent_reference,
             name = excluded.name,
             path = excluded.path,
             kind = excluded.kind,
             size = COALESCE(excluded.size, ntfs_files.size)",
        rusqlite::params![
            volume_id,
            file_reference,
            parent_reference,
            record.name,
            path,
            kind,
            size
        ],
    )?;
    transaction.execute(
        "INSERT INTO entries (path, name, kind, size, item_count, last_seen, volume_id)
         VALUES (?1, ?2, ?3, ?4, NULL, 0, ?5)
         ON CONFLICT(path) DO UPDATE SET
             name = excluded.name,
             kind = excluded.kind,
             size = COALESCE(excluded.size, entries.size),
             volume_id = excluded.volume_id",
        rusqlite::params![path, record.name, kind, size, volume_id],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{REASON_RENAME_NEW_NAME, REASON_RENAME_OLD_NAME};

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
                 WHERE type = 'table' AND name IN ('volume_sync', 'ntfs_files', 'ntfs_staging')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 3);
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
}
