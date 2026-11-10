mod common;
use common::*;
use mimicmail::{
    array, db, gui, mail,
    pipeline::{self, DraftOptions},
};
use serde_json::{Value, json};
use std::path::PathBuf;
fn files(tmp: &tempfile::TempDir, raw: &[u8], name: &str) -> Vec<anyhow::Result<PathBuf>> {
    let path = tmp.path().join(name);
    std::fs::write(&path, raw).unwrap();
    vec![Ok(path)]
}
#[test]
fn incoming_quote_only_and_self_mail_never_call_model() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = memory();
    let mut llm = Fake::new(vec![]);
    for (sender, to, body) in [
        ("boss@example.com", "me@example.com", "받은 메일"),
        ("me@example.com", "boss@example.com", "> quote"),
        ("me@example.com", "me@example.com", "나에게"),
    ] {
        let result = pipeline::ingest(
            &mut db,
            &mut llm,
            files(&tmp, &raw(sender, to, body, ""), "mail.eml"),
            "me@example.com",
            &[],
            None,
        )
        .unwrap();
        assert_eq!(result, json!({"indexed":0,"skipped":1}));
    }
    assert!(llm.calls.is_empty());
}
#[test]
fn aliases_multiple_recipients_and_repeat_ingestion_preserve_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = memory();
    let mut llm = Fake::new(vec![unknown(), unknown()]);
    let raw = raw(
        "alias@example.com",
        "boss@example.com,peer@example.com,me@example.com",
        "내 문체\n> 상대방",
        "Thu, 01 Jan 2026 10:00:00 +0900",
    );
    let result = pipeline::ingest(
        &mut db,
        &mut llm,
        files(&tmp, &raw, "sent.eml"),
        "ME@example.com",
        &["ALIAS@example.com".to_owned()],
        None,
    )
    .unwrap();
    assert_eq!(result, json!({"indexed":1,"skipped":0}));
    assert_eq!(llm.calls.len(), 2);
    let mail = gui::get_mail(&db, 1).unwrap();
    gui::update_mail(
        &db,
        1,
        &json!({"revision":0,"authored_body":"편집한 문체","recipients":mail["recipients"]}),
    )
    .unwrap();
    let result = pipeline::ingest(
        &mut db,
        &mut llm,
        files(&tmp, &raw, "sent.eml"),
        "me@example.com",
        &["alias@example.com".to_owned()],
        None,
    )
    .unwrap();
    assert_eq!(result, json!({"indexed":0,"skipped":1}));
    assert_eq!(
        gui::get_mail(&db, 1).unwrap()["authored_body"],
        "편집한 문체"
    );
    assert_eq!(
        pipeline::retrieve(&db, "me@example.com", "boss@example.com", &unknown(), 5)
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn failed_classification_leaves_no_partial_message() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = memory();
    let mut invalid = unknown();
    invalid["purpose"] = json!("invalid");
    let mut llm = Fake::new(vec![unknown(), invalid]);
    assert!(
        pipeline::ingest(
            &mut db,
            &mut llm,
            files(
                &tmp,
                &raw(
                    "me@example.com",
                    "boss@example.com,peer@example.com",
                    "내용",
                    ""
                ),
                "mail.eml"
            ),
            "me@example.com",
            &[],
            None
        )
        .is_err()
    );
    for table in ["sent_mail", "mail_archive"] {
        assert_eq!(
            db::one(&db, &format!("SELECT count(*) AS n FROM {table}"), [])
                .unwrap()
                .unwrap()["n"],
            0
        );
    }
}
#[test]
fn since_filters_korea_dates_before_classification() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = memory();
    let mut llm = Fake::new(vec![unknown(), unknown()]);
    let mut paths = vec![];
    for (i, date) in [
        "Wed, 31 Dec 2025 14:59:59 +0000",
        "Wed, 31 Dec 2025 15:00:00 +0000",
        "Wed, 31 Dec 2025 16:00:00 +0000",
        "",
        "Thu, 01 Jan 2026 00:00:00",
    ]
    .iter()
    .enumerate()
    {
        paths.extend(files(
            &tmp,
            &raw("me@example.com", "boss@example.com", "본문", date),
            &format!("{i}.eml"),
        ));
    }
    let result = pipeline::ingest(
        &mut db,
        &mut llm,
        paths,
        "me@example.com",
        &[],
        Some("2026-01-01".parse().unwrap()),
    )
    .unwrap();
    assert_eq!(
        result,
        json!({"indexed":2,"skipped":3,"excluded_before_date":1,"excluded_unknown_date":2})
    );
    assert_eq!(llm.calls.len(), 2);
}
#[test]
fn verbatim_evidence_is_required() {
    let mut profile = unknown();
    profile["role"] = json!("팀장");
    profile["role_evidence"] = json!("김 팀장님");
    profile["relationship"] = json!("superior");
    profile["relationship_evidence"] = json!("제 상사입니다");
    let mail = mail::parse_mail(&raw(
        "me@example.com",
        "boss@example.com",
        "김 팀장님, 검토 부탁드립니다.",
        "",
    ))
    .unwrap();
    let mut llm = Fake::new(vec![profile]);
    let result =
        pipeline::classify(&mut llm, &mail, "me@example.com", "boss@example.com", "").unwrap();
    assert_eq!(result["role"], "팀장");
    assert_eq!(result["relationship"], "unknown");
}
#[test]
fn retrieval_priority_owner_isolation_and_unknown_values() {
    let db = memory();
    for (fp, recipient, role, relation, purpose, owner) in [
        (
            "purpose",
            "other@example.com",
            "unknown",
            "unknown",
            "request",
            "me@example.com",
        ),
        (
            "role",
            "other@example.com",
            "팀장",
            "unknown",
            "thanks",
            "me@example.com",
        ),
        (
            "relation",
            "other@example.com",
            "unknown",
            "superior",
            "thanks",
            "me@example.com",
        ),
        (
            "recipient",
            "boss@example.com",
            "unknown",
            "unknown",
            "thanks",
            "me@example.com",
        ),
        (
            "another-owner",
            "boss@example.com",
            "unknown",
            "unknown",
            "request",
            "stranger@example.com",
        ),
    ] {
        db.execute(
            "INSERT INTO sent_mail VALUES (?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                owner,
                fp,
                fp,
                owner,
                recipient,
                role,
                relation,
                purpose,
                format!("style {fp}")
            ],
        )
        .unwrap();
    }
    let profile = json!({"role":"팀장","relationship":"superior","purpose":"request"});
    let found = pipeline::retrieve(&db, "me@example.com", "boss@example.com", &profile, 5).unwrap();
    assert_eq!(
        found
            .iter()
            .map(|r| r["fingerprint"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["recipient", "relation", "role", "purpose"]
    );
    assert!(
        pipeline::retrieve(
            &db,
            "me@example.com",
            "new@example.com",
            &json!({"role":"unknown","relationship":"unknown","purpose":"other"}),
            5
        )
        .unwrap()
        .is_empty()
    );
    assert!(pipeline::retrieve(&db, "me@example.com", "new@example.com", &profile, 0).is_err());
}
#[test]
fn draft_separates_current_facts_and_historical_style() {
    let db = populated();
    let mut llm = Fake::new(vec![
        unknown(),
        json!({"subject":"회의 확인","body":"화요일에 뵙겠습니다.","missing_facts":[]}),
    ]);
    let raw = raw(
        "boss@example.com",
        "me@example.com",
        "이번 회의는 화요일입니다.",
        "",
    );
    let result = pipeline::draft(
        &db,
        &mut llm,
        &raw,
        DraftOptions {
            owner: "me@example.com",
            recipient: "boss@example.com",
            request: "참석한다고 답장",
            role: Some("팀장"),
            relationship: Some("superior"),
            limit: 5,
        },
    )
    .unwrap();
    let data = &llm.calls[1].1;
    assert!(!data["style_references"].to_string().contains("화요일"));
    assert!(!data["current_facts"].to_string().contains("중요한 답변"));
    assert_eq!(data["recipient_context"]["relationship"], "superior");
    assert_eq!(
        result["classification"]["role_evidence"],
        "사용자 지정 (--role)"
    );
    assert_eq!(array(&result, "style_sources").len(), 2);
}
#[test]
fn manual_role_applies_to_future_ingest_and_draft_override() {
    let tmp = tempfile::tempdir().unwrap();
    let mut db = populated();
    gui::update_contact_label(&db,&json!({"owner":"me@example.com","recipient":"boss@example.com","label":"교수","revision":0})).unwrap();
    let raw = raw("me@example.com", "boss@example.com", "새 메일", "");
    let mut llm = Fake::new(vec![unknown()]);
    pipeline::ingest(
        &mut db,
        &mut llm,
        files(&tmp, &raw, "new.eml"),
        "me@example.com",
        &[],
        None,
    )
    .unwrap();
    assert_eq!(
        gui::get_mail(&db, 3).unwrap()["recipients"][0]["role"],
        "교수"
    );
    for (role, expected) in [(None, "교수"), (Some("학과장"), "학과장")] {
        let mut llm = Fake::new(vec![
            unknown(),
            json!({"subject":"답장","body":"감사합니다.","missing_facts":[]}),
        ]);
        pipeline::draft(
            &db,
            &mut llm,
            &raw,
            DraftOptions {
                owner: "me@example.com",
                recipient: "boss@example.com",
                request: "감사 인사",
                role,
                relationship: None,
                limit: 5,
            },
        )
        .unwrap();
        assert_eq!(llm.calls[1].1["recipient_context"]["role"], expected);
    }
}
#[test]
fn invalid_draft_output_and_empty_request_fail() {
    let db = memory();
    let raw = raw("boss@example.com", "me@example.com", "내용", "");
    for output in [
        json!({"subject":"","body":"text","missing_facts":[]}),
        json!({"subject":"s","body":"b","missing_facts":[1]}),
        Value::Null,
    ] {
        let mut llm = Fake::new(vec![unknown(), output]);
        assert!(
            pipeline::draft(
                &db,
                &mut llm,
                &raw,
                DraftOptions {
                    owner: "me@example.com",
                    recipient: "boss@example.com",
                    request: "답장",
                    role: None,
                    relationship: None,
                    limit: 5
                }
            )
            .is_err()
        );
    }
    let mut llm = Fake::new(vec![]);
    assert!(
        pipeline::draft(
            &db,
            &mut llm,
            &raw,
            DraftOptions {
                owner: "me@example.com",
                recipient: "boss@example.com",
                request: " ",
                role: None,
                relationship: None,
                limit: 5
            }
        )
        .is_err()
    );
    assert!(llm.calls.is_empty());
}
