use crate::{array, mail::address_pairs, sha256, text};
use anyhow::Result;
use chrono::{DateTime, FixedOffset};
use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::LazyLock,
};
use unicode_normalization::UnicodeNormalization;

pub fn readable(value: &str) -> String {
    static BLANK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\n[ \t]*\n(?:[ \t]*\n)+").unwrap());
    let normalized: String = html_escape::decode_html_entities(value)
        .replace('\u{a0}', " ")
        .nfc()
        .collect();
    BLANK.replace_all(&normalized, "\n\n").trim().to_owned()
}
pub fn quote_timestamp(value: &str) -> String {
    static STAMP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d{4}-\d{2}-\d{2})\s*(?:\([^)]*\))?\s*(\d{2}:\d{2}(?::\d{2})?)\s*\(?(?:UTC)?([+-]\d{2}:?\d{2})").unwrap()
    });
    let Some(m) = STAMP.captures(value) else {
        return String::new();
    };
    let time = if m[2].len() == 5 {
        format!("{}:00", &m[2])
    } else {
        m[2].to_owned()
    };
    let offset = m[3].replace(':', "");
    let iso = format!("{}T{}{}:{}", &m[1], time, &offset[..3], &offset[3..]);
    DateTime::parse_from_rfc3339(&iso)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}
