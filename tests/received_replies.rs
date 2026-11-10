mod common;
use common::*;
use mimicmail::{array, db, gui, privacy, text};
use serde_json::json;

#[test]
fn replies_persist_as_searchable_facts_without_changing_sent_style() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let db = db::connect(&path).unwrap();
    load_tables(&db, &legacy()["tables"]);
    let before = gui::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    let styles = gui::draft_context(&db, "me@example.com", "boss@example.com", "reply").unwrap();
    let saved = gui::create_reply(&db, &json!({"owner":"me@example.com","recipient":"boss@example.com","body":"검토완료\n문의: boss@example.com"})).unwrap();
    let key = text(&saved, "key").to_owned();
    drop(db);
    let db = db::connect(&path).unwrap();
    let data = gui::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    assert_eq!(data["messages"], before["messages"]);
    let reply = array(&data, "timeline").last().unwrap();
    assert_eq!(reply["keys"], json!([key]));
    assert_eq!(reply["kind"], "received");
    assert_eq!(reply["mine"], false);
    assert_eq!(reply["sender"], privacy::person_id("boss@example.com"));
    assert_eq!(reply["sender_name"], "팀장");
    assert_eq!(reply["body"], "검토완료\n문의: [이메일 삭제됨]");
    assert!(chrono::DateTime::parse_from_rfc3339(text(reply, "sent_at")).is_ok());
    assert!(array(reply, "source_ids").is_empty());
    let list = gui::conversations(&db, "검토완료", true, "").unwrap();
    assert_eq!(array(&list, "items").len(), 1);
    assert_eq!(list["items"][0]["search_match"]["key"], key);
    assert_eq!(
        gui::search_matches(array(&data, "timeline"), "검토완료").len(),
        1
    );
    let catalog = gui::draft_context(&db, "me@example.com", "boss@example.com", "reply").unwrap();
    assert_eq!(catalog["style_candidates"], styles["style_candidates"]);
    assert!(!catalog.to_string().contains("검토완료"));
    let selected = gui::selected_context(
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
        let data = gui::conversation(&db, owner, recipient).unwrap();
        assert!(!data.to_string().contains("검토완료"));
        assert!(
            gui::delete_message(&db, &json!({"owner":owner,"recipient":recipient,"key":key}))
                .is_err()
        );
    }
    gui::delete_message(
        &db,
        &json!({"owner":"me@example.com","recipient":"boss@example.com","key":key}),
    )
    .unwrap();
    drop(db);
    let db = db::connect(&path).unwrap();
    assert_eq!(
        gui::conversation(&db, "me@example.com", "boss@example.com").unwrap(),
        before
    );
    assert!(
        array(
            &gui::conversations(&db, "검토완료", false, "").unwrap(),
            "items"
        )
        .is_empty()
    );
    assert!(
        gui::selected_context(
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
    gui::update_contact_label(&db, &json!({"owner":"me@example.com","recipient":"boss@example.com","label":"교수","revision":0})).unwrap();
    let created = gui::create_conversation(
        &db,
        &json!({"owner":"me@example.com","name":"새 교수","label":"교수"}),
    )
    .unwrap();
    let recipient = text(&created["contact"], "recipient");
    let before = gui::draft_context(&db, "me@example.com", recipient, "reply").unwrap();
    assert!(!array(&before, "style_candidates").is_empty());
    let payload = json!({"owner":"me@example.com","recipient":recipient,"body":"확인했습니다."});
    let first = gui::create_reply(&db, &payload).unwrap();
    let second = gui::create_reply(&db, &payload).unwrap();
    assert_ne!(first["key"], second["key"]);
    let data = gui::conversation(&db, "me@example.com", recipient).unwrap();
    assert_eq!(data["timeline"][0]["sender_name"], "새 교수");
    assert_eq!(array(&data, "timeline").len(), 2);
    assert!(array(&data, "messages").is_empty());
    let after = gui::draft_context(&db, "me@example.com", recipient, "reply").unwrap();
    assert_eq!(after["style_candidates"], before["style_candidates"]);
    let list = gui::conversations(&db, "확인했습니다", true, "교수").unwrap();
    assert_eq!(list["items"][0]["recipient"], recipient);
    assert_eq!(list["items"][0]["reply_count"], 2);
    gui::delete_message(
        &db,
        &json!({"owner":"me@example.com","recipient":recipient,"key":first["key"]}),
    )
    .unwrap();
    let data = gui::conversation(&db, "me@example.com", recipient).unwrap();
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
        assert!(gui::create_reply(&db, &payload).is_err());
    }
    assert!(
        db::rows(&db, "SELECT * FROM received_replies", [])
            .unwrap()
            .is_empty()
    );
}
