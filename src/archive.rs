//! Source mail, conversations, and transactional edits.
use crate::{
    array,
    context::{has_reply, readable, timeline},
    db, fault,
    pipeline::{PURPOSE_LABELS, PURPOSES, RELATIONSHIPS},
    privacy, search, select, text,
    work::Work,
};
use anyhow::{Result, bail};
use chrono::Utc;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};

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
pub(crate) fn conversation_deleted(db: &Connection, owner: &str, recipient: &str) -> Result<bool> {
    db::exists(
        db,
        "SELECT 1 FROM deleted_messages WHERE owner=? AND message_key=?",
        params![owner, format!("conversation:{recipient}")],
    )
}
pub(crate) fn require_active_conversation(
    db: &Connection,
    owner: &str,
    recipient: &str,
) -> Result<()> {
    if conversation_deleted(db, owner, recipient)? {
        return Err(fault(404, "삭제된 대화입니다. 대화 목록을 새로고침하세요."));
    }
    Ok(())
}
pub fn conversation(db: &Connection, owner: &str, recipient: &str) -> Result<Value> {
    let owner = privacy::person_id(owner);
    let recipient = privacy::person_id(recipient);
    if conversation_deleted(db, &owner, &recipient)? {
        return Ok(json!({"messages":[],"timeline":[]}));
    }
    let mut statement = db.prepare_cached("SELECT a.id FROM mail_archive a JOIN sent_mail s ON a.owner=s.owner AND a.fingerprint=s.fingerprint WHERE a.owner=? AND s.recipient=? ORDER BY a.sent_at,a.id")?;
    let ids = statement.query_map(params![owner, recipient], |r| r.get::<_, i64>(0))?;
    let messages = ids
        .map(|id| get_mail(db, id?))
        .collect::<Result<Vec<_>>>()?;
    finish_conversation(
        db,
        &owner,
        &recipient,
        messages,
        &crate::work::Work::default(),
    )
}

