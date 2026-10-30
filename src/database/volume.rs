use crate::journal::{
    ATTRIBUTE_DIRECTORY, JournalRecord, REASON_BASIC_INFO_CHANGE, REASON_CLOSE, REASON_DATA_EXTEND,
    REASON_DATA_OVERWRITE, REASON_DATA_TRUNCATION, REASON_FILE_DELETE, REASON_RENAME_OLD_NAME,
};
use rusqlite::{Connection, OptionalExtension, Result};

const RESOLVE_MFT_QUERY: &str = "
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
        FROM resolved
        CROSS JOIN ntfs_staging child
        WHERE child.volume_id = ?1
          AND child.parent_reference = resolved.file_reference
          AND child.file_reference != child.parent_reference
          AND resolved.depth < 4096
          AND instr(resolved.visited, '/' || child.file_reference || '/') = 0
    )
    SELECT file_reference, parent_reference, name, path, kind, size
    FROM resolved";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeSyncState {
    pub journal_id: String,
    pub next_usn: i64,
    pub root_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeRebuildCheckpoint {
    pub root_path: String,
    pub journal_id: String,
    pub next_usn: i64,
    pub next_file_reference: String,
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
    let transaction = connection.unchecked_transaction()?;
    transaction.execute("DELETE FROM ntfs_staging WHERE volume_id = ?1", [volume_id])?;
    transaction.execute(
        "DELETE FROM volume_rebuild WHERE volume_id = ?1",
        [volume_id],
    )?;
    transaction.execute_batch(
        "CREATE INDEX IF NOT EXISTS ntfs_staging_parent_idx
         ON ntfs_staging(volume_id, parent_reference)",
    )?;
    transaction.commit()?;
    Ok(())
}

pub fn load_volume_rebuild_checkpoint(
    connection: &Connection,
    volume_id: &str,
) -> Result<Option<VolumeRebuildCheckpoint>> {
    connection
        .query_row(
            "SELECT root_path, journal_id, next_usn, next_file_reference
             FROM volume_rebuild WHERE volume_id = ?1",
            [volume_id],
            |row| {
                Ok(VolumeRebuildCheckpoint {
                    root_path: row.get(0)?,
                    journal_id: row.get(1)?,
                    next_usn: row.get(2)?,
                    next_file_reference: row.get(3)?,
                })
            },
        )
        .optional()
}

pub fn start_volume_rebuild(
    connection: &mut Connection,
    volume_id: &str,
    root_path: &str,
    journal_id: &str,
    next_usn: i64,
) -> Result<()> {
    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM ntfs_staging WHERE volume_id = ?1", [volume_id])?;
    transaction.execute(
        "INSERT INTO volume_rebuild (
            volume_id, root_path, journal_id, next_usn, next_file_reference
         ) VALUES (?1, ?2, ?3, ?4, '0')
         ON CONFLICT(volume_id) DO UPDATE SET
            root_path = excluded.root_path,
            journal_id = excluded.journal_id,
            next_usn = excluded.next_usn,
            next_file_reference = excluded.next_file_reference",
        rusqlite::params![volume_id, root_path, journal_id, next_usn],
    )?;
    transaction.execute_batch("DROP INDEX IF EXISTS ntfs_staging_parent_idx")?;
    transaction.commit()
}

pub fn resume_volume_rebuild(connection: &mut Connection) -> Result<()> {
    connection.execute_batch("DROP INDEX IF EXISTS ntfs_staging_parent_idx")
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
        "DELETE FROM volume_rebuild
         WHERE volume_id IN (SELECT volume_id FROM volume_sync WHERE root_path = ?1)",
        [&*root_path],
    )?;
    transaction.execute(
        "DELETE FROM volume_sync WHERE root_path = ?1",
        [&*root_path],
    )?;
    transaction.commit()
}

#[cfg(test)]
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

pub fn stage_mft_page(
    connection: &mut Connection,
    volume_id: &str,
    records: &[JournalRecord],
    next_file_reference: u64,
) -> Result<()> {
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare_cached(
            "INSERT INTO ntfs_staging (
                volume_id, file_reference, parent_reference, name, kind, size
            ) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
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
    let checkpoint_updated = transaction.execute(
        "UPDATE volume_rebuild SET next_file_reference = ?2 WHERE volume_id = ?1",
        rusqlite::params![volume_id, next_file_reference.to_string()],
    )?;
    if checkpoint_updated != 1 {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "MFT page has no persisted rebuild checkpoint",
            ),
        )));
    }
    transaction.commit()
}

pub fn finish_volume_rebuild(
    connection: &mut Connection,
    volume_id: &str,
    root_path: &str,
    journal_id: &str,
    next_usn: i64,
) -> Result<usize> {
    let transaction = connection.transaction()?;
    let root_prefix = if root_path.ends_with(['\\', '/']) {
        root_path.to_owned()
    } else {
        format!("{root_path}\\")
    };
    transaction.execute_batch(
        "CREATE INDEX IF NOT EXISTS ntfs_staging_parent_idx
         ON ntfs_staging(volume_id, parent_reference)",
    )?;
    transaction.execute_batch("DROP TABLE IF EXISTS temp.usn_resolved;")?;
    transaction.execute(
        &format!("CREATE TEMP TABLE usn_resolved AS {RESOLVE_MFT_QUERY}"),
        rusqlite::params![volume_id, root_path],
    )?;

    let staging_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM ntfs_staging WHERE volume_id = ?1",
        [volume_id],
        |row| row.get(0),
    )?;
    let resolved_count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM usn_resolved", [], |row| row.get(0))?;
    let root_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM usn_resolved
         WHERE file_reference = parent_reference AND path = ?1",
        [root_path],
        |row| row.get(0),
    )?;
    if staging_count == 0 || root_count != 1 || resolved_count > staging_count {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "MFT path resolution failed: {staging_count} records staged, \
                     {resolved_count} resolved, {root_count} volume roots found"
                ),
            ),
        )));
    }
    let skipped_records = usize::try_from(staging_count - resolved_count).map_err(|error| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(error)))
    })?;

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
    transaction.execute(
        "DELETE FROM volume_rebuild WHERE volume_id = ?1",
        [volume_id],
    )?;
    transaction.execute_batch("DROP TABLE temp.usn_resolved;")?;
    transaction.commit()?;
    Ok(skipped_records)
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
             WHERE volume_id = ?1
               AND (path = ?2 OR substr(path, 1, length(?3)) = ?3)",
            rusqlite::params![volume_id, deleted_path, subtree_prefix],
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
