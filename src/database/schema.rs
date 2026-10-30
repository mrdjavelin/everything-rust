use rusqlite::{Connection, Result};

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

pub(super) fn initialize_database(connection: &Connection) -> Result<()> {
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
