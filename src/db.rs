use anyhow::Result;
use rusqlite::{
    Connection, OpenFlags, Params, Transaction, TransactionBehavior, params, types::ValueRef,
};
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
pub(crate) fn immediate(db: &Connection) -> Result<Transaction<'_>> {
    Ok(Transaction::new_unchecked(
        db,
        TransactionBehavior::Immediate,
    )?)
}
pub fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("../sql/schema.sql"))?;
    if !rows(db, "PRAGMA table_info(mail_archive)", [])?
        .iter()
        .any(|r| r["name"] == "context_json")
    {
        db.execute(
            "ALTER TABLE mail_archive ADD COLUMN context_json TEXT NOT NULL DEFAULT '{}'",
            [],
        )?;
    }
    if rows(db, "PRAGMA table_info(received_replies)", [])?
        .iter()
        .any(|r| r["name"] == "sent_at")
    {
        let tx =
            rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch("ALTER TABLE received_replies DROP COLUMN sent_at;")?;
        if exists(
            &tx,
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='browse_meta'",
            [],
        )? {
            tx.execute_batch(
                "DELETE FROM browse_meta; DELETE FROM browse_items; DELETE FROM browse_contacts;",
            )?;
        }
        tx.commit()?;
    }
    crate::privacy::migrate(db)?;
    preserve_mail_ids(db)?;
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    for deleted in rows(
        &tx,
        "SELECT owner,substr(message_key,14) AS recipient FROM deleted_messages WHERE message_key LIKE 'conversation:%'",
        [],
    )? {
        purge_conversation(
            &tx,
            crate::text(&deleted, "owner"),
            crate::text(&deleted, "recipient"),
        )?;
    }
    tx.commit()?;
    Ok(())
}

// Physical deletion must not recycle IDs that an older editor or deletion key still uses.
fn preserve_mail_ids(db: &Connection) -> Result<()> {
    let tables = rows(
        db,
        "SELECT name,sql FROM sqlite_master WHERE type='table' AND name IN ('mail_archive','received_replies') AND instr(upper(sql),'AUTOINCREMENT')=0",
        [],
    )?;
    if tables.is_empty() {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    let cached = exists(
        &tx,
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='browse_meta'",
        [],
    )?;
    for trigger in rows(
        &tx,
        "SELECT name FROM sqlite_master WHERE type='trigger' AND name GLOB 'browse_*'",
        [],
    )? {
        tx.execute_batch(&format!(
            "DROP TRIGGER \"{}\"",
            crate::text(&trigger, "name").replace('"', "\"\"")
        ))?;
    }
    for table in tables {
        let name = crate::text(&table, "name");
        let sql = crate::text(&table, "sql");
        let definition = sql[sql.find('(').unwrap()..].replacen(
            "INTEGER PRIMARY KEY",
            "INTEGER PRIMARY KEY AUTOINCREMENT",
            1,
        );
        tx.execute_batch(&format!("CREATE TABLE {name}_monotonic {definition}; INSERT INTO {name}_monotonic SELECT * FROM {name}; DROP TABLE {name}; ALTER TABLE {name}_monotonic RENAME TO {name};"))?;
    }
    tx.execute_batch(include_str!("../sql/schema.sql"))?;
    if cached {
        tx.execute("DELETE FROM browse_meta", [])?;
    }
    tx.commit()?;
    if cached {
        crate::mailbox::initialize(db)?;
    }
    Ok(())
}

pub(crate) fn purge_conversation(db: &Connection, owner: &str, recipient: &str) -> Result<()> {
    let mail = rows(
        db,
        "SELECT fingerprint FROM sent_mail WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?;
    for table in [
        "sent_mail",
        "received_replies",
        "contact_labels",
        "manual_conversations",
    ] {
        db.execute(
            &format!("DELETE FROM {table} WHERE owner=? AND recipient=?"),
            params![owner, recipient],
        )?;
    }
    for item in mail {
        db.execute("DELETE FROM mail_archive WHERE owner=?1 AND fingerprint=?2 AND NOT EXISTS (SELECT 1 FROM sent_mail WHERE owner=?1 AND fingerprint=?2)", params![owner,crate::text(&item,"fingerprint")])?;
    }
    if exists(
        db,
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='browse_meta'",
        [],
    )? {
        for table in ["browse_items", "browse_contacts", "browse_dirty"] {
            db.execute(
                &format!("DELETE FROM {table} WHERE owner=? AND recipient=?"),
                params![owner, recipient],
            )?;
        }
    }
    Ok(())
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