pub fn mail_datetime(value: &str) -> Option<DateTime<FixedOffset>> {
    // RFC -0000 and absent zones do not identify an instant.
    static ZONE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:\s[+-]\d{4}|\s(?:UT|UTC|GMT|[ECMP][SD]T))(?:\s*\([^)]*\))?\s*$")
            .unwrap()
    });
    if !ZONE.is_match(value) || value.split_whitespace().any(|s| s == "-0000") {
        return None;
    }
    let stamp = mailparse::dateparse(value).ok()?;
    Some(
        DateTime::from_timestamp(stamp, 0)?
            .with_timezone(&chrono_tz::Asia::Seoul)
            .fixed_offset(),
    )
}
pub fn quoted_messages(body: &str, authored: &str) -> Result<Vec<Value>> {
    static BOUNDARY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)-{2,}\s*(?:Original Message|Forwarded message|원본 메시지|전달된 메시지)\s*-+",
        )
        .unwrap()
    });
    static ON: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^\s*(?:On .+wrote:|.+님이\s*작성:|Begin forwarded message:)\s*$").unwrap()
    });
    static SENDER: LazyLock<FancyRegex> = LazyLock::new(|| {
        FancyRegex::new(r"(?i)(?:^|\n)\s*(?:From|보낸 사람)\s*:\s*(.*?)(?=\s*(?:To|받는 사람|Sent|Date|보낸 날짜)\s*:|\n|$)").unwrap()
    });
    static DATE: LazyLock<FancyRegex> = LazyLock::new(|| {
        FancyRegex::new(r"(?i)(?:Sent|Date|보낸 날짜)\s*:\s*(.*?)(?=\s*(?:Subject|제목)\s*:|\n|$)")
            .unwrap()
    });
    static SUBJECT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*(?:Subject|제목)\s*:\s*([^\n]*)\n").unwrap());
    let value = readable(body);
    let mut chunks: Vec<_> = BOUNDARY.split(&value).skip(1).collect();
    if chunks.is_empty() {
        chunks = ON.split(&value).skip(1).collect();
    }
    let mut quotes = Vec::new();
    for chunk in chunks {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let pairs = SENDER
            .captures(chunk)?
            .and_then(|m| m.get(1).map(|m| address_pairs(m.as_str())))
            .unwrap_or_default();
        let (name, sender) = if pairs.len() == 1 && pairs[0].1.contains('@') {
            pairs[0].clone()
        } else {
            (String::new(), String::new())
        };
        let date = DATE
            .captures(chunk)?
            .and_then(|m| m.get(1).map(|m| m.as_str().trim().to_owned()))
            .unwrap_or_default();
        let subject = SUBJECT.captures(chunk);
        let display = subject
            .as_ref()
            .map(|m| &chunk[m.get(0).unwrap().end()..])
            .unwrap_or(chunk);
        quotes.push(json!({"sender":sender,"sender_name":name,"date":date,"sent_at":quote_timestamp(&date),"subject":subject.as_ref().map(|m| m[1].trim()).unwrap_or(""),"body":readable(display)}));
    }
    if quotes.is_empty()
        && value.split_whitespace().collect::<Vec<_>>()
            != readable(authored).split_whitespace().collect::<Vec<_>>()
    {
        let lines: Vec<_> = value
            .lines()
            .filter_map(|l| l.trim_start().strip_prefix('>').map(str::trim_start))
            .collect();
        let remainder = if !lines.is_empty() {
            lines.join("\n")
        } else {
            value
                .strip_prefix(readable(authored).trim())
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        if !remainder.is_empty() {
            quotes.push(json!({"sender":"","sender_name":"","date":"","body":remainder}));
        }
    }
    Ok(quotes)
}
pub fn topic(subject: &str) -> String {
    static PREFIX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^(?:(?:re|fw|fwd)\s*:\s*)+").unwrap());
    PREFIX.replace(&readable(subject), "").trim().to_owned()
}
// Existing SQLite deletion keys are hashes of Python's tuple repr. Keep that wire format.
fn python_repr(value: &str) -> String {
    use unicode_general_category::{GeneralCategory as G, get_general_category};
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::from(quote);
    for c in value.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c == quote => {
                result.push('\\');
                result.push(c);
            }
            c if c != ' '
                && matches!(
                    get_general_category(c),
                    G::Control
                        | G::Format
                        | G::PrivateUse
                        | G::Unassigned
                        | G::Surrogate
                        | G::SpaceSeparator
                        | G::LineSeparator
                        | G::ParagraphSeparator
                ) =>
            {
                let n = c as u32;
                result.push_str(&if n <= 0xff {
                    format!("\\x{n:02x}")
                } else if n <= 0xffff {
                    format!("\\u{n:04x}")
                } else {
                    format!("\\U{n:08x}")
                });
            }
            c => result.push(c),
        }
    }
    result.push(quote);
    result
}
pub fn quote_key(sender: &str, stamp: &str, body: &str, fingerprint: &str, index: usize) -> String {
    let normalized: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let mut parts = vec![
        python_repr(sender),
        python_repr(stamp),
        python_repr(&normalized),
    ];
    if sender.is_empty() || stamp.is_empty() {
        parts.push(python_repr(fingerprint));
        parts.push(index.to_string());
    }
    format!(
        "quote:{}",
        sha256(format!("({})", parts.join(", ")).as_bytes())
    )
}
pub fn has_reply(context: &Value) -> bool {
    !text(context, "in_reply_to").is_empty()
        || !array(context, "references").is_empty()
        || !array(context, "quotes").is_empty()
}
fn timestamp(stamp: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|d| d.timestamp_micros())
}
struct Item {
    value: Value,
    sort_at: String,
    quote_order: usize,
}
pub fn timeline(messages: &[Value], owner: &str, deleted: &HashSet<String>) -> Vec<Value> {
    timeline_checked(messages, owner, deleted, &crate::work::Work::default()).unwrap()
}
pub fn timeline_checked(
    messages: &[Value],
    owner: &str,
    deleted: &HashSet<String>,
    work: &crate::work::Work,
) -> Result<Vec<Value>> {
    let mut items: Vec<Item> = Vec::new();
    let mut lookup: HashMap<(String, String), BTreeMap<i64, usize>> = HashMap::new();
    for mail in messages {
        work.check()?;
        let normalized = readable(text(mail, "original_authored_body"))
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if let Some(stamp) = timestamp(text(mail, "sent_at")) {
            lookup
                .entry((owner.to_owned(), normalized))
                .or_default()
                .entry(stamp)
                .or_insert(items.len());
        }
        let fingerprint = mail
            .get("fingerprint")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| mail["id"].to_string());
        items.push(Item { sort_at: text(mail,"sent_at").to_owned(), quote_order: 0, value: json!({"kind":"sent","sender":owner,"sender_name":"나","mine":true,"sent_at":mail["sent_at"],"subject":topic(text(mail,"subject")),"body":mail["display_body"],"source_ids":[mail["id"]],"has_reply":mail["has_reply"],"keys":[format!("sent:{fingerprint}")]}) });
    }
    for mail in messages {
        work.check()?;
        for (index, quote) in array(&mail["context"], "quotes").iter().rev().enumerate() {
            work.check()?;
            let body = readable(text(quote, "body"));
            let sender = text(quote, "sender");
            let stamp = if text(quote, "sent_at").is_empty() {
                quote_timestamp(text(quote, "date"))
            } else {
                text(quote, "sent_at").to_owned()
            };
            let fingerprint = mail
                .get("fingerprint")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| mail["id"].to_string());
            let key = if text(quote, "key").is_empty() {
                quote_key(sender, &stamp, &body, &fingerprint, index)
            } else {
                text(quote, "key").to_owned()
            };
            let identity = (
                sender.to_owned(),
                body.chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>(),
            );
            let time = timestamp(&stamp);
            let duplicate = if !sender.is_empty() {
                time.and_then(|t| {
                    lookup.get(&identity).and_then(|m| {
                        m.range(t.saturating_sub(2_000_000)..=t.saturating_add(2_000_000))
                            .map(|(_, i)| *i)
                            .min()
                    })
                })
            } else {
                None
            };
            if let Some(i) = duplicate {
                for (field, value) in [("keys", json!(key)), ("source_ids", mail["id"].clone())] {
                    let values = items[i].value[field].as_array_mut().unwrap();
                    if !values.contains(&value) {
                        values.push(value);
                    }
                }
                continue;
            }
            if !sender.is_empty()
                && let Some(time) = time
            {
                lookup
                    .entry(identity)
                    .or_default()
                    .entry(time)
                    .or_insert(items.len());
            }
            items.push(Item { sort_at: if stamp.is_empty() { text(mail,"sent_at").to_owned() } else { stamp.clone() }, quote_order: index,
                value: json!({"kind":"quote","sender":sender,"sender_name":if !text(quote,"sender_name").is_empty() { text(quote,"sender_name") } else if !sender.is_empty() { sender } else { "작성자 미확인" },"mine":sender==owner,"sent_at":stamp,"subject":topic(if text(quote,"subject").is_empty() {text(mail,"subject")} else {text(quote,"subject")}),"body":body,"source_ids":[mail["id"]],"has_reply":true,"keys":[key]}) });
        }
    }
    items.sort_by(|a, b| {
        (&a.sort_at, a.value["kind"] == "sent", a.quote_order).cmp(&(
            &b.sort_at,
            b.value["kind"] == "sent",
            b.quote_order,
        ))
    });
    work.check()?;
    Ok(items
        .into_iter()
        .filter(|i| {
            !array(&i.value, "keys")
                .iter()
                .any(|k| deleted.contains(k.as_str().unwrap_or("")))
        })
        .map(|i| i.value)
        .collect())
}
