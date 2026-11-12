use mimicmail::drafting;
use mimicmail::settings;
mod common;
use common::*;
use mimicmail::{
    db,
    pipeline::{self, DraftOptions},
};
use serde_json::json;

#[test]
fn context_survives_reopening_and_is_shared_between_conversations() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let db = db::connect(&path).unwrap();
    let entries = json!([{"key":"이름","value":"홍길동"},{"key":"소속","value":"개발팀"}]);
    settings::update(&db, &json!({"revision":0,"entries":entries})).unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    let db = db::open(&path, true).unwrap();
    assert_eq!(
        settings::get(&db).unwrap(),
        json!({"revision":1,"entries":entries})
    );
    for (owner, recipient) in [("local", "first"), ("other", "second")] {
        let context = drafting::selected_context(&db, owner, recipient, "other", &[], &[]).unwrap();
        assert_eq!(
            context["current_facts"]["general_context"],
            json!({"이름":"홍길동","소속":"개발팀"})
        );
    }
    assert_eq!(before, std::fs::read(&path).unwrap());
}

#[test]
fn invalid_and_stale_updates_preserve_saved_context() {
    let db = memory();
    let saved = settings::update(
        &db,
        &json!({"revision":0,"entries":[{"key":" 이름 ","value":" 홍길동 "}]}),
    )
    .unwrap();
    assert_eq!(saved["entries"], json!([{"key":"이름","value":"홍길동"}]));
    for payload in [
        json!({"revision":0,"entries":[]}),
        json!({"revision":1,"entries":[{"key":"이름","value":"A"},{"key":" 이름 ","value":"B"}]}),
        json!({"revision":1,"entries":[{"key":" ","value":"A"}]}),
        json!({"revision":1,"entries":[{"key":"이름","value":" "}]}),
        json!({"revision":1,"entries":[{"key":"이름","value":12}]}),
        json!({"revision":1,"entries":[{"key":"가".repeat(101),"value":"A"}]}),
        json!({"revision":1,"entries":[{"key":"이름","value":"가".repeat(10001)}]}),
        json!({"revision":1,"entries":{}}),
        json!({"entries":[]}),
    ] {
        assert!(settings::update(&db, &payload).is_err(), "{payload}");
        assert_eq!(settings::get(&db).unwrap(), saved);
    }
    let cleared = settings::update(&db, &json!({"revision":1,"entries":[]})).unwrap();
    assert_eq!(cleared, json!({"revision":2,"entries":[]}));
}

#[test]
fn context_redacts_email_addresses_and_supports_older_databases() {
    let db = memory();
    let saved = settings::update(
        &db,
        &json!({"revision":0,"entries":[{"key":"연락처","value":"홍길동 <me@example.com>"}]}),
    )
    .unwrap();
    assert!(!saved.to_string().contains("me@example.com"));
    assert!(saved.to_string().contains("[이메일 삭제됨]"));
    db.execute("DROP TABLE general_context", []).unwrap();
    assert_eq!(
        settings::get(&db).unwrap(),
        json!({"revision":0,"entries":[]})
    );
    assert_eq!(
        drafting::selected_context(&db, "local", "recipient", "other", &[], &[]).unwrap()["current_facts"]
            ["general_context"],
        json!({})
    );
}

#[test]
fn saved_context_reaches_draft_generation() {
    let db = memory();
    settings::update(
        &db,
        &json!({"revision":0,"entries":[{"key":"이름","value":"홍길동"}]}),
    )
    .unwrap();
    let mut llm = Fake::new(vec![
        unknown(),
        json!({"subject":"답장","body":"감사합니다.","missing_facts":[]}),
    ]);
    pipeline::draft(
        &db,
        &mut llm,
        &raw(
            "boss@example.com",
            "me@example.com",
            "확인 부탁드립니다.",
            "",
        ),
        DraftOptions {
            owner: "me@example.com",
            recipient: "boss@example.com",
            request: "감사 인사",
            role: None,
            relationship: None,
            limit: 5,
        },
    )
    .unwrap();
    assert_eq!(
        llm.calls.last().unwrap().1["current_facts"]["general_context"],
        json!({"이름":"홍길동"})
    );
}
