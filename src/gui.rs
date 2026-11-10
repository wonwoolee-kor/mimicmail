use crate::{
    array,
    context::{has_reply, readable, timeline},
    db, fault,
    pipeline::{GENERATE, PURPOSES, RELATIONSHIPS, STYLE_FIELDS, retrieve},
    select, text,
};
use anyhow::{Result, bail};
use chrono::Utc;
use regex::Regex;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
    sync::LazyLock,
};
use unicode_casefold::UnicodeCaseFold;

pub const PURPOSE_LABELS: [&str; 8] = [
    "요청",
    "답변",
    "보고",
    "일정",
    "감사",
    "사과",
    "후속 확인",
    "기타",
];
pub const RELATIONSHIP_LABELS: [&str; 10] = [
    "상사",
    "동료",
    "부하",
    "고객",
    "공급자",
    "지도교수",
    "지도학생",
    "개인",
    "외부 관계자",
    "미상",
];
pub fn labels(keys: &[&str], values: &[&str]) -> Value {
    keys.iter()
        .zip(values)
        .map(|(k, v)| ((*k).to_owned(), json!(v)))
        .collect()
}
fn fold(s: &str) -> String {
    s.case_fold().collect()
}
fn chars(s: &str, start: usize, length: usize) -> String {
    s.chars().skip(start).take(length).collect()
}
pub fn first_search_match(items: &[Value], query: &str) -> Option<Value> {
    if query.is_empty() {
        return None;
    }
    for item in items {
        for field in ["body", "subject"] {
            let value = text(item, field);
            let folded = fold(value);
            let Some(offset) = folded.find(query) else {
                continue;
            };
            let mut positions = Vec::with_capacity(folded.len());
            for (index, c) in value.chars().enumerate() {
                for folded in c.case_fold() {
                    positions.extend(std::iter::repeat_n(index, folded.len_utf8()));
                }
            }
            let start = positions[offset];
            let end = positions[offset + query.len() - 1] + 1;
            let left = start.saturating_sub(12);
            let count = value.chars().count();
            let right = (left + 140).max(end).min(count);
            let preview = format!(
                "{}{}{}",
                if left > 0 { "…" } else { "" },
                chars(value, left, right - left),
                if right < count { "…" } else { "" }
            );
            return Some(
                json!({"key":item["keys"][0],"field":field,"term":chars(value,start,end-start),"preview":preview}),
            );
        }
    }
    None
}
pub fn get_mail(db: &Connection, id: i64) -> Result<Value> {
    let mut item = db::one(db, "SELECT * FROM mail_archive WHERE id=?", [id])?
        .ok_or_else(|| fault(404, "메일을 찾을 수 없습니다."))?;
    let recipients = db::rows(
        db,
        "SELECT recipient,role,relationship,purpose,authored_body FROM sent_mail WHERE owner=? AND fingerprint=? ORDER BY recipient",
        params![text(&item, "owner"), text(&item, "fingerprint")],
    )?;
    for (field, key) in [
        ("to_json", "to"),
        ("cc_json", "cc"),
        ("context_json", "context"),
    ] {
        item[key] = serde_json::from_str(text(&item, field))?;
        item.as_object_mut().unwrap().remove(field);
    }
    item["authored_body"] = json!(
        recipients
            .first()
            .map(|r| text(r, "authored_body"))
            .unwrap_or(text(&item, "original_authored_body"))
    );
    item["recipients"] = recipients
        .iter()
        .map(|r| select(r, &["recipient", "role", "relationship", "purpose"]))
        .collect();
    for (key, source) in [
        ("display_body", "authored_body"),
        ("display_original", "original_body"),
        ("display_original_authored", "original_authored_body"),
    ] {
        item[key] = json!(readable(text(&item, source)));
    }
    item["has_reply"] = json!(has_reply(&item["context"]));
    item["deleted"] = json!(db::exists(
        db,
        "SELECT 1 FROM deleted_messages WHERE owner=? AND message_key=?",
        params![
            text(&item, "owner"),
            format!("sent:{}", text(&item, "fingerprint"))
        ]
    )?);
    Ok(item)
}
fn deleted_keys(db: &Connection, owner: &str) -> Result<HashSet<String>> {
    Ok(db
        .prepare_cached("SELECT message_key FROM deleted_messages WHERE owner=?")?
        .query_map([owner], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
pub fn conversation(db: &Connection, owner: &str, recipient: &str) -> Result<Value> {
    let mut statement = db.prepare_cached("SELECT a.id FROM mail_archive a JOIN sent_mail s ON a.owner=s.owner AND a.fingerprint=s.fingerprint WHERE a.owner=? AND s.recipient=? ORDER BY a.sent_at,a.id")?;
    let ids = statement.query_map(params![owner, recipient], |r| r.get::<_, i64>(0))?;
    let messages = ids
        .map(|id| get_mail(db, id?))
        .collect::<Result<Vec<_>>>()?;
    let timeline = timeline(&messages, owner, &deleted_keys(db, owner)?);
    Ok(json!({"messages":messages,"timeline":timeline}))
}
pub fn conversations(
    db: &Connection,
    query: &str,
    replies: bool,
    label_filter: &str,
) -> Result<Value> {
    let mut contacts: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut statement = db.prepare_cached("SELECT a.owner,s.recipient,a.context_json FROM mail_archive a JOIN sent_mail s ON a.owner=s.owner AND a.fingerprint=s.fingerprint ORDER BY a.sent_at DESC,a.id DESC")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let owner: String = row.get(0)?;
        let recipient: String = row.get(1)?;
        let context: Value = serde_json::from_str(&row.get::<_, String>(2)?)?;
        let key = json!([owner, recipient]).to_string();
        let name = context["recipient_names"][&recipient]
            .as_str()
            .unwrap_or("");
        if contacts
            .get(&key)
            .is_none_or(|v| text(v, "name").is_empty())
        {
            contacts.insert(
                key,
                json!({"owner":owner,"recipient":recipient,"name":name}),
            );
        }
    }
    let manual = db::rows(db, "SELECT * FROM manual_conversations", [])?;
    for item in &manual {
        let key = json!([item["owner"], item["recipient"]]).to_string();
        let existing = contacts.get(&key).map(|v| text(v, "name")).unwrap_or("");
        let name = if text(item, "name").is_empty() {
            existing
        } else {
            text(item, "name")
        };
        contacts.insert(
            key,
            json!({"owner":item["owner"],"recipient":item["recipient"],"name":name}),
        );
    }
    let label_rows = db::rows(db, "SELECT * FROM contact_labels", [])?;
    let mut groups = Vec::new();
    let mut all_ids = HashSet::new();
    let mut reply_ids = HashSet::new();
    let mut owners = BTreeSet::new();
    let query = fold(query).trim().to_owned();
    for contact in contacts.values() {
        let owner = text(contact, "owner");
        let recipient = text(contact, "recipient");
        let name = text(contact, "name");
        let data = conversation(db, owner, recipient)?;
        let registered = manual
            .iter()
            .find(|m| m["owner"] == owner && m["recipient"] == recipient);
        let items = array(&data, "timeline");
        if items.is_empty() && registered.is_none() {
            continue;
        }
        let active: Vec<_> = array(&data, "messages")
            .iter()
            .filter(|m| m["deleted"] != true)
            .collect();
        for mail in &active {
            all_ids.insert(mail["id"].as_i64().unwrap());
            if mail["has_reply"] == true {
                reply_ids.insert(mail["id"].as_i64().unwrap());
            }
        }
        owners.insert(owner.to_owned());
        let label = label_rows
            .iter()
            .find(|l| l["owner"] == owner && l["recipient"] == recipient);
        let empty = json!({"sent_at":registered.map(|r| text(r,"created_at")).unwrap_or(""),"subject":"","body":""});
        let latest = items.last().unwrap_or(&empty);
        let label_text = label.map(|l| text(l, "label")).unwrap_or("");
        let mut reply_count = active.iter().filter(|m| m["has_reply"] == true).count();
        if reply_count == 0 {
            reply_count = items.iter().filter(|m| m["kind"] == "quote").count();
        }
        let match_info = first_search_match(items, &query);
        // Check one field at a time instead of concatenating every message body.
        let matches_query = query.is_empty()
            || [recipient, name, label_text]
                .iter()
                .any(|s| fold(&readable(s)).contains(&query))
            || items.iter().any(|i| {
                fold(&readable(&format!(
                    "{} {}",
                    text(i, "subject"),
                    text(i, "body")
                )))
                .contains(&query)
            })
            || active.iter().any(|m| {
                array(m, "recipients").iter().any(|p| {
                    PURPOSES
                        .iter()
                        .position(|v| *v == text(p, "purpose"))
                        .is_some_and(|i| fold(PURPOSE_LABELS[i]).contains(&query))
                })
            });
        groups.push(json!({"owner":owner,"recipient":recipient,"name":if name.is_empty() {recipient.split('@').next().unwrap_or("")} else {name},"label":label_text,"label_revision":label.map(|l| l["revision"].clone()).unwrap_or(json!(0)),"count":active.len(),"registered":registered.is_some(),"reply_count":reply_count,"latest":latest["sent_at"],"subject":latest["subject"],"preview":chars(text(latest,"body"),0,140),"search_match":match_info,"matches":matches_query && (!replies || reply_count>0) && (label_filter.is_empty() || label_text==label_filter)}));
    }
    groups.sort_by_key(|g| {
        std::cmp::Reverse(
            chrono::DateTime::parse_from_rfc3339(text(g, "latest"))
                .map(|d| d.timestamp_micros())
                .unwrap_or(0),
        )
    });
    let labels: BTreeSet<_> = groups
        .iter()
        .map(|g| text(g, "label").to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    Ok(
        json!({"total_mails":all_ids.len(),"total_conversations":groups.len(),"reply_mails":reply_ids.len(),"owners":owners,"labels":labels,"items":groups.into_iter().filter(|g|g["matches"]==true).collect::<Vec<_>>()}),
    )
}
fn string<'a>(payload: &'a Value, key: &str, message: &str) -> Result<&'a str> {
    payload[key].as_str().ok_or_else(|| fault(400, message))
}
fn revision(payload: &Value, message: &str) -> Result<i64> {
    payload["revision"]
        .as_i64()
        .ok_or_else(|| fault(400, message))
}
fn immediate(db: &Connection) -> Result<Transaction<'_>> {
    Ok(Transaction::new_unchecked(
        db,
        TransactionBehavior::Immediate,
    )?)
}
pub fn update_mail(db: &Connection, id: i64, payload: &Value) -> Result<Value> {
    let tx = immediate(db)?;
    let current = get_mail(&tx, id)?;
    if current["deleted"] == true {
        return Err(fault(
            409,
            "삭제된 메시지입니다. 편집기를 닫고 새로고침하세요.",
        ));
    }
    let body = string(payload, "authored_body", "문체 참조 본문을 입력하세요.")?;
    if body.trim().is_empty() {
        bail!("문체 참조 본문을 입력하세요.");
    }
    let version = revision(payload, "메일의 수정 버전이 없습니다. 메일을 다시 여세요.")?;
    let profiles = payload["recipients"]
        .as_array()
        .filter(|a| a.iter().all(Value::is_object))
        .ok_or_else(|| fault(400, "수신자 정보 형식이 올바르지 않습니다."))?;
    let expected: HashSet<_> = array(&current, "recipients")
        .iter()
        .map(|p| text(p, "recipient"))
        .collect();
    if profiles.len() != expected.len()
        || profiles
            .iter()
            .map(|p| text(p, "recipient"))
            .collect::<HashSet<_>>()
            != expected
    {
        bail!("기존 수신자의 분류만 수정할 수 있습니다.");
    }
    for p in profiles {
        if !PURPOSES.contains(&text(p, "purpose"))
            || !RELATIONSHIPS.contains(&text(p, "relationship"))
        {
            bail!("유효한 목적과 관계를 선택하세요.");
        }
        if string(p, "role", "직책은 100자 이내로 입력하세요.")?
            .chars()
            .count()
            > 100
        {
            bail!("직책은 100자 이내로 입력하세요.");
        }
    }
    if tx.execute(
        "UPDATE mail_archive SET revision=revision+1,updated_at=? WHERE id=? AND revision=?",
        params![Utc::now().to_rfc3339(), id, version],
    )? == 0
    {
        return Err(fault(
            409,
            "다른 창에서 변경된 메일입니다. 입력 내용을 복사한 뒤 메일을 다시 여세요.",
        ));
    }
    for p in profiles {
        let label = db::one(
            &tx,
            "SELECT label FROM contact_labels WHERE owner=? AND recipient=?",
            params![text(&current, "owner"), text(p, "recipient")],
        )?;
        let role = label
            .as_ref()
            .map(|l| text(l, "label"))
            .unwrap_or(text(p, "role").trim());
        tx.execute("UPDATE sent_mail SET authored_body=?,role=?,relationship=?,purpose=? WHERE owner=? AND fingerprint=? AND recipient=?",params![body,if role.is_empty(){"unknown"}else{role},text(p,"relationship"),text(p,"purpose"),text(&current,"owner"),text(&current,"fingerprint"),text(p,"recipient")])?;
    }
    tx.commit()?;
    get_mail(db, id)
}
fn update_label_inner(db: &Connection, payload: &Value) -> Result<Value> {
    let message = "직급 라벨은 100자 이내로 입력하세요.";
    let owner = string(payload, "owner", message)?;
    let recipient = string(payload, "recipient", message)?;
    let label = string(payload, "label", message)?;
    if label.chars().count() > 100 {
        bail!("{message}");
    }
    let version = revision(payload, "라벨의 수정 버전이 올바르지 않습니다.")?;
    if version < 0 {
        bail!("라벨의 수정 버전이 올바르지 않습니다.");
    }
    if !db::exists(
        db,
        "SELECT 1 FROM sent_mail WHERE owner=? AND recipient=? UNION ALL SELECT 1 FROM manual_conversations WHERE owner=? AND recipient=?",
        params![owner, recipient, owner, recipient],
    )? {
        return Err(fault(404, "대화를 찾을 수 없습니다."));
    }
    let current = db::one(
        db,
        "SELECT revision FROM contact_labels WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?;
    if current
        .map(|r| r["revision"].as_i64().unwrap_or(0))
        .unwrap_or(0)
        != version
    {
        return Err(fault(
            409,
            "다른 창에서 변경된 라벨입니다. 새로고침 후 다시 지정하세요.",
        ));
    }
    let label = label.trim();
    let changed=db.execute("INSERT INTO contact_labels(owner,recipient,label,revision) VALUES (?,?,?,1) ON CONFLICT(owner,recipient) DO UPDATE SET label=excluded.label,revision=contact_labels.revision+1 WHERE contact_labels.revision=?",params![owner,recipient,label,version])?;
    if changed == 0 {
        return Err(fault(
            409,
            "다른 창에서 변경된 라벨입니다. 새로고침 후 다시 지정하세요.",
        ));
    }
    db.execute(
        "UPDATE sent_mail SET role=? WHERE owner=? AND recipient=?",
        params![
            if label.is_empty() { "unknown" } else { label },
            owner,
            recipient
        ],
    )?;
    db.execute("UPDATE mail_archive SET revision=revision+1,updated_at=? WHERE owner=? AND fingerprint IN (SELECT fingerprint FROM sent_mail WHERE owner=? AND recipient=?)",params![Utc::now().to_rfc3339(),owner,owner,recipient])?;
    Ok(json!({"label":label,"revision":version+1}))
}
pub fn update_contact_label(db: &Connection, payload: &Value) -> Result<Value> {
    let tx = immediate(db)?;
    let result = update_label_inner(&tx, payload)?;
    tx.commit()?;
    Ok(result)
}
pub fn create_conversation(db: &Connection, payload: &Value) -> Result<Value> {
    let owner = string(payload, "owner", "수신자 정보를 확인하세요.")?
        .trim()
        .to_lowercase();
    let recipient = string(payload, "recipient", "수신자 정보를 확인하세요.")?
        .trim()
        .to_lowercase();
    let optional = |key| -> Result<&str> {
        if payload.get(key).is_none() {
            Ok("")
        } else {
            string(payload, key, "수신자 정보를 확인하세요.")
        }
    };
    let name = optional("name")?.trim();
    let label = optional("label")?.trim();
    static ADDRESS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[^\s@<>,;]+@[^\s@<>,;]+\.[^\s@<>,;]+$").unwrap());
    if !ADDRESS.is_match(&owner) || !ADDRESS.is_match(&recipient) {
        bail!("올바른 발신자·수신자 이메일을 입력하세요.");
    }
    if name.chars().count() > 100 || label.chars().count() > 100 {
        bail!("이름과 직급은 100자 이내로 입력하세요.");
    }
    let tx = immediate(db)?;
    let existing = conversations(&tx, "", false, "")?;
    if let Some(contact) = array(&existing, "items")
        .iter()
        .find(|c| c["owner"] == owner && c["recipient"] == recipient)
    {
        return Ok(json!({"contact":contact,"created":false}));
    }
    tx.execute(
        "INSERT INTO manual_conversations VALUES (?,?,?,?) ON CONFLICT(owner,recipient) DO NOTHING",
        params![owner, recipient, name, Utc::now().to_rfc3339()],
    )?;
    if !label.is_empty() {
        let version = db::one(
            &tx,
            "SELECT revision FROM contact_labels WHERE owner=? AND recipient=?",
            params![owner, recipient],
        )?
        .map(|r| r["revision"].clone())
        .unwrap_or(json!(0));
        update_label_inner(
            &tx,
            &json!({"owner":owner,"recipient":recipient,"label":label,"revision":version}),
        )?;
    }
    tx.commit()?;
    let data = conversations(db, "", false, "")?;
    let contact = array(&data, "items")
        .iter()
        .find(|c| c["owner"] == owner && c["recipient"] == recipient)
        .ok_or_else(|| fault(404, "대화를 찾을 수 없습니다."))?;
    Ok(json!({"contact":contact,"created":true}))
}
pub fn delete_message(db: &Connection, payload: &Value) -> Result<Value> {
    let owner = string(payload, "owner", "삭제할 메시지 정보가 올바르지 않습니다.")?;
    let recipient = string(
        payload,
        "recipient",
        "삭제할 메시지 정보가 올바르지 않습니다.",
    )?;
    let key = string(payload, "key", "삭제할 메시지 정보가 올바르지 않습니다.")?;
    if [owner, recipient, key].iter().any(|s| s.is_empty()) {
        bail!("삭제할 메시지 정보가 올바르지 않습니다.");
    }
    let tx = immediate(db)?;
    let data = conversation(&tx, owner, recipient)?;
    let item = array(&data, "timeline")
        .iter()
        .find(|i| array(i, "keys").iter().any(|k| k == key))
        .ok_or_else(|| {
            fault(
                404,
                "이미 삭제되었거나 찾을 수 없는 메시지입니다. 새로고침하세요.",
            )
        })?;
    if item["kind"] == "sent" {
        let mail = array(&data, "messages")
            .iter()
            .find(|m| m["id"] == item["source_ids"][0])
            .unwrap();
        if payload["revision"].as_i64().is_none() || payload["revision"] != mail["revision"] {
            return Err(fault(
                409,
                "다른 창에서 변경된 메시지입니다. 새로고침 후 다시 삭제하세요.",
            ));
        }
    }
    let ids = db::rows(&tx, "SELECT id FROM mail_archive WHERE owner=?", [owner])?;
    let all_mail = ids
        .iter()
        .map(|r| get_mail(&tx, r["id"].as_i64().unwrap()))
        .collect::<Result<Vec<_>>>()?;
    let mut keys: HashSet<String> = array(item, "keys")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    for related in timeline(&all_mail, owner, &HashSet::new()) {
        if array(&related, "keys")
            .iter()
            .any(|k| keys.contains(k.as_str().unwrap_or("")))
        {
            keys.extend(
                array(&related, "keys")
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned),
            );
        }
    }
    for key in keys {
        tx.execute(
            "INSERT OR IGNORE INTO deleted_messages VALUES (?,?)",
            params![owner, key],
        )?;
    }
    tx.commit()?;
    Ok(json!({"deleted":true}))
}
pub fn draft_context(
    db: &Connection,
    owner: &str,
    recipient: &str,
    purpose: &str,
) -> Result<Value> {
    let owner = owner.trim().to_lowercase();
    let recipient = recipient.trim().to_lowercase();
    let data = conversation(db, &owner, &recipient)?;
    let active: Vec<_> = array(&data, "messages")
        .iter()
        .filter(|m| m["deleted"] != true)
        .collect();
    let profile = active.last().and_then(|m| {
        array(m, "recipients")
            .iter()
            .find(|p| p["recipient"] == recipient)
    });
    let label = db::one(
        db,
        "SELECT label FROM contact_labels WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?;
    let role = label
        .as_ref()
        .map(|l| {
            if text(l, "label").is_empty() {
                "unknown"
            } else {
                text(l, "label")
            }
        })
        .unwrap_or(profile.map(|p| text(p, "role")).unwrap_or("unknown"));
    let profile = json!({"role":role,"relationship":profile.map(|p|text(p,"relationship")).unwrap_or("unknown"),"purpose":purpose});
    let facts: Vec<_>=array(&data,"timeline").iter().map(|i|json!({"sender":if text(i,"sender").is_empty(){"작성자 미확인"}else{text(i,"sender")},"date":i["sent_at"],"subject":i["subject"],"body":if i["kind"]=="sent" {active.iter().find(|m|m["id"]==i["source_ids"][0]).map(|m|m["display_original_authored"].clone()).unwrap_or(Value::Null)}else{i["body"].clone()}})).collect();
    let references = retrieve(db, &owner, &recipient, &profile, 5)?;
    Ok(
        json!({"instructions":GENERATE,"current_facts":{"user":owner,"recipient":recipient,"conversation":facts},"recipient_context":profile,"style_references":references.iter().map(|r|select(r,&STYLE_FIELDS)).collect::<Vec<_>>()}),
    )
}
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}
pub fn prompt_reference(db_path: &Path, owner: &str, recipient: &str) -> Result<Value> {
    let path = std::path::absolute(db_path)?;
    let binary = std::env::current_exe()?;
    let command = [
        binary.to_string_lossy().as_ref(),
        "--db",
        path.to_string_lossy().as_ref(),
        "--user",
        owner,
        "context",
        "--to",
        recipient,
    ]
    .map(shell_quote)
    .join(" ");
    Ok(
        json!({"database_path":path,"project_path":env!("CARGO_MANIFEST_DIR"),"context_command":command}),
    )
}