// The browsing index needs no raw originals, address arrays, or editor fields.
// Fetch its source rows together instead of calling get_mail for every message.
pub(crate) fn index_conversation(
    db: &Connection,
    owner: &str,
    recipient: &str,
    work: &crate::work::Work,
) -> Result<Value> {
    if conversation_deleted(db, owner, recipient)? {
        return Ok(json!({"messages":[],"timeline":[]}));
    }
    let deleted = deleted_keys(db, owner)?;
    let mut messages = db::rows(db,
        "SELECT a.id,a.owner,a.fingerprint,a.subject,a.sent_at,a.revision,a.original_authored_body,a.context_json,
         (SELECT authored_body FROM sent_mail WHERE owner=a.owner AND fingerprint=a.fingerprint ORDER BY recipient LIMIT 1) AS authored_body,
         (SELECT json_group_array(json_object('purpose',purpose)) FROM sent_mail WHERE owner=a.owner AND fingerprint=a.fingerprint) AS recipients_json
         FROM mail_archive a JOIN sent_mail s ON a.owner=s.owner AND a.fingerprint=s.fingerprint
         WHERE a.owner=? AND s.recipient=? ORDER BY a.sent_at,a.id", params![owner, recipient])?;
    for mail in &mut messages {
        work.check()?;
        mail["context"] = serde_json::from_str(text(mail, "context_json"))?;
        mail["recipients"] = serde_json::from_str(text(mail, "recipients_json"))?;
        mail["display_body"] = json!(readable(text(mail, "authored_body")));
        mail["has_reply"] = json!(has_reply(&mail["context"]));
        mail["deleted"] = json!(deleted.contains(&format!("sent:{}", text(mail, "fingerprint"))));
        for key in ["context_json", "recipients_json", "authored_body"] {
            mail.as_object_mut().unwrap().remove(key);
        }
    }
    finish_conversation(db, owner, recipient, messages, work)
}

fn finish_conversation(
    db: &Connection,
    owner: &str,
    recipient: &str,
    messages: Vec<Value>,
    work: &crate::work::Work,
) -> Result<Value> {
    let deleted = deleted_keys(db, owner)?;
    let mut timeline = crate::context::timeline(&messages, owner, &deleted, work)?;
    // Order manual mail by reply ID; it has no sent date.
    let positions: HashMap<_, _> = messages
        .iter()
        .filter_map(|m| {
            m["context"]["after_reply_id"]
                .as_i64()
                .map(|after| (m["id"].as_i64().unwrap(), after))
        })
        .collect();
    let mut entered = Vec::new();
    timeline.retain_mut(|item| {
        if item["kind"] == "sent"
            && let Some(id) = item["source_ids"][0].as_i64()
            && let Some(after) = positions.get(&id)
        {
            item.as_object_mut().unwrap().remove("sent_at");
            entered.push((*after, id, item.clone()));
            false
        } else {
            true
        }
    });
    let replies = db::rows(
        db,
        "SELECT * FROM received_replies WHERE owner=? AND recipient=? ORDER BY id",
        params![owner, recipient],
    )?;
    if !replies.is_empty() {
        let manual = db::one(
            db,
            "SELECT name FROM manual_conversations WHERE owner=? AND recipient=?",
            params![owner, recipient],
        )?;
        let name = manual
            .as_ref()
            .map(|c| text(c, "name"))
            .or_else(|| {
                messages
                    .iter()
                    .rev()
                    .find_map(|m| m["context"]["recipient_names"][recipient].as_str())
            })
            .unwrap_or("상대방");
        for reply in replies {
            work.check()?;
            let key = format!("received:{}", reply["id"]);
            if deleted.contains(&key) {
                continue;
            }
            entered.push((reply["id"].as_i64().unwrap(), 0, json!({"kind":"received","sender":recipient,"sender_name":name,"mine":false,"subject":"","body":reply["body"],"source_ids":[],"has_reply":true,"keys":[key]})));
        }
    }
    entered.sort_by_key(|(after, id, _)| (*after, *id));
    timeline.extend(entered.into_iter().map(|(_, _, item)| item));
    let mut result = json!({});
    result["messages"] = Value::Array(messages);
    result["timeline"] = Value::Array(timeline);
    Ok(result)
}
pub fn conversations(
    db: &Connection,
    query: &str,
    replies: bool,
    label_filter: &str,
    unlabeled: bool,
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
    let colors: serde_json::Map<String, Value> = db::rows(db, "SELECT * FROM label_colors", [])?
        .into_iter()
        .map(|row| {
            (
                text(&row, "label").to_owned(),
                select(&row, &["color", "revision"]),
            )
        })
        .collect();
    let mut groups = Vec::new();
    let mut all_ids = HashSet::new();
    let mut reply_ids = HashSet::new();
    let mut owners = BTreeSet::new();
    let query = search::fold(query).trim().to_owned();
    for contact in contacts.values() {
        let owner = text(contact, "owner");
        let recipient = text(contact, "recipient");
        if conversation_deleted(db, owner, recipient)? {
            continue;
        }
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
        let latest_date = items
            .iter()
            .rev()
            .find(|i| i.get("sent_at").is_some())
            .unwrap_or(&empty);
        let label_text = label.map(|l| text(l, "label")).unwrap_or("");
        let mut reply_count = active.iter().filter(|m| m["has_reply"] == true).count();
        if reply_count == 0 {
            reply_count = items.iter().filter(|m| m["kind"] == "quote").count();
        }
        reply_count += items.iter().filter(|m| m["kind"] == "received").count();
        let match_info = search::first_match(items, &query, &Work::default())?;
        // Check one field at a time instead of concatenating every message body.
        let matches_query = query.is_empty()
            || [recipient, name, label_text]
                .iter()
                .any(|s| search::fold(&readable(s)).contains(&query))
            || items.iter().any(|i| {
                search::fold(&readable(&format!(
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
                        .is_some_and(|i| search::fold(PURPOSE_LABELS[i]).contains(&query))
                })
            });
        groups.push(json!({"owner":owner,"recipient":recipient,"name":if name.is_empty() {recipient.split('@').next().unwrap_or("")} else {name},"label":label_text,"label_color":colors.get(label_text).map(|c|text(c,"color")).unwrap_or(""),"label_revision":label.map(|l| l["revision"].clone()).unwrap_or(json!(0)),"count":active.len(),"registered":registered.is_some(),"reply_count":reply_count,"latest":latest_date["sent_at"],"subject":latest["subject"],"preview":text(latest,"body").chars().take(140).collect::<String>(),"search_match":match_info,"matches":matches_query && (!replies || reply_count>0) && (if unlabeled {label_text.is_empty()} else {label_filter.is_empty() || label_text==label_filter})}));
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
        json!({"total_mails":all_ids.len(),"total_conversations":groups.len(),"reply_mails":reply_ids.len(),"owners":owners,"labels":labels,"label_colors":colors,"items":groups.into_iter().filter(|g|g["matches"]==true).collect::<Vec<_>>()}),
    )
}
pub fn update_mail(db: &Connection, id: i64, payload: &Value) -> Result<Value> {
    let tx = db::immediate(db)?;
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
        tx.execute("UPDATE sent_mail SET authored_body=?,role=?,relationship=?,purpose=? WHERE owner=? AND fingerprint=? AND recipient=?",params![privacy::redact(body),privacy::redact(if role.is_empty(){"unknown"}else{role}),text(p,"relationship"),text(p,"purpose"),text(&current,"owner"),text(&current,"fingerprint"),text(p,"recipient")])?;
    }
    tx.commit()?;
    get_mail(db, id)
}
fn update_label_inner(db: &Connection, payload: &Value) -> Result<Value> {
    let message = "직급 라벨은 100자 이내로 입력하세요.";
    let owner = privacy::person_id(string(payload, "owner", message)?);
    let recipient = privacy::person_id(string(payload, "recipient", message)?);
    let label = string(payload, "label", message)?;
    if label.chars().count() > 100 {
        bail!("{message}");
    }
    let version = revision(payload, "라벨의 수정 버전이 올바르지 않습니다.")?;
    require_active_conversation(db, &owner, &recipient)?;
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
    let label = privacy::redact(label.trim());
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
            if label.is_empty() { "unknown" } else { &label },
            owner,
            recipient
        ],
    )?;
    db.execute("UPDATE mail_archive SET revision=revision+1,updated_at=? WHERE owner=? AND fingerprint IN (SELECT fingerprint FROM sent_mail WHERE owner=? AND recipient=?)",params![Utc::now().to_rfc3339(),owner,owner,recipient])?;
    Ok(json!({"label":label,"revision":version+1}))
}
pub fn update_contact_label(db: &Connection, payload: &Value) -> Result<Value> {
    let tx = db::immediate(db)?;
    let result = update_label_inner(&tx, payload)?;
    if let Some(color) = payload.get("color") {
        let color = color
            .as_str()
            .ok_or_else(|| fault(400, "색상은 #RRGGBB 형식으로 입력하세요."))?;
        if !color.is_empty()
            && !(color.len() == 7
                && color.starts_with('#')
                && color[1..].bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(fault(400, "색상은 #RRGGBB 형식으로 입력하세요."));
        }
        let label = text(&result, "label");
        if !label.is_empty() {
            let version = payload["color_revision"]
                .as_i64()
                .ok_or_else(|| fault(400, "색상의 수정 버전이 올바르지 않습니다."))?;
            let current = db::one(
                &tx,
                "SELECT revision FROM label_colors WHERE label=?",
                [label],
            )?
            .map(|v| v["revision"].as_i64().unwrap())
            .unwrap_or(0);
            if current != version {
                return Err(fault(
                    409,
                    "다른 창에서 변경된 색상입니다. 새로고침 후 다시 지정하세요.",
                ));
            }
            tx.execute("INSERT INTO label_colors(label,color,revision) VALUES (?,?,1) ON CONFLICT(label) DO UPDATE SET color=excluded.color,revision=label_colors.revision+1",params![label,color.to_lowercase()])?;
        }
    }
    tx.commit()?;
    Ok(result)
}
pub fn create_conversation(db: &Connection, payload: &Value) -> Result<Value> {
    let owner = privacy::person_id(
        payload["owner"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("local"),
    );
    let name = string(payload, "name", "이름을 입력하세요.")?.trim();
    let label = payload
        .get("label")
        .map(|_| string(payload, "label", "직급을 확인하세요."))
        .transpose()?
        .unwrap_or("")
        .trim();
    if name.is_empty() || name.chars().count() > 100 || label.chars().count() > 100 {
        bail!("이름은 필수이며 이름과 직급은 100자 이내로 입력하세요.");
    }
    let name = privacy::redact(name);
    let label = privacy::redact(label);
    let tx = db::immediate(db)?;
    let existing = conversations(&tx, "", false, "", false)?;
    if let Some(contact) = array(&existing, "items")
        .iter()
        .find(|c| c["owner"] == owner && c["name"] == name)
    {
        return Ok(json!({"contact":contact,"created":false}));
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("대화 ID 생성 실패: {e}"))?;
    let recipient = format!("person:{}", crate::sha256(&random));
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
    let data = conversations(db, "", false, "", false)?;
    let contact = array(&data, "items")
        .iter()
        .find(|c| c["owner"] == owner && c["recipient"] == recipient)
        .ok_or_else(|| fault(404, "대화를 찾을 수 없습니다."))?;
    Ok(json!({"contact":contact,"created":true}))
}
pub fn rename_conversation(db: &Connection, payload: &Value) -> Result<Value> {
    let owner = privacy::person_id(string(payload, "owner", "대화를 선택하세요.")?);
    let recipient = privacy::person_id(string(payload, "recipient", "대화를 선택하세요.")?);
    let name = string(payload, "name", "이름을 입력하세요.")?.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(fault(400, "이름은 1~100자로 입력하세요."));
    }
    let name = privacy::redact(name);
    let original = string(
        payload,
        "original_name",
        "대화를 다시 열어 이름을 수정하세요.",
    )?;
    let tx = db::immediate(db)?;
    require_active_conversation(&tx, &owner, &recipient)?;
    let current = db::one(
        &tx,
        "SELECT name FROM manual_conversations WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?
    .ok_or_else(|| fault(404, "직접 만든 대화를 선택하세요."))?;
    if current["name"] != original {
        return Err(fault(
            409,
            "다른 창에서 이름이 변경되었습니다. 대화를 다시 열어 수정하세요.",
        ));
    }
    let contacts = conversations(&tx, "", false, "", false)?;
    if array(&contacts, "items")
        .iter()
        .any(|c| c["owner"] == owner && c["recipient"] != recipient && c["name"] == name)
    {
        return Err(fault(
            409,
            "같은 이름의 대화가 있습니다. 다른 이름을 입력하세요.",
        ));
    }
    tx.execute(
        "UPDATE manual_conversations SET name=? WHERE owner=? AND recipient=?",
        params![name, owner, recipient],
    )?;
    tx.commit()?;
    Ok(json!({"name":name}))
}

pub fn create_sent_mail(db: &Connection, payload: &Value) -> Result<Value> {
    let owner = privacy::person_id(string(payload, "owner", "대화를 선택하세요.")?);
    let recipient = privacy::person_id(string(payload, "recipient", "대화를 선택하세요.")?);
    let body = string(payload, "body", "내가 보낸 메일 내용을 입력하세요.")?;
    if body.trim().is_empty() {
        return Err(fault(400, "내가 보낸 메일 내용을 입력하세요."));
    }
    let tx = db::immediate(db)?;
    require_active_conversation(&tx, &owner, &recipient)?;
    let manual = db::one(
        &tx,
        "SELECT name FROM manual_conversations WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?;
    let data = conversation(&tx, &owner, &recipient)?;
    let messages = array(&data, "messages");
    if manual.is_none() && messages.is_empty() {
        return Err(fault(
            404,
            "대화를 찾을 수 없습니다. 대화를 다시 선택하세요.",
        ));
    }
    let name = manual
        .as_ref()
        .map(|c| text(c, "name"))
        .or_else(|| {
            messages
                .iter()
                .rev()
                .find_map(|m| m["context"]["recipient_names"][&recipient].as_str())
        })
        .unwrap_or("상대방");
    let label = db::one(
        &tx,
        "SELECT label FROM contact_labels WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?;
    let role = label
        .as_ref()
        .map(|l| text(l, "label"))
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");
    let after_reply_id: i64 = tx.query_row(
        "SELECT COALESCE(MAX(id),0) FROM received_replies WHERE owner=? AND recipient=?",
        params![owner, recipient],
        |r| r.get(0),
    )?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| anyhow::anyhow!("메일 ID 생성 실패: {e}"))?;
    let fingerprint = crate::sha256(&random);
    let body = privacy::redact(body);
    let context =
        json!({"recipient_names":{&recipient:name},"quotes":[],"after_reply_id":after_reply_id});
    tx.execute("INSERT INTO mail_archive(owner,fingerprint,subject,sent_at,sender,to_json,cc_json,original_body,original_authored_body,source_name,context_json) VALUES (?,?,'','',?,?,'[]',?,?,'',?)",
        params![owner, fingerprint, owner, json!([recipient]).to_string(), body, body, context.to_string()])?;
    let id = tx.last_insert_rowid();
    tx.execute("INSERT INTO sent_mail(owner,fingerprint,message_id,sender,recipient,role,relationship,purpose,authored_body) VALUES (?,?,?,?,?,?,'unknown','other',?)",
        params![owner, fingerprint, format!("manual:{fingerprint}"), owner, recipient, role, body])?;
    tx.commit()?;
    Ok(json!({"id":id,"key":format!("sent:{fingerprint}")}))
}

fn reply_body(payload: &Value) -> Result<&str> {
    let body = string(payload, "body", "상대 답장을 입력하세요.")?;
    if body.trim().is_empty() {
        return Err(fault(400, "상대 답장을 입력하세요."));
    }
    Ok(body)
}

pub fn create_reply(db: &Connection, payload: &Value) -> Result<Value> {
    let owner = string(payload, "owner", "대화를 선택하세요.")?;
    let recipient = string(payload, "recipient", "대화를 선택하세요.")?;
    let body = reply_body(payload)?;
    let owner = privacy::person_id(owner);
    let recipient = privacy::person_id(recipient);
    let tx = db::immediate(db)?;
    require_active_conversation(&tx, &owner, &recipient)?;
    if !db::exists(
        &tx,
        "SELECT 1 FROM sent_mail WHERE owner=? AND recipient=? UNION ALL SELECT 1 FROM manual_conversations WHERE owner=? AND recipient=?",
        params![owner, recipient, owner, recipient],
    )? {
        return Err(fault(
            404,
            "대화를 찾을 수 없습니다. 대화를 다시 선택하세요.",
        ));
    }
    tx.execute(
        "INSERT INTO received_replies(owner,recipient,body) VALUES (?,?,?)",
        params![owner, recipient, privacy::redact(body)],
    )?;
    let key = format!("received:{}", tx.last_insert_rowid());
    tx.commit()?;
    Ok(json!({"key":key}))
}

pub fn update_reply(db: &Connection, payload: &Value) -> Result<Value> {
    let owner = privacy::person_id(string(payload, "owner", "대화를 선택하세요.")?);
    let recipient = privacy::person_id(string(payload, "recipient", "대화를 선택하세요.")?);
    let key = string(payload, "key", "수정할 답장을 선택하세요.")?;
    let id = key
        .strip_prefix("received:")
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(|| fault(400, "수정할 답장을 선택하세요."))?;
    let original_body = string(payload, "original_body", "답장을 다시 열어 수정하세요.")?;
    let body = reply_body(payload)?;
    let tx = db::immediate(db)?;
    require_active_conversation(&tx, &owner, &recipient)?;
    let current = db::one(
        &tx,
        "SELECT body FROM received_replies WHERE id=? AND owner=? AND recipient=?",
        params![id, owner, recipient],
    )?
    .ok_or_else(|| fault(404, "답장을 찾을 수 없습니다. 대화를 다시 여세요."))?;
    if deleted_keys(&tx, &owner)?.contains(key) {
        return Err(fault(404, "삭제된 답장입니다. 대화를 다시 여세요."));
    }
    if current["body"] != original_body {
        return Err(fault(
            409,
            "다른 창에서 답장이 변경되었습니다. 답장을 다시 열어 수정하세요.",
        ));
    }
    tx.execute(
        "UPDATE received_replies SET body=? WHERE id=? AND owner=? AND recipient=?",
        params![privacy::redact(body), id, owner, recipient],
    )?;
    tx.commit()?;
    Ok(json!({"key":key}))
}
pub fn delete_conversation(db: &Connection, payload: &Value) -> Result<Value> {
    let owner = string(payload, "owner", "삭제할 대화를 선택하세요.")?;
    let recipient = string(payload, "recipient", "삭제할 대화를 선택하세요.")?;
    if owner.trim().is_empty() || recipient.trim().is_empty() {
        return Err(fault(400, "삭제할 대화를 선택하세요."));
    }
    let owner = privacy::person_id(owner);
    let recipient = privacy::person_id(recipient);
    let tx = db::immediate(db)?;
    if !conversation_deleted(&tx, &owner, &recipient)?
        && !db::exists(
            &tx,
            "SELECT 1 FROM sent_mail WHERE owner=?1 AND recipient=?2 UNION ALL SELECT 1 FROM manual_conversations WHERE owner=?1 AND recipient=?2 UNION ALL SELECT 1 FROM received_replies WHERE owner=?1 AND recipient=?2",
            params![owner, recipient],
        )?
    {
        return Err(fault(
            404,
            "대화를 찾을 수 없습니다. 대화 목록을 새로고침하세요.",
        ));
    }
    // Keep only the contact ID to prevent reimporting deleted content.
    tx.execute(
        "INSERT OR IGNORE INTO deleted_messages(owner,message_key) VALUES (?,?)",
        params![owner, format!("conversation:{recipient}")],
    )?;
    db::purge_conversation(&tx, &owner, &recipient)?;
    tx.commit()?;
    Ok(json!({"deleted":true}))
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
    let owner = privacy::person_id(owner);
    let recipient = privacy::person_id(recipient);
    let tx = db::immediate(db)?;
    let data = conversation(&tx, &owner, &recipient)?;
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
    let ids = db::rows(&tx, "SELECT id FROM mail_archive WHERE owner=?", [&owner])?;
    let all_mail = ids
        .iter()
        .map(|r| get_mail(&tx, r["id"].as_i64().unwrap()))
        .collect::<Result<Vec<_>>>()?;
    let mut keys: HashSet<String> = array(item, "keys")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    for related in timeline(&all_mail, &owner, &HashSet::new(), &Work::default())? {
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

fn string<'a>(payload: &'a Value, key: &str, message: &str) -> Result<&'a str> {
    payload[key].as_str().ok_or_else(|| fault(400, message))
}
fn revision(payload: &Value, message: &str) -> Result<i64> {
    payload["revision"]
        .as_i64()
        .ok_or_else(|| fault(400, message))
}
