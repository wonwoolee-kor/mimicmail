//! Disk-backed browsing projections. Source rows remain authoritative; triggers
//! invalidate affected conversations even when another CLI/process edits the DB.
use crate::{array, db, gui, privacy, select, text, work::Work};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use unicode_casefold::UnicodeCaseFold;

fn fold(s: &str) -> String {
    s.case_fold().collect()
}

pub fn initialize(db: &Connection) -> Result<()> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    tx.execute_batch(include_str!("../assets/browse-schema.sql"))?;
    let current = db::exists(&tx, "SELECT 1 FROM browse_meta WHERE version=1", [])?;
    if !db::rows(&tx, "PRAGMA table_info(browse_items)", [])?
        .iter()
        .any(|r| r["name"] == "search_text")
    {
        tx.execute_batch("ALTER TABLE browse_items ADD COLUMN search_text TEXT;")?;
    }
    for table in [
        "mail_archive",
        "sent_mail",
        "contact_labels",
        "manual_conversations",
        "received_replies",
        "deleted_messages",
    ] {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            let mut body = String::new();
            for row in ["OLD", "NEW"] {
                if (row == "OLD" && event == "INSERT") || (row == "NEW" && event == "DELETE") {
                    continue;
                }
                let select = match table {
                    "sent_mail" => format!(
                        "SELECT owner,recipient FROM sent_mail WHERE owner={row}.owner AND fingerprint={row}.fingerprint UNION SELECT {row}.owner,{row}.recipient"
                    ),
                    "mail_archive" => format!(
                        "SELECT owner,recipient FROM sent_mail WHERE owner={row}.owner AND fingerprint={row}.fingerprint"
                    ),
                    "deleted_messages" => format!(
                        "SELECT owner,recipient FROM sent_mail WHERE owner={row}.owner UNION SELECT owner,recipient FROM manual_conversations WHERE owner={row}.owner UNION SELECT owner,recipient FROM received_replies WHERE owner={row}.owner"
                    ),
                    _ => format!("SELECT {row}.owner,{row}.recipient"),
                };
                body.push_str(&format!("INSERT OR IGNORE INTO browse_dirty {select};"));
            }
            if !current {
                tx.execute_batch(&format!("DROP TRIGGER IF EXISTS browse_{table}_{event};"))?;
            }
            tx.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS browse_{table}_{event} AFTER {event} ON {table} BEGIN {body} END;"))?;
        }
    }
    if !current {
        tx.execute_batch("DELETE FROM browse_items; DELETE FROM browse_contacts; DELETE FROM browse_dirty; DELETE FROM browse_meta; INSERT INTO browse_meta VALUES (1);")?;
        tx.execute_batch("INSERT OR IGNORE INTO browse_dirty SELECT owner,recipient FROM sent_mail UNION SELECT owner,recipient FROM manual_conversations UNION SELECT owner,recipient FROM received_replies;")?;
    }
    tx.commit()?;
    Ok(())
}

/// Warm reads take only a read transaction. Rebuilds and the subsequent read
/// share a snapshot, so deletion/edit invalidation cannot race a page response.
pub fn read<T>(
    db: &Connection,
    work: &Work,
    query: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let mut tx = Transaction::new_unchecked(db, TransactionBehavior::Deferred)?;
    if db::exists(&tx, "SELECT 1 FROM browse_dirty LIMIT 1", [])? {
        drop(tx);
        tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
        let dirty = db::rows(&tx, "SELECT owner,recipient FROM browse_dirty", [])?;
        for contact in dirty {
            work.check()?;
            rebuild(
                &tx,
                text(&contact, "owner"),
                text(&contact, "recipient"),
                work,
            )?;
        }
    }
    work.check()?;
    let result = query(&tx);
    tx.commit()?;
    result
}

