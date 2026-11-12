use mimicmail::drafting;
use mimicmail::search;
mod common;
use common::*;
use mimicmail::{archive, array, db, privacy, text};
use serde_json::json;

#[test]
fn replies_persist_as_searchable_facts_without_changing_sent_style() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let db = db::connect(&path).unwrap();
    load_tables(&db, &legacy()["tables"]);
    let before = archive::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    let styles =
        drafting::selected_context(&db, "me@example.com", "boss@example.com", "reply", &[], &[])
            .unwrap();
    let saved = archive::create_reply(&db, &json!({"owner":"me@example.com","recipient":"boss@example.com","body":"검토완료\n문의: boss@example.com"})).unwrap();
    let key = text(&saved, "key").to_owned();
    drop(db);
    let db = db::connect(&path).unwrap();
    let data = archive::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    assert_eq!(data["messages"], before["messages"]);
    let reply = array(&data, "timeline").last().unwrap();
    assert_eq!(reply["keys"], json!([key]));
    assert_eq!(reply["kind"], "received");
    assert_eq!(reply["mine"], false);
    assert_eq!(reply["sender"], privacy::person_id("boss@example.com"));
    assert_eq!(reply["sender_name"], "팀장");
    assert_eq!(reply["body"], "검토완료\n문의: [이메일 삭제됨]");
    assert!(reply.get("sent_at").is_none());
    assert!(array(reply, "source_ids").is_empty());
    let list = archive::conversations(&db, "검토완료", true, "", false).unwrap();
    assert_eq!(array(&list, "items").len(), 1);
    assert_eq!(list["items"][0]["search_match"]["key"], key);
    assert_eq!(
        search::matches(
            array(&data, "timeline"),
            "검토완료",
            usize::MAX,
            &mimicmail::work::Work::default()
        )
        .unwrap()
        .len(),
        1
    );
    let catalog =
        drafting::selected_context(&db, "me@example.com", "boss@example.com", "reply", &[], &[])
            .unwrap();
    assert_eq!(catalog["style_candidates"], styles["style_candidates"]);
    assert!(!catalog.to_string().contains("검토완료"));
    let selected = drafting::selected_context(
        &db,
        "me@example.com",
        "boss@example.com",
        "reply",
        std::slice::from_ref(&key),
        &[],
    )
    .unwrap();
    assert_eq!(
        selected["current_facts"]["conversation"][0]["body"],
        reply["body"]
    );
    assert!(array(&selected, "style_references").is_empty());
    for (owner, recipient) in [
        ("other@example.com", "boss@example.com"),
        ("me@example.com", "peer@example.com"),
    ] {
        let data = archive::conversation(&db, owner, recipient).unwrap();
        assert!(!data.to_string().contains("검토완료"));
        assert!(
            archive::delete_message(&db, &json!({"owner":owner,"recipient":recipient,"key":key}))
                .is_err()
        );
    }
    archive::delete_message(
        &db,
        &json!({"owner":"me@example.com","recipient":"boss@example.com","key":key}),
    )
    .unwrap();
    drop(db);
    let db = db::connect(&path).unwrap();
    assert_eq!(
        archive::conversation(&db, "me@example.com", "boss@example.com").unwrap(),
        before
    );
    assert!(
        array(
            &archive::conversations(&db, "검토완료", false, "", false).unwrap(),
            "items"
        )
        .is_empty()
    );
    assert!(
        drafting::selected_context(
            &db,
            "me@example.com",
            "boss@example.com",
            "reply",
            &[key],
            &[]
        )
        .is_err()
    );
    assert_eq!(
        db::rows(&db, "SELECT * FROM received_replies", [])
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn reply_only_contacts_keep_label_style_candidates_and_distinct_messages() {
    let db = populated();
    archive::update_contact_label(&db, &json!({"owner":"me@example.com","recipient":"boss@example.com","label":"교수","revision":0})).unwrap();
    let created = archive::create_conversation(
        &db,
        &json!({"owner":"me@example.com","name":"새 교수","label":"교수"}),
    )
    .unwrap();
    let recipient = text(&created["contact"], "recipient");
    let before =
        drafting::selected_context(&db, "me@example.com", recipient, "reply", &[], &[]).unwrap();
    assert!(!array(&before, "style_candidates").is_empty());
    let payload = json!({"owner":"me@example.com","recipient":recipient,"body":"확인했습니다."});
    let first = archive::create_reply(&db, &payload).unwrap();
    let second = archive::create_reply(&db, &payload).unwrap();
    assert_ne!(first["key"], second["key"]);
    let data = archive::conversation(&db, "me@example.com", recipient).unwrap();
    assert_eq!(data["timeline"][0]["sender_name"], "새 교수");
    assert_eq!(array(&data, "timeline").len(), 2);
    assert!(array(&data, "messages").is_empty());
    let after =
        drafting::selected_context(&db, "me@example.com", recipient, "reply", &[], &[]).unwrap();
    assert_eq!(after["style_candidates"], before["style_candidates"]);
    let list = archive::conversations(&db, "확인했습니다", true, "교수", false).unwrap();
    assert_eq!(list["items"][0]["recipient"], recipient);
    assert_eq!(list["items"][0]["reply_count"], 2);
    archive::delete_message(
        &db,
        &json!({"owner":"me@example.com","recipient":recipient,"key":first["key"]}),
    )
    .unwrap();
    let data = archive::conversation(&db, "me@example.com", recipient).unwrap();
    assert_eq!(array(&data, "timeline").len(), 1);
    assert_eq!(data["timeline"][0]["keys"][0], second["key"]);
}

#[test]
fn reply_validation_rejects_empty_bodies_and_unknown_conversations() {
    let db = populated();
    for payload in [
        json!({"owner":"me@example.com","recipient":"boss@example.com","body":" \n\t"}),
        json!({"owner":"me@example.com","recipient":"boss@example.com","body":42}),
        json!({"owner":"me@example.com","recipient":"boss@example.com"}),
        json!({"owner":"other@example.com","recipient":"boss@example.com","body":"내용"}),
        json!({"owner":"me@example.com","recipient":"missing","body":"내용"}),
        json!({"body":"내용"}),
    ] {
        assert!(archive::create_reply(&db, &payload).is_err());
    }
    assert!(
        db::rows(&db, "SELECT * FROM received_replies", [])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn replies_edit_without_reviving_deleted_or_overwriting_stale_data() {
    let db = populated();
    let payload =
        json!({"owner":"me@example.com","recipient":"boss@example.com","body":"처음 답장"});
    let saved = archive::create_reply(&db, &payload).unwrap();
    let data = archive::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    let reply = array(&data, "timeline")
        .iter()
        .find(|i| i["keys"][0] == saved["key"])
        .unwrap();
    assert!(reply.get("sent_at").is_none());
    let mut edit = json!({"owner":"me@example.com","recipient":"boss@example.com","key":saved["key"],"body":"수정 답장 boss@example.com","original_body":reply["body"]});
    for field in ["owner", "recipient"] {
        let mut wrong = edit.clone();
        wrong[field] = json!("someone@example.com");
        assert!(archive::update_reply(&db, &wrong).is_err());
    }
    archive::update_reply(&db, &edit).unwrap();
    assert!(
        archive::update_reply(&db, &edit).is_err(),
        "stale edit must fail"
    );
    let data = archive::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    let reply = array(&data, "timeline")
        .iter()
        .find(|i| i["keys"][0] == saved["key"])
        .unwrap();
    assert_eq!(reply["body"], "수정 답장 [이메일 삭제됨]");
    assert!(reply.get("sent_at").is_none());
    edit["original_body"] = reply["body"].clone();
    archive::delete_message(&db, &edit).unwrap();
    assert!(archive::update_reply(&db, &edit).is_err());
}

#[test]
fn manual_conversation_rename_preserves_identity_replies_and_label() {
    let db = populated();
    let created = archive::create_conversation(
        &db,
        &json!({"owner":"me@example.com","name":"이름 오타","label":"교수"}),
    )
    .unwrap();
    let recipient = &created["contact"]["recipient"];
    archive::create_reply(
        &db,
        &json!({"owner":"me@example.com","recipient":recipient,"body":"답장"}),
    )
    .unwrap();
    let mut edit = json!({"owner":"me@example.com","recipient":recipient,"original_name":"이름 오타","name":"고친 이름"});
    archive::rename_conversation(&db, &edit).unwrap();
    assert!(archive::rename_conversation(&db, &edit).is_err());
    let list = archive::conversations(&db, "고친 이름", false, "교수", false).unwrap();
    assert_eq!(list["items"][0]["recipient"], *recipient);
    assert_eq!(list["items"][0]["reply_count"], 1);
    let data = archive::conversation(&db, "me@example.com", recipient.as_str().unwrap()).unwrap();
    assert_eq!(data["timeline"][0]["sender_name"], "고친 이름");
    edit["original_name"] = json!("고친 이름");
    archive::create_conversation(&db, &json!({"owner":"me@example.com","name":"다른 사람"}))
        .unwrap();
    for name in [" ", "다른 사람"] {
        edit["name"] = json!(name);
        assert!(archive::rename_conversation(&db, &edit).is_err());
    }
    edit["name"] = json!("변경");
    edit["owner"] = json!("other@example.com");
    assert!(archive::rename_conversation(&db, &edit).is_err());
    edit["owner"] = json!("me@example.com");
    edit["recipient"] = json!("boss@example.com");
    assert!(archive::rename_conversation(&db, &edit).is_err());
}

#[test]
fn migration_removes_reply_timestamps_and_cached_copies_preserving_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let db = db::connect(&path).unwrap();
    load_tables(&db, &legacy()["tables"]);
    let saved = archive::create_reply(
        &db,
        &json!({"owner":"me@example.com","recipient":"boss@example.com","body":"보존할 답장"}),
    )
    .unwrap();
    let before = archive::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    db.execute_batch("ALTER TABLE received_replies ADD COLUMN sent_at TEXT NOT NULL DEFAULT '2026-11-03T09:30:00+09:00';").unwrap();
    mimicmail::mailbox::initialize(&db).unwrap();
    let work = mimicmail::work::Work::default();
    mimicmail::mailbox::read(&db, &work, |db| {
        mimicmail::mailbox::conversations(db, "", false, "", false, &work)
    })
    .unwrap();
    db.execute_batch("UPDATE browse_items SET item=json_set(item,'$.sent_at','2026-11-03T09:30:00+09:00') WHERE json_extract(item,'$.kind')='received';").unwrap();
    drop(db);
    let db = db::connect(&path).unwrap();
    assert!(
        !db::rows(&db, "PRAGMA table_info(received_replies)", [])
            .unwrap()
            .iter()
            .any(|r| r["name"] == "sent_at")
    );
    assert!(
        db::rows(&db, "SELECT * FROM browse_items", [])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        archive::conversation(&db, "me@example.com", "boss@example.com").unwrap(),
        before
    );
    mimicmail::mailbox::initialize(&db).unwrap();
    let selected = mimicmail::mailbox::read(&db, &work, |db| {
        mimicmail::mailbox::conversation_page(
            db,
            "me@example.com",
            "boss@example.com",
            "",
            mimicmail::mailbox::Page {
                limit: 1,
                message_key: Some(text(&saved, "key").to_owned()),
                ..Default::default()
            },
            &work,
        )
    })
    .unwrap();
    assert!(selected["timeline"][0].get("sent_at").is_none());
    assert_eq!(selected["timeline"][0]["body"], "보존할 답장");
}
