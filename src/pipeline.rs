use crate::{
    context::mail_datetime,
    db,
    llm::{LanguageModel, object_schema},
    mail::{Mail, parse_mail, read_source},
    select, text,
};
use anyhow::{Result, bail};
use chrono::NaiveDate;
use rusqlite::{Connection, named_params, params};
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf};

pub const PURPOSES: [&str; 8] = [
    "request",
    "reply",
    "report",
    "schedule",
    "thanks",
    "apology",
    "follow_up",
    "other",
];
pub const RELATIONSHIPS: [&str; 10] = [
    "superior",
    "peer",
    "subordinate",
    "client",
    "vendor",
    "academic_advisor",
    "student",
    "personal",
    "external",
    "unknown",
];
pub const CLASSIFY: &str = include_str!("../assets/classify.txt");
pub const GENERATE: &str = include_str!("../assets/generate.txt");
pub const STYLE_FIELDS: [&str; 5] = [
    "recipient",
    "role",
    "relationship",
    "purpose",
    "authored_body",
];
pub fn profile_schema() -> Value {
    object_schema(
        json!({"purpose":{"type":"string","enum":PURPOSES},"role":{"type":"string"},"role_evidence":{"type":"string"},"relationship":{"type":"string","enum":RELATIONSHIPS},"relationship_evidence":{"type":"string"}}),
    )
}
pub fn draft_schema() -> Value {
    object_schema(
        json!({"subject":{"type":"string"},"body":{"type":"string"},"missing_facts":{"type":"array","items":{"type":"string"}}}),
    )
}
pub fn classify(
    llm: &mut dyn LanguageModel,
    mail: &Mail,
    owner: &str,
    recipient: &str,
    request: &str,
) -> Result<Value> {
    let mut profile = llm.complete(CLASSIFY,&json!({"user":owner,"recipient":recipient,"conversation":mail.conversation(),"authored_body":mail.authored_body,"request":request}),&profile_schema())?;
    if !PURPOSES.contains(&text(&profile, "purpose"))
        || !RELATIONSHIPS.contains(&text(&profile, "relationship"))
    {
        bail!("모델이 유효하지 않은 목적 또는 관계를 반환했습니다.");
    }
    for field in ["role", "relationship"] {
        let key = format!("{field}_evidence");
        if text(&profile, field).trim().is_empty() {
            bail!("모델이 유효하지 않은 {field} 값을 반환했습니다.");
        }
        let evidence = text(&profile, &key);
        if evidence.trim().is_empty() || !mail.body.contains(evidence) {
            profile[field] = json!("unknown");
            profile[&key] = json!("");
        }
    }
    Ok(profile)
}
pub fn ingest(
    db: &mut Connection,
    llm: &mut dyn LanguageModel,
    paths: impl IntoIterator<Item = Result<PathBuf>>,
    owner: &str,
    aliases: &[String],
    since: Option<NaiveDate>,
) -> Result<Value> {
    let owner = owner.trim().to_lowercase();
    let mut own: HashSet<_> = aliases.iter().map(|s| s.trim().to_lowercase()).collect();
    own.insert(owner.clone());
    let mut counts = json!({"indexed":0,"skipped":0});
    if since.is_some() {
        counts["excluded_before_date"] = json!(0);
        counts["excluded_unknown_date"] = json!(0);
    }
    for path in paths {
        let path = path?;
        let mail = parse_mail(&read_source(&path)?)?;
        let sent_at = mail_datetime(&mail.date);
        if let Some(since) = since
            && sent_at.is_none_or(|d| d.date_naive() < since)
        {
            increment(&mut counts, "skipped");
            increment(
                &mut counts,
                if sent_at.is_none() {
                    "excluded_unknown_date"
                } else {
                    "excluded_before_date"
                },
            );
            continue;
        }
        let mut seen = HashSet::new();
        let recipients: Vec<_> = mail
            .to
            .iter()
            .chain(&mail.cc)
            .filter(|r| !own.contains(*r) && seen.insert((*r).clone()))
            .collect();
        if !own.contains(&mail.sender) || mail.authored_body.is_empty() || recipients.is_empty() {
            increment(&mut counts, "skipped");
            continue;
        }
        let mut pending = Vec::new();
        for recipient in recipients {
            if !db::exists(
                db,
                "SELECT 1 FROM sent_mail WHERE owner=? AND fingerprint=? AND recipient=?",
                params![owner, mail.fingerprint, recipient],
            )? {
                pending.push(recipient);
            }
        }
        if pending.is_empty() {
            db.execute(
                "UPDATE mail_archive SET context_json=? WHERE owner=? AND fingerprint=?",
                params![mail.context.to_string(), owner, mail.fingerprint],
            )?;
            increment(&mut counts, "skipped");
            continue;
        }
        let mut profiles = Vec::new();
        for recipient in &pending {
            let mut profile = classify(llm, &mail, &owner, recipient, "")?;
            if let Some(label) = db::one(
                db,
                "SELECT label FROM contact_labels WHERE owner=? AND recipient=?",
                params![owner, recipient],
            )? {
                profile["role"] = json!(if text(&label, "label").is_empty() {
                    "unknown"
                } else {
                    text(&label, "label")
                });
            }
            profiles.push(profile);
        }
        let tx = db.transaction()?;
        for (recipient, profile) in pending.iter().zip(&profiles) {
            tx.execute(
                "INSERT INTO sent_mail VALUES (?,?,?,?,?,?,?,?,?)",
                params![
                    owner,
                    mail.fingerprint,
                    mail.message_id,
                    mail.sender,
                    recipient,
                    text(profile, "role"),
                    text(profile, "relationship"),
                    text(profile, "purpose"),
                    mail.authored_body
                ],
            )?;
        }
        tx.execute("INSERT OR IGNORE INTO mail_archive (owner,fingerprint,subject,sent_at,sender,to_json,cc_json,original_body,original_authored_body,source_name,context_json) VALUES (?,?,?,?,?,?,?,?,?,?,?)",params![owner,mail.fingerprint,mail.subject,sent_at.map(|s| s.to_rfc3339()).unwrap_or_default(),mail.sender,serde_json::to_string(&mail.to)?,serde_json::to_string(&mail.cc)?,mail.body,mail.authored_body,path.file_name().unwrap_or_default().to_string_lossy(),mail.context.to_string()])?;
        tx.commit()?;
        increment(&mut counts, "indexed");
    }
    Ok(counts)
}
fn increment(value: &mut Value, key: &str) {
    value[key] = json!(value[key].as_u64().unwrap_or(0) + 1);
}
pub fn retrieve(
    db: &Connection,
    owner: &str,
    recipient: &str,
    profile: &Value,
    limit: usize,
) -> Result<Vec<Value>> {
    if limit < 1 {
        bail!("검색 개수는 1 이상이어야 합니다.");
    }
    let mut statement = db.prepare_cached("SELECT *, recipient=:recipient AS same_recipient,
        (relationship=:relationship AND :relationship!='unknown') AS same_relationship,
        (role=:role AND :role!='unknown') AS same_role, (purpose=:purpose AND :purpose!='other') AS same_purpose
        FROM sent_mail WHERE owner=:owner AND NOT EXISTS (SELECT 1 FROM deleted_messages d WHERE d.owner=sent_mail.owner AND d.message_key='sent:'||sent_mail.fingerprint)
        AND (recipient=:recipient OR (relationship=:relationship AND :relationship!='unknown') OR (role=:role AND :role!='unknown') OR (purpose=:purpose AND :purpose!='other'))
        ORDER BY same_recipient DESC,same_relationship DESC,same_role DESC,same_purpose DESC,fingerprint,recipient")?;
    let mut rows = statement.query(named_params! {":owner":owner.trim().to_lowercase(),":recipient":recipient.trim().to_lowercase(),":role":text(profile,"role"),":relationship":text(profile,"relationship"),":purpose":text(profile,"purpose")})?;
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    while let Some(row) = rows.next()? {
        let fingerprint: String = row.get("fingerprint")?;
        if seen.insert(fingerprint) {
            results.push(db::row_value(row)?);
            if results.len() == limit {
                break;
            }
        }
    }
    Ok(results)
}
pub struct DraftOptions<'a> {
    pub owner: &'a str,
    pub recipient: &'a str,
    pub request: &'a str,
    pub role: Option<&'a str>,
    pub relationship: Option<&'a str>,
    pub limit: usize,
}
pub fn draft(
    db: &Connection,
    llm: &mut dyn LanguageModel,
    raw: &[u8],
    options: DraftOptions<'_>,
) -> Result<Value> {
    let DraftOptions {
        owner,
        recipient,
        request,
        mut role,
        relationship,
        limit,
    } = options;
    if request.trim().is_empty() {
        bail!("작성할 메일의 요청 내용을 입력하세요.");
    }
    let mail = parse_mail(raw)?;
    let owner = owner.trim().to_lowercase();
    let recipient = recipient.trim().to_lowercase();
    let mut profile = classify(llm, &mail, &owner, &recipient, request)?;
    let label = db::one(
        db,
        "SELECT label FROM contact_labels WHERE owner=? AND recipient=?",
        params![owner, recipient],
    )?;
    let mut role_evidence = "사용자 지정 (--role)";
    if role.is_none()
        && let Some(label) = &label
    {
        role = Some(if text(label, "label").is_empty() {
            "unknown"
        } else {
            text(label, "label")
        });
        role_evidence = "사용자 지정 직급 라벨";
    }
    if let Some(role) = role {
        if role.trim().is_empty() {
            bail!("직책은 빈 문자열일 수 없습니다.");
        }
        profile["role"] = json!(role.trim());
        profile["role_evidence"] = json!(role_evidence);
    }
    if let Some(relationship) = relationship {
        if !RELATIONSHIPS.contains(&relationship) {
            bail!("지원하지 않는 관계입니다.");
        }
        profile["relationship"] = json!(relationship);
        profile["relationship_evidence"] = json!("사용자 지정 (--relationship)");
    }
    let references = retrieve(db, &owner, &recipient, &profile, limit)?;
    let mut result = llm.complete(GENERATE,&json!({"current_facts":{"user":owner,"recipient":recipient,"request":request,"conversation":mail.conversation()},"recipient_context":select(&profile,&["role","relationship","purpose"]),"style_references":references.iter().map(|r| select(r,&STYLE_FIELDS)).collect::<Vec<_>>()}),&draft_schema())?;
    if ["subject", "body"]
        .iter()
        .any(|k| text(&result, k).trim().is_empty())
    {
        bail!("모델이 비어 있는 제목 또는 본문을 반환했습니다.");
    }
    if !result["missing_facts"]
        .as_array()
        .is_some_and(|a| a.iter().all(Value::is_string))
    {
        bail!("모델이 유효하지 않은 누락 정보 목록을 반환했습니다.");
    }
    result["classification"] = profile;
    result["style_sources"] = references
        .iter()
        .map(|r| select(r, &["fingerprint", "message_id"]))
        .collect();
    Ok(result)
}
