use anyhow::Result;
use rusqlite::{Connection, OpenFlags, Params, types::ValueRef};
use serde_json::{Map, Value};
use std::{path::Path, time::Duration};

pub fn open(path: &Path, readonly: bool) -> Result<Connection> {
    let flags = if readonly {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    };
    let db = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    db.busy_timeout(Duration::from_secs(2))?;
    db.set_prepared_statement_cache_capacity(16);
    if !readonly {
        db.pragma_update(None, "secure_delete", true)?;
    }
    Ok(db)
}
pub fn connect(path: &Path) -> Result<Connection> {
    let db = open(path, false)?;
    initialize(&db)?;
    Ok(db)
}
pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("../assets/schema.sql"))?;
    if !rows(db, "PRAGMA table_info(mail_archive)", [])?
        .iter()
        .any(|r| r["name"] == "context_json")
    {
        db.execute(
            "ALTER TABLE mail_archive ADD COLUMN context_json TEXT NOT NULL DEFAULT '{}'",
            [],
        )?;
    }
    crate::privacy::migrate(db)
}
pub fn row_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut result = Map::new();
    for (index, name) in row.as_ref().column_names().iter().enumerate() {
        let value = match row.get_ref(index)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(n) => n.into(),
            ValueRef::Real(n) => n.into(),
            ValueRef::Text(s) => String::from_utf8_lossy(s).into_owned().into(),
            ValueRef::Blob(_) => Value::Null,
        };
        result.insert((*name).to_owned(), value);
    }
    Ok(Value::Object(result))
}
pub fn rows(db: &Connection, sql: &str, params: impl Params) -> Result<Vec<Value>> {
    Ok(db
        .prepare_cached(sql)?
        .query_map(params, row_value)?
        .collect::<rusqlite::Result<_>>()?)
}
pub fn one(db: &Connection, sql: &str, params: impl Params) -> Result<Option<Value>> {
    let mut statement = db.prepare_cached(sql)?;
    let mut rows = statement.query(params)?;
    rows.next()?.map(row_value).transpose().map_err(Into::into)
}
pub fn exists(db: &Connection, sql: &str, params: impl Params) -> Result<bool> {
    Ok(db.prepare_cached(sql)?.exists(params)?)
}
