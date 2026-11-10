use crate::{context, db, mail::Mail, sha256, text};
use anyhow::Result;
use regex::Regex;
use rusqlite::{Connection, Transaction, TransactionBehavior, params_from_iter};
use serde_json::{Value, json};
use std::sync::LazyLock;

pub fn person_id(value: &str) -> String {
    let value = value.trim().to_lowercase();
    if value.contains('@') {
        format!("person:{}", sha256(value.as_bytes()))
    } else {
        value
    }
}

pub fn redact(value: &str) -> String {
    static ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)[^\s<>\[\](),;:\"\\@&]+@[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?"#).unwrap()
    });
    ADDRESS.replace_all(value, "[이메일 삭제됨]").into_owned()
}

fn display_name(name: &str, address: &str) -> String {
    let name = if name.trim().is_empty() || name.contains('@') {
        address.split('@').next().unwrap_or("")
    } else {
        name
    };
    if name.is_empty() || name.starts_with("person:") {
        "이름 없음".to_owned()
    } else {
        redact(name)
    }
}

fn redact_json(value: &mut Value) {
    match value {
        Value::String(s) => *s = redact(s),
        Value::Array(items) => items.iter_mut().for_each(redact_json),
        Value::Object(map) => map.values_mut().for_each(redact_json),
        _ => {}
    }
}

pub fn mail_context(value: &Value, recipients: &[String], fingerprint: &str) -> Value {
    let mut result = value.clone();
    let mut names = serde_json::Map::new();
    if let Some(existing) = value["recipient_names"].as_object() {
        for (address, name) in existing {
            names.insert(
                person_id(address),
                json!(display_name(name.as_str().unwrap_or(""), address)),
            );
        }
    }
    for address in recipients {
        names
            .entry(person_id(address))
            .or_insert_with(|| json!(display_name("", address)));
    }
    result["recipient_names"] = json!(names);
    if let Some(quotes) = result["quotes"].as_array_mut() {
        for (index, quote) in quotes.iter_mut().rev().enumerate() {
            let sender = text(quote, "sender").to_owned();
            let stamp = if text(quote, "sent_at").is_empty() {
                context::quote_timestamp(text(quote, "date"))
            } else {
                text(quote, "sent_at").to_owned()
            };
            // Keep deletion keys stable when addresses and quoted bodies are scrubbed.
            if text(quote, "key").is_empty() {
                quote["key"] = json!(context::quote_key(
                    &sender,
                    &stamp,
                    &context::readable(text(quote, "body")),
                    fingerprint,
                    index
                ));
            }
            if !sender.is_empty() {
                quote["sender_name"] = json!(display_name(text(quote, "sender_name"), &sender));
            }
            quote["sender"] = json!(person_id(&sender));
        }
    }
    redact_json(&mut result);
    result
}

pub fn stored_mail(mail: &Mail) -> Mail {
    let recipients = mail.to.iter().chain(&mail.cc).cloned().collect::<Vec<_>>();
    Mail {
        fingerprint: mail.fingerprint.clone(),
        message_id: redact(&mail.message_id),
        sender: person_id(&mail.sender),
        to: mail.to.iter().map(|s| person_id(s)).collect(),
        cc: mail.cc.iter().map(|s| person_id(s)).collect(),
        subject: redact(&mail.subject),
        date: redact(&mail.date),
        body: redact(&mail.body),
        authored_body: redact(&mail.authored_body),
        context: mail_context(&mail.context, &recipients, &mail.fingerprint),
    }
}

pub fn migrate(db: &Connection) -> Result<()> {
    if db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? >= 1 {
        return Ok(());
    }
    db.pragma_update(None, "secure_delete", true)?;
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    for table in [
        "sent_mail",
        "mail_archive",
        "contact_labels",
        "label_colors",
        "deleted_messages",
        "manual_conversations",
    ] {
        for mut row in db::rows(
            &tx,
            &format!("SELECT rowid AS migration_rowid,* FROM {table}"),
            [],
        )? {
            if table == "mail_archive" {
                let to: Vec<String> = serde_json::from_str(text(&row, "to_json"))?;
                let cc: Vec<String> = serde_json::from_str(text(&row, "cc_json"))?;
                let recipients = to.iter().chain(&cc).cloned().collect::<Vec<_>>();
                let context: Value = serde_json::from_str(text(&row, "context_json"))?;
                row["context_json"] = json!(
                    mail_context(&context, &recipients, text(&row, "fingerprint")).to_string()
                );
                row["to_json"] = json!(serde_json::to_string(
                    &to.iter().map(|s| person_id(s)).collect::<Vec<_>>()
                )?);
                row["cc_json"] = json!(serde_json::to_string(
                    &cc.iter().map(|s| person_id(s)).collect::<Vec<_>>()
                )?);
            }
            if table == "manual_conversations" {
                row["name"] = json!(display_name(text(&row, "name"), text(&row, "recipient")));
            }
            let rowid = row["migration_rowid"].as_i64().unwrap();
            let mut columns = Vec::new();
            let mut values = Vec::new();
            for (column, value) in row.as_object().unwrap() {
                if let Some(s) = value.as_str() {
                    columns.push(format!("{column}=?"));
                    values.push(rusqlite::types::Value::Text(
                        if matches!(column.as_str(), "owner" | "sender" | "recipient") {
                            person_id(s)
                        } else {
                            redact(s)
                        },
                    ));
                }
            }
            values.push(rowid.into());
            tx.execute(
                &format!("UPDATE {table} SET {} WHERE rowid=?", columns.join(",")),
                params_from_iter(values),
            )?;
        }
    }
    tx.pragma_update(None, "user_version", 1)?;
    tx.commit()?;
    db.execute_batch("VACUUM")?;
    Ok(())
}
