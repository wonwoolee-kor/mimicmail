//! Persistent general context shared by CLI and HTTP callers.
use crate::{db, fault, privacy, text};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::HashSet;

pub fn get(db: &Connection) -> Result<Value> {
    // Read-only access may precede migration.
    if db::exists(
        db,
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='general_context'",
        [],
    )? && let Some(row) = db::one(
        db,
        "SELECT entries_json,revision FROM general_context WHERE id=1",
        [],
    )? {
        let entries: Value = serde_json::from_str(text(&row, "entries_json"))?;
        return Ok(json!({"entries":entries,"revision":row["revision"]}));
    }
    Ok(json!({"entries":[],"revision":0}))
}
pub fn update(db: &Connection, payload: &Value) -> Result<Value> {
    let version = payload["revision"]
        .as_i64()
        .ok_or_else(|| fault(400, "설정을 다시 연 뒤 저장하세요."))?;
    let entries = payload["entries"]
        .as_array()
        .ok_or_else(|| fault(400, "키와 값을 확인하세요."))?;
    let mut normalized = Vec::new();
    let mut keys = HashSet::new();
    for entry in entries {
        let key = privacy::redact(
            entry["key"]
                .as_str()
                .ok_or_else(|| fault(400, "키를 입력하세요."))?
                .trim(),
        );
        let value = privacy::redact(
            entry["value"]
                .as_str()
                .ok_or_else(|| fault(400, "값을 입력하세요."))?
                .trim(),
        );
        if key.is_empty() || key.chars().count() > 100 {
            return Err(fault(400, "키는 1~100자로 입력하세요."));
        }
        if value.is_empty() || value.chars().count() > 10000 {
            return Err(fault(400, "값은 1~10,000자로 입력하세요."));
        }
        if !keys.insert(key.clone()) {
            return Err(fault(400, "중복된 키를 수정하거나 삭제하세요."));
        }
        normalized.push(json!({"key":key,"value":value}));
    }
    let tx = db::immediate(db)?;
    if get(&tx)?["revision"] != version {
        return Err(fault(
            409,
            "다른 창에서 변경된 설정입니다. 설정을 다시 연 뒤 저장하세요.",
        ));
    }
    tx.execute("INSERT INTO general_context(id,entries_json,revision) VALUES (1,?,1) ON CONFLICT(id) DO UPDATE SET entries_json=excluded.entries_json,revision=general_context.revision+1", [json!(normalized).to_string()])?;
    tx.commit()?;
    Ok(json!({"entries":normalized,"revision":version+1}))
}