fn rebuild(db: &Connection, owner: &str, recipient: &str, work: &Work) -> Result<()> {
    let data = gui::index_conversation(db, owner, recipient, work)?;
    let messages = array(&data, "messages");
    let items = array(&data, "timeline");
    let manual = db::one(
        db,
        "SELECT rowid,name,created_at FROM manual_conversations WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?;
    let label = db::one(
        db,
        "SELECT label,revision FROM contact_labels WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?;
    let archived_name = messages
        .iter()
        .rev()
        .filter_map(|m| m["context"]["recipient_names"][recipient].as_str())
        .find(|n| !n.is_empty())
        .unwrap_or("");
    let name = manual
        .as_ref()
        .map(|m| text(m, "name"))
        .filter(|n| !n.is_empty())
        .unwrap_or(archived_name);
    let active: Vec<_> = messages.iter().filter(|m| m["deleted"] != true).collect();
    let mail_ids: Vec<_> = active.iter().map(|m| m["id"].clone()).collect();
    let reply_ids: Vec<_> = active
        .iter()
        .filter(|m| m["has_reply"] == true)
        .map(|m| m["id"].clone())
        .collect();
    let mut reply_count = reply_ids.len();
    if reply_count == 0 {
        reply_count = items.iter().filter(|i| i["kind"] == "quote").count();
    }
    reply_count += items.iter().filter(|i| i["kind"] == "received").count();
    let purposes: BTreeSet<_> = active
        .iter()
        .flat_map(|m| array(m, "recipients"))
        .filter_map(|p| {
            crate::pipeline::PURPOSES
                .iter()
                .position(|v| *v == text(p, "purpose"))
                .map(|i| gui::PURPOSE_LABELS[i])
        })
        .collect();
    let empty = json!({"sent_at":manual.as_ref().map(|m| text(m,"created_at")).unwrap_or(""),"subject":"","body":""});
    let latest = items.last().unwrap_or(&empty);
    let latest_date = items
        .iter()
        .rev()
        .find(|i| i.get("sent_at").is_some())
        .unwrap_or(&empty);
    let summary = if gui::conversation_deleted(db, owner, recipient)?
        || (items.is_empty() && manual.is_none())
    {
        Value::Null
    } else {
        json!({"owner":owner,"recipient":recipient,"name":if name.is_empty(){recipient.split('@').next().unwrap_or("")}else{name},
            "label":label.as_ref().map(|l|text(l,"label")).unwrap_or(""),"label_revision":label.as_ref().map(|l|l["revision"].clone()).unwrap_or(json!(0)),
            "count":active.len(),"registered":manual.is_some(),"reply_count":reply_count,
            "latest":latest_date["sent_at"],"subject":latest["subject"],"preview":text(latest,"body").chars().take(140).collect::<String>(),"search_match":null,"matches":true})
    };
    let details = json!({"mail_ids":mail_ids,"reply_ids":reply_ids,"purposes":purposes,
        "sort_date":messages.last().map(|m|text(m,"sent_at")).unwrap_or(""),"sort_id":messages.last().map(|m|m["id"].clone()).unwrap_or(json!(0)),
        "manual_id":manual.as_ref().map(|m|m["rowid"].clone()).unwrap_or(json!(0))});
    db.execute("INSERT INTO browse_contacts VALUES (?,?,1,?,?) ON CONFLICT(owner,recipient) DO UPDATE SET revision=revision+1,summary=excluded.summary,details=excluded.details",params![owner,recipient,summary.to_string(),details.to_string()])?;
    db.execute(
        "DELETE FROM browse_items WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?;
    let mails: HashMap<_, _> = messages
        .iter()
        .map(|m| {
            (
                m["id"].as_i64().unwrap(),
                select(m, &["id", "subject", "revision", "deleted"]),
            )
        })
        .collect();
    let mut insert = db.prepare_cached("INSERT INTO browse_items VALUES (?,?,?,?,?,?,?,?)")?;
    for (position, item) in items.iter().enumerate() {
        work.check()?;
        let mail = item["source_ids"][0]
            .as_i64()
            .and_then(|id| mails.get(&id))
            .unwrap_or(&Value::Null);
        let body = fold(text(item, "body"));
        let subject = fold(text(item, "subject"));
        let combined = format!("{} {}", text(item, "subject"), text(item, "body"));
        let normalized = fold(&crate::context::readable(&combined));
        let search_text = (normalized != format!("{subject} {body}")).then_some(normalized);
        insert.execute(params![
            owner,
            recipient,
            position as i64,
            item.to_string(),
            mail.to_string(),
            body,
            subject,
            search_text
        ])?;
    }
    db.execute(
        "DELETE FROM browse_dirty WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?;
    Ok(())
}

pub fn conversations(
    db: &Connection,
    query: &str,
    replies: bool,
    label: &str,
    unlabeled: bool,
    work: &Work,
) -> Result<Value> {
    let query = fold(query).trim().to_owned();
    let colors: serde_json::Map<String, Value> = db::rows(db, "SELECT * FROM label_colors", [])?
        .into_iter()
        .map(|r| {
            (
                text(&r, "label").to_owned(),
                select(&r, &["color", "revision"]),
            )
        })
        .collect();
    let mut groups = Vec::new();
    let (mut all_ids, mut reply_ids, mut owners, mut labels) = (
        HashSet::new(),
        HashSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    let mut statement = db.prepare_cached("SELECT summary,details FROM browse_contacts ORDER BY json_extract(details,'$.sort_date') DESC,json_extract(details,'$.sort_id') DESC,json_extract(details,'$.manual_id')")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        work.check()?;
        let mut group: Value = serde_json::from_str(&row.get::<_, String>(0)?)?;
        if group.is_null() {
            continue;
        }
        let details: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
        all_ids.extend(array(&details, "mail_ids").iter().filter_map(Value::as_i64));
        reply_ids.extend(
            array(&details, "reply_ids")
                .iter()
                .filter_map(Value::as_i64),
        );
        owners.insert(text(&group, "owner").to_owned());
        let label_text = text(&group, "label").to_owned();
        if !label_text.is_empty() {
            labels.insert(label_text.clone());
        }
        group["label_color"] = json!(
            colors
                .get(&label_text)
                .map(|c| text(c, "color"))
                .unwrap_or("")
        );
        let selected = (!replies || group["reply_count"].as_u64().unwrap_or(0) > 0)
            && (if unlabeled {
                label_text.is_empty()
            } else {
                label.is_empty() || label_text == label
            });
        let mut matches = query.is_empty();
        if !query.is_empty() && selected {
            let owner = text(&group, "owner");
            let recipient = text(&group, "recipient");
            let item = db.query_row("SELECT item FROM browse_items WHERE owner=?1 AND recipient=?2 AND (instr(search_body,?3)>0 OR instr(search_subject,?3)>0) ORDER BY position LIMIT 1",params![owner,recipient,query],|r|r.get::<_,String>(0));
            let preview = match item {
                Ok(s) => gui::first_search_match(&[serde_json::from_str(&s)?], &query),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e.into()),
            };
            matches = [recipient, text(&group, "name"), &label_text]
                .iter()
                .any(|s| fold(&crate::context::readable(s)).contains(&query))
                || array(&details, "purposes")
                    .iter()
                    .any(|p| fold(p.as_str().unwrap_or("")).contains(&query))
                || db::exists(
                    db,
                    "SELECT 1 FROM browse_items WHERE owner=?1 AND recipient=?2 AND instr(coalesce(search_text,search_subject||' '||search_body),?3)>0 LIMIT 1",
                    params![owner, recipient, query],
                )?;
            group["search_match"] = preview.unwrap_or(Value::Null);
        }
        group["matches"] = json!(matches && selected);
        groups.push(group);
    }
    groups.sort_by_key(|g| {
        std::cmp::Reverse(
            chrono::DateTime::parse_from_rfc3339(text(g, "latest"))
                .map(|d| d.timestamp_micros())
                .unwrap_or(0),
        )
    });
    Ok(
        json!({"total_mails":all_ids.len(),"total_conversations":groups.len(),"reply_mails":reply_ids.len(),"owners":owners,"labels":labels,"label_colors":colors,"items":groups.into_iter().filter(|g|g["matches"]==true).collect::<Vec<_>>()}),
    )
}

#[derive(Default)]
pub struct Page {
    pub before: Option<usize>,
    pub limit: usize,
    pub match_index: Option<usize>,
    pub message_key: Option<String>,
    pub revision: Option<i64>,
}

fn ordered_matches(item: &Value, query: &str, work: &Work) -> Result<Vec<Value>> {
    let mut matches = gui::search_matches_checked(std::slice::from_ref(item), query, work)?;
    // The browser displays a subject before its message body.
    matches.sort_by_key(|m| m["field"] != "subject");
    Ok(matches)
}

pub fn conversation_page(
    db: &Connection,
    owner: &str,
    recipient: &str,
    query: &str,
    page: Page,
    work: &Work,
) -> Result<Value> {
    let owner = privacy::person_id(owner);
    let recipient = privacy::person_id(recipient);
    let revision = db
        .query_row(
            "SELECT revision FROM browse_contacts WHERE owner=? AND recipient=?",
            params![owner, recipient],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0);
    if page.revision.is_some_and(|r| r != revision) {
        return Err(crate::fault(
            409,
            "대화 내용이 변경되었습니다. 대화를 다시 불러오세요.",
        ));
    }
    let total: usize = db.query_row(
        "SELECT count(*) FROM browse_items WHERE owner=? AND recipient=?",
        params![owner, recipient],
        |r| r.get(0),
    )?;
    let query = fold(query.trim());
    let limit = page.limit.clamp(1, 100);
    let mut counts = Vec::new();
    let mut match_total = 0;
    if !query.is_empty() {
        let mut statement=db.prepare_cached("SELECT position,item FROM browse_items WHERE owner=?1 AND recipient=?2 AND (instr(search_body,?3)>0 OR instr(search_subject,?3)>0) ORDER BY position")?;
        let mut rows = statement.query(params![owner, recipient, query])?;
        while let Some(row) = rows.next()? {
            work.check()?;
            let item: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
            let count = ordered_matches(&item, &query, work)?.len();
            counts.push((row.get::<_, usize>(0)?, match_total, count));
            match_total += count;
        }
    }
    let message_position = page.message_key.as_ref().map(|key| -> Result<usize> {
        db.query_row("SELECT position FROM browse_items, json_each(browse_items.item,'$.keys') AS k WHERE owner=? AND recipient=? AND k.value=?", params![owner, recipient, key], |row| row.get(0))
            .optional()?.ok_or_else(|| crate::fault(404, "메시지를 찾을 수 없습니다. 대화를 다시 여세요."))
    }).transpose()?;
    let target = if message_position.is_none()
        && match_total > 0
        && (page.match_index.is_some() || page.before.is_none())
    {
        Some(page.match_index.unwrap_or(0) % match_total)
    } else {
        None
    };
    let end = if let Some(position) = message_position {
        ((position / limit + 1) * limit).min(total)
    } else if let Some(target) = target {
        let position = counts
            .iter()
            .find(|(_, start, count)| target >= *start && target < start + count)
            .unwrap()
            .0;
        ((position / limit + 1) * limit).min(total)
    } else {
        page.before.unwrap_or(total).min(total)
    };
    let start = end.saturating_sub(limit);
    let mut timeline = Vec::new();
    let mut messages = Vec::new();
    let mut seen = HashSet::new();
    let mut matches = Vec::new();
    let mut statement=db.prepare_cached("SELECT position,item,mail FROM browse_items WHERE owner=?1 AND recipient=?2 AND position>=?3 AND position<?4 ORDER BY position")?;
    let mut rows = statement.query(params![owner, recipient, start as i64, end as i64])?;
    while let Some(row) = rows.next()? {
        work.check()?;
        let position = row.get::<_, usize>(0)?;
        let item: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
        let mail: Value = serde_json::from_str(&row.get::<_, String>(2)?)?;
        if let Some(id) = mail["id"].as_i64()
            && seen.insert(id)
        {
            messages.push(mail);
        }
        if let Some((_, ordinal, _)) = counts.iter().find(|(p, _, _)| *p == position) {
            for (index, mut m) in ordered_matches(&item, &query, work)?
                .into_iter()
                .enumerate()
            {
                m["index"] = json!(ordinal + index);
                matches.push(m);
            }
        }
        timeline.push(item);
    }
    Ok(
        json!({"messages":messages,"timeline":timeline,"search_matches":matches,"match_total":match_total,"match_index":target,"page":{"start":start,"end":end,"total":total,"revision":revision}}),
    )
}
