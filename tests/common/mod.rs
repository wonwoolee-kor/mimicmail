#![allow(dead_code)]
use anyhow::Result;
use mimicmail::{db, llm::LanguageModel};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::VecDeque;

pub fn unknown() -> Value {
    json!({"purpose":"request","role":"unknown","role_evidence":"","relationship":"unknown","relationship_evidence":""})
}
pub struct Fake {
    pub results: VecDeque<Value>,
    pub calls: Vec<(String, Value, Value)>,
}
impl Fake {
    pub fn new(results: Vec<Value>) -> Self {
        Self {
            results: results.into(),
            calls: vec![],
        }
    }
}
impl LanguageModel for Fake {
    fn complete(&mut self, instructions: &str, data: &Value, schema: &Value) -> Result<Value> {
        self.calls
            .push((instructions.to_owned(), data.clone(), schema.clone()));
        self.results
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("unexpected model call"))
    }
}
pub fn memory() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db::initialize(&db).unwrap();
    db
}
pub fn load_tables(db: &Connection, tables: &Value) {
    for (table, rows) in tables.as_object().unwrap() {
        for row in rows.as_array().unwrap() {
            let columns = row.as_object().unwrap();
            let sql = format!(
                "INSERT INTO {} ({}) VALUES ({})",
                table,
                columns.keys().cloned().collect::<Vec<_>>().join(","),
                vec!["?"; columns.len()].join(",")
            );
            let values: Vec<rusqlite::types::Value> = columns
                .values()
                .map(|v| match v {
                    Value::Null => rusqlite::types::Value::Null,
                    Value::Number(n) => n.as_i64().unwrap().into(),
                    Value::String(s) => s.clone().into(),
                    _ => panic!("invalid fixture"),
                })
                .collect();
            db.execute(&sql, rusqlite::params_from_iter(values))
                .unwrap();
        }
    }
}
pub fn legacy() -> Value {
    serde_json::from_str(include_str!("../fixtures/legacy.json")).unwrap()
}
pub fn populated() -> Connection {
    let db = memory();
    load_tables(&db, &legacy()["tables"]);
    db
}
pub fn raw(sender: &str, to: &str, body: &str, date: &str) -> Vec<u8> {
    format!("From: {sender}\nTo: {to}\nSubject: 검토 요청\nMessage-ID: <test@example.com>\nDate: {date}\nContent-Type: text/plain; charset=utf-8\n\n{body}").into_bytes()
}
pub fn clean(value: &mut Value) {
    match value {
        Value::Object(m) => {
            m.remove("updated_at");
            for v in m.values_mut() {
                clean(v);
            }
        }
        Value::Array(a) => {
            for v in a {
                clean(v);
            }
        }
        _ => {}
    }
}
