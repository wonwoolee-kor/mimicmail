mod common;
use common::*;
use mimicmail::{array, context, db, gui, mail};
use serde_json::{Value, json};
use std::collections::HashSet;

#[test]
fn mime_and_quotes_match_python_fixtures() {
    let cases: Value = serde_json::from_str(include_str!("fixtures/mail.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let actual = serde_json::to_value(
            mail::parse_mail(case["raw"].as_str().unwrap().as_bytes()).unwrap(),
        )
        .unwrap();
        assert_eq!(actual, case["expected"], "case {}", case["name"]);
    }
}
fn anonymized(value: &Value, field: &str) -> Value {
    use mimicmail::privacy::{person_id, redact};
    match value {
        Value::String(s)
            if matches!(
                field,
                "owner" | "recipient" | "sender" | "user" | "to" | "cc" | "owners"
            ) =>
        {
            json!(person_id(s))
        }
        Value::String(s) => json!(redact(s)),
        Value::Array(items) => items.iter().map(|v| anonymized(v, field)).collect(),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| {
                (
                    if field == "recipient_names" {
                        person_id(k)
                    } else {
                        k.clone()
                    },
                    anonymized(v, k),
                )
            })
            .collect(),
        _ => value.clone(),
    }
}

#[test]
fn existing_database_search_edit_labels_and_deletion_match_python() {
    let fixture = legacy();
    let db = memory();
    load_tables(&db, &fixture["tables"]);
    for state in fixture["states"].as_array().unwrap() {
        match state["action"].as_str() {
            Some("edit") => {
                gui::update_mail(
                    &db,
                    state["id"].as_i64().unwrap(),
                    &anonymized(&state["payload"], ""),
                )
                .unwrap();
            }
            Some("label") => {
                gui::update_contact_label(&db, &state["payload"]).unwrap();
            }
            Some("delete") => {
                gui::delete_message(&db, &state["payload"]).unwrap();
            }
            _ => {}
        }
        for (key, expected) in state["expected"].as_object().unwrap() {
            let mut actual = match key.as_str() {
                "list" => gui::conversations(&db, "", false, "").unwrap(),
                "conversation" => {
                    gui::conversation(&db, "me@example.com", "boss@example.com").unwrap()
                }
                "context" => {
                    gui::draft_context(&db, "me@example.com", "boss@example.com", "other").unwrap()
                }
                "replies" => gui::conversations(&db, "", true, "").unwrap(),
                key => gui::conversations(&db, key.strip_prefix("search:").unwrap(), false, "")
                    .unwrap(),
            };
            if key == "context" {
                assert_eq!(actual["current_facts"]["conversation"], json!([]));
                assert_eq!(actual["style_references"], json!([]));
                assert_eq!(actual["recipient_context"], expected["recipient_context"]);
                let keys: Vec<_> = array(&actual, "history_candidates")
                    .iter()
                    .map(|i| i["key"].as_str().unwrap().to_owned())
                    .collect();
                let mut selected = gui::selected_context(
                    &db,
                    "me@example.com",
                    "boss@example.com",
                    "other",
                    &keys,
                    &[],
                )
                .unwrap();
                for fact in selected["current_facts"]["conversation"]
                    .as_array_mut()
                    .unwrap()
                {
                    fact.as_object_mut().unwrap().remove("key");
                }
                selected["current_facts"]
                    .as_object_mut()
                    .unwrap()
                    .remove("recipient_name");
                assert_eq!(
                    selected["current_facts"],
                    anonymized(&expected["current_facts"], "")
                );
                continue;
            }
            actual.as_object_mut().unwrap().remove("label_colors");
            if let Some(items) = actual.get_mut("items").and_then(Value::as_array_mut) {
                for item in items {
                    item.as_object_mut().unwrap().remove("label_color");
                }
            }
            let mut expected = anonymized(expected, "");
            if let Some(messages) = actual.get_mut("messages").and_then(Value::as_array_mut) {
                for mail in messages {
                    if let Some(quotes) = mail["context"]["quotes"].as_array_mut() {
                        for quote in quotes {
                            quote.as_object_mut().unwrap().remove("key");
                        }
                    }
                }
            }
            clean(&mut actual);
            clean(&mut expected);
            assert_eq!(actual, expected, "state {} {key}", state["action"]);
        }
    }
}
#[test]
fn persisted_python_deletion_keys_are_honored() {
    let db = memory();
    load_tables(
        &db,
        &serde_json::from_str(include_str!("fixtures/deleted.json")).unwrap(),
    );
    let data = gui::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    assert!(array(&data, "timeline").is_empty());
    assert_eq!(
        gui::conversations(&db, "", false, "").unwrap()["total_mails"],
        1
    );
}
#[test]
fn read_only_context_does_not_write_or_create_database() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let db = db::connect(&path).unwrap();
    load_tables(&db, &legacy()["tables"]);
    drop(db);
    let before = std::fs::read(&path).unwrap();
    let db = db::open(&path, true).unwrap();
    assert_eq!(gui::draft_context(&db,"me@example.com","boss@example.com","other").unwrap()["current_facts"]["conversation"].as_array().unwrap().len(),0);
    drop(db);
    assert_eq!(before, std::fs::read(&path).unwrap());
    let missing = tmp.path().join("missing.sqlite3");
    assert!(db::open(&missing, true).is_err());
    assert!(!missing.exists());
}
#[test]
fn legacy_schema_without_context_column_is_upgraded_once() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../assets/schema.sql"))
        .unwrap();
    db::initialize(&db).unwrap();
    db::initialize(&db).unwrap();
    assert_eq!(
        db::rows(&db, "PRAGMA table_info(mail_archive)", [])
            .unwrap()
            .iter()
            .filter(|r| r["name"] == "context_json")
            .count(),
        1
    );
}
#[test]
fn timeline_deduplicates_same_instant_but_preserves_repeated_messages() {
    let sent = |id, stamp, body, quotes| json!({"id":id,"sent_at":stamp,"subject":"RE: 일정","display_body":if id==1{"편집한 문체"}else{body},"original_authored_body":body,"has_reply":false,"context":{"quotes":quotes}});
    let own = json!({"sender":"me@example.com","sender_name":"나","sent_at":"2026-01-01T10:00:01+09:00","body":"확인했습니다.\n감사합니다."});
    let reply = json!({"sender":"boss@example.com","sender_name":"교수","sent_at":"2026-01-02T10:00:00+09:00","body":"자료도 보내주세요."});
    let messages = vec![
        sent(
            1,
            "2026-01-01T10:00:00+09:00",
            "확인했습니다. 감사합니다.",
            json!([]),
        ),
        sent(
            2,
            "2026-01-03T10:00:00+09:00",
            "확인했습니다. 감사합니다.",
            json!([reply, own]),
        ),
        sent(
            3,
            "2026-01-04T10:00:00+09:00",
            "자료 보냅니다.",
            json!([reply, own]),
        ),
    ];
    let data = context::timeline(&messages, "me@example.com", &HashSet::new());
    assert_eq!(
        data.iter().map(|m| m["kind"].clone()).collect::<Vec<_>>(),
        vec![json!("sent"), json!("quote"), json!("sent"), json!("sent")]
    );
    assert_eq!(data[0]["source_ids"], json!([1, 2, 3]));
    assert_eq!(data[1]["source_ids"], json!([2, 3]));
    assert_eq!(data[0]["body"], "편집한 문체");
}
#[test]
fn unknown_quote_authors_are_not_invented() {
    let quotes = context::quoted_messages("> 相手の質問\n私の返事", "私の返事").unwrap();
    assert_eq!(quotes[0]["sender"], "");
    assert_eq!(quotes[0]["body"], "相手の質問");
}
#[test]
fn malformed_sender_attachment_only_and_input_limit_fail() {
    for raw in [
        b"From: a@example.com, b@example.com\n\nbody".as_slice(),
        b"From: a@example.com\nContent-Type: text/plain\nContent-Disposition: attachment\n\nsecret",
    ] {
        assert!(mail::parse_mail(raw).is_err());
    }
    assert!(mail::read_limited("12345".as_bytes(), 4).is_err());
}
#[test]
fn korea_date_boundary_and_unknown_timezone() {
    assert_eq!(
        context::mail_datetime("Mon, 01 Jun 1987 15:00:00 +0000")
            .unwrap()
            .to_rfc3339(),
        "1987-06-02T01:00:00+10:00"
    );
    assert_eq!(
        context::mail_datetime("Wed, 31 Dec 2025 15:00:00 +0000")
            .unwrap()
            .to_rfc3339(),
        "2026-01-01T00:00:00+09:00"
    );
    for value in [
        "",
        "garbage",
        "Thu, 01 Jan 2026 00:00:00",
        "Thu, 01 Jan 2026 00:00:00 -0000",
    ] {
        assert!(context::mail_datetime(value).is_none(), "{value}");
    }
}
#[test]
fn manual_conversation_persists_and_reuses_contacts() {
    let db = populated();
    let payload = json!({"owner":"ME@example.com","name":"새 수신자","label":"교수"});
    let first = gui::create_conversation(&db, &payload).unwrap();
    assert_eq!(first["created"], true);
    assert_eq!(first["contact"]["count"], 0);
    assert_eq!(first["contact"]["name"], "새 수신자");
    assert_eq!(
        gui::create_conversation(&db, &payload).unwrap()["created"],
        false
    );
    assert_eq!(
        db::one(&db, "SELECT count(*) AS n FROM sent_mail", [])
            .unwrap()
            .unwrap()["n"],
        2
    );
    assert_eq!(
        gui::conversations(&db, "", false, "교수").unwrap()["items"][0]["recipient"],
        first["contact"]["recipient"]
    );
}
#[test]
fn invalid_mutations_leave_database_unchanged_and_conflicts_fail() {
    let db = populated();
    let mail = gui::get_mail(&db, 1).unwrap();
    let payload = json!({"revision":0,"authored_body":"내 문체","recipients":mail["recipients"]});
    for (key, value) in [
        ("authored_body", json!(" ")),
        ("recipients", json!([])),
        ("revision", json!("0")),
    ] {
        let mut invalid = payload.clone();
        invalid[key] = value;
        assert!(gui::update_mail(&db, 1, &invalid).is_err());
    }
    assert_eq!(gui::get_mail(&db, 1).unwrap()["revision"], 0);
    gui::update_mail(&db, 1, &payload).unwrap();
    assert_eq!(
        gui::update_mail(&db, 1, &payload)
            .unwrap_err()
            .downcast_ref::<mimicmail::Fault>()
            .unwrap()
            .0,
        409
    );
    let label = json!({"owner":"me@example.com","recipient":"boss@example.com","label":"교수","revision":0});
    gui::update_contact_label(&db, &label).unwrap();
    assert!(gui::update_contact_label(&db, &label).is_err());
}
#[test]
fn new_recipient_style_does_not_copy_other_conversation_facts() {
    let db = populated();
    gui::update_contact_label(&db,&json!({"owner":"me@example.com","recipient":"boss@example.com","label":"교수","revision":0})).unwrap();
    let created = gui::create_conversation(
        &db,
        &json!({"owner":"me@example.com","name":"새 수신자","label":"교수"}),
    )
    .unwrap();
    let recipient = created["contact"]["recipient"].as_str().unwrap();
    let data = gui::draft_context(&db, "me@example.com", recipient, "other").unwrap();
    assert_eq!(data["current_facts"]["conversation"], json!([]));
    assert_eq!(
        data["style_candidates"][0]["recipient"],
        mimicmail::privacy::person_id("boss@example.com")
    );
    assert_eq!(data["style_references"], json!([]));
    let id = data["style_candidates"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let selected =
        gui::selected_context(&db, "me@example.com", recipient, "other", &[], &[id]).unwrap();
    assert_eq!(
        selected["style_references"][0]["recipient"],
        mimicmail::privacy::person_id("boss@example.com")
    );
    assert_eq!(selected["current_facts"]["conversation"], json!([]));
    assert_eq!(
        gui::draft_context(&db, "other@example.com", recipient, "other").unwrap()["style_references"],
        json!([])
    );
}
