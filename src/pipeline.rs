use crate::{
    context::mail_datetime,
    db,
    drafting::{self, GENERATE},
    llm::{LanguageModel, object_schema},
    mail::{Mail, parse_mail, read_source},
    privacy, select, text,
};
use anyhow::{Result, bail};
use chrono::NaiveDate;
use rusqlite::{Connection, params};
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

pub const CLASSIFY: &str = include_str!("../prompts/classify.txt");
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
    let owner_id = privacy::person_id(&owner);
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
            if crate::archive::conversation_deleted(db, &owner_id, &privacy::person_id(recipient))?
            {
                continue;
            }
            if !db::exists(
                db,
                "SELECT 1 FROM sent_mail WHERE owner=? AND fingerprint=? AND recipient=?",
                params![owner_id, mail.fingerprint, privacy::person_id(recipient)],
            )? {
                pending.push(recipient);
            }
        }
        if pending.is_empty() {
            db.execute(
                "UPDATE mail_archive SET context_json=? WHERE owner=? AND fingerprint=?",
                params![
                    privacy::stored_mail(&mail).context.to_string(),
                    owner_id,
                    mail.fingerprint
                ],
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
                params![owner_id, privacy::person_id(recipient)],
            )? {
                profile["role"] = json!(if text(&label, "label").is_empty() {
                    "unknown"
                } else {
                    text(&label, "label")
                });
            }
            profiles.push(profile);
        }
        let stored = privacy::stored_mail(&mail);
        let tx = db.transaction()?;
        let mut inserted = false;
        for (recipient, profile) in pending.iter().zip(&profiles) {
            if crate::archive::conversation_deleted(&tx, &owner_id, &privacy::person_id(recipient))?
            {
                continue;
            }
            tx.execute(
                "INSERT INTO sent_mail VALUES (?,?,?,?,?,?,?,?,?)",
                params![
                    owner_id,
                    stored.fingerprint,
                    stored.message_id,
                    stored.sender,
                    privacy::person_id(recipient),
                    privacy::redact(text(profile, "role")),
                    text(profile, "relationship"),
                    text(profile, "purpose"),
                    stored.authored_body
                ],
            )?;
            inserted = true;
        }
        if !inserted {
            tx.commit()?;
            increment(&mut counts, "skipped");
            continue;
        }
        tx.execute("INSERT OR IGNORE INTO mail_archive (owner,fingerprint,subject,sent_at,sender,to_json,cc_json,original_body,original_authored_body,source_name,context_json) VALUES (?,?,?,?,?,?,?,?,?,?,?)",params![owner_id,stored.fingerprint,stored.subject,sent_at.map(|s| s.to_rfc3339()).unwrap_or_default(),stored.sender,serde_json::to_string(&stored.to)?,serde_json::to_string(&stored.cc)?,stored.body,stored.authored_body,privacy::redact(&path.file_name().unwrap_or_default().to_string_lossy()),stored.context.to_string()])?;
        tx.commit()?;
        increment(&mut counts, "indexed");
    }
    Ok(counts)
}
fn increment(value: &mut Value, key: &str) {
    value[key] = json!(value[key].as_u64().unwrap_or(0) + 1);
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
    let owner = privacy::person_id(&owner);
    let recipient = privacy::person_id(&recipient);
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
    if limit < 1 {
        bail!("검색 개수는 1 이상이어야 합니다.");
    }
    let mut context = drafting::context_with_profile(db, &owner, &recipient, &profile, &[], &[])?;
    context["current_facts"]["request"] = json!(request);
    context["current_facts"]["conversation"] = mail.conversation();
    if !crate::array(&context, "history_candidates").is_empty()
        || !crate::array(&context, "style_candidates").is_empty()
    {
        let schema = object_schema(json!({
            "history_keys":{"type":"array","items":{"type":"string"}},
            "style_ids":{"type":"array","items":{"type":"string"},"maxItems":limit}
        }));
        let choice = llm.complete(text(&context, "selection_instructions"), &context, &schema)?;
        let ids = |key| -> Result<Vec<String>> {
            choice[key]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("참조 선택은 배열이어야 합니다."))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow::anyhow!("참조 식별자는 문자열이어야 합니다."))
                })
                .collect()
        };
        let history_keys = ids("history_keys")?;
        let style_ids = ids("style_ids")?;
        if style_ids.len() > limit {
            bail!("선택한 문체 예시가 검색 개수를 초과했습니다.");
        }
        let selected = drafting::context_with_profile(
            db,
            &owner,
            &recipient,
            &profile,
            &history_keys,
            &style_ids,
        )?;
        context["current_facts"]["previous_conversation"] =
            selected["current_facts"]["conversation"].clone();
        context["style_references"] = selected["style_references"].clone();
    }
    let references = context["style_references"].clone();
    let input = select(
        &context,
        &["current_facts", "recipient_context", "style_references"],
    );
    let mut result = llm.complete(GENERATE, &input, &draft_schema())?;
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
        .as_array()
        .unwrap()
        .iter()
        .map(|r| select(r, &["fingerprint", "message_id"]))
        .collect();
    Ok(result)
}
