use std::path::PathBuf;

use crate::entry::{EntryKind, FsEntry};
use rusqlite::{Connection, Result, types::Value};

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
