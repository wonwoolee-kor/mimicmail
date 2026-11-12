pub mod archive;
pub mod context;
pub mod db;
pub mod drafting;
pub mod llm;
pub mod mail;
pub mod mailbox;
pub mod pipeline;
pub mod privacy;
pub mod search;
pub mod server;
pub mod settings;
pub mod work;

use serde_json::Value;

pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
pub fn select(value: &Value, keys: &[&str]) -> Value {
    keys.iter()
        .map(|key| ((*key).to_owned(), value[*key].clone()))
        .collect()
}
pub fn sha256(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}

#[derive(Debug)]
pub struct Fault(pub u16, pub String);
impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.1.fmt(f)
    }
}
impl std::error::Error for Fault {}
pub fn fault(code: u16, message: &str) -> anyhow::Error {
    Fault(code, message.to_owned()).into()
}
