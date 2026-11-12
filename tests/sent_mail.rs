use mimicmail::drafting;
mod common;
use common::*;
use mimicmail::{archive, array, db, mailbox, privacy, text, work::Work};
use serde_json::json;

#[test]
fn entered_sent_mail_persists_as_searchable_history_and_style() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let db = db::connect(&path).unwrap();
    load_tables(&db, &legacy()["tables"]);
    mailbox::initialize(&db).unwrap();
    let work = Work::default();
    mailbox::read(&db, &work, |db| {
        mailbox::conversations(db, "", false, "", false, &work)
    })
    .unwrap();
    let saved = archive::create_sent_mail(&db, &json!({"owner":"me@example.com","recipient":"boss@example.com","body":"검토본을 전달드립니다.\nme@example.com"})).unwrap();
    let key = text(&saved, "key").to_owned();
    let id = saved["id"].as_i64().unwrap();
    drop(db);
    let db = db::connect(&path).unwrap();
    let data = archive::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    let item = array(&data, "timeline").last().unwrap();
    assert_eq!(item["keys"], json!([key]));
    assert_eq!(item["kind"], "sent");
    assert_eq!(item["mine"], true);
    assert_eq!(item["sender"], privacy::person_id("me@example.com"));
    assert_eq!(item["body"], "검토본을 전달드립니다.\n[이메일 삭제됨]");
    assert!(item.get("sent_at").is_none());
    let mail = archive::get_mail(&db, id).unwrap();
    assert_eq!(mail["display_original"], item["body"]);
    let selected = drafting::selected_context(
        &db,
        "me@example.com",
        "boss@example.com",
        "other",
        std::slice::from_ref(&key),
        &[text(&mail, "fingerprint").to_owned()],
    )
    .unwrap();
    assert_eq!(
        selected["current_facts"]["conversation"][0]["body"],
        item["body"]
    );
    assert_eq!(
        selected["style_references"][0]["authored_body"],
        item["body"]
    );
    let list = mailbox::read(&db, &work, |db| {
        mailbox::conversations(db, "검토본", false, "", false, &work)
    })
    .unwrap();
    assert_eq!(list["items"][0]["search_match"]["key"], key);
    assert_eq!(
        list,
        archive::conversations(&db, "검토본", false, "", false).unwrap()
    );
    let page = mailbox::read(&db, &work, |db| {
        mailbox::conversation_page(
            db,
            "me@example.com",
            "boss@example.com",
            "",
            mailbox::Page {
                limit: 1,
                message_key: Some(key.clone()),
                ..Default::default()
            },
            &work,
        )
    })
    .unwrap();
    assert_eq!(page["timeline"][0], *item);
    assert!(
        !archive::conversation(&db, "me@example.com", "peer@example.com")
            .unwrap()
            .to_string()
            .contains("검토본")
    );
    assert!(archive::delete_message(&db, &json!({"owner":"other@example.com","recipient":"boss@example.com","key":key,"revision":0})).is_err());
    archive::delete_message(
        &db,
        &json!({"owner":"me@example.com","recipient":"boss@example.com","key":key,"revision":0}),
    )
    .unwrap();
    let list = mailbox::read(&db, &work, |db| {
        mailbox::conversations(db, "검토본", false, "", false, &work)
    })
    .unwrap();
    assert!(array(&list, "items").is_empty());
    assert!(
        drafting::selected_context(
            &db,
            "me@example.com",
            "boss@example.com",
            "other",
            &[key],
            &[]
        )
        .is_err()
    );
    assert!(
        drafting::selected_context(
            &db,
            "me@example.com",
            "boss@example.com",
            "other",
            &[],
            &[text(&mail, "fingerprint").to_owned()]
        )
        .is_err()
    );
}

#[test]
fn entered_mail_and_replies_keep_save_order_after_edit_and_delete() {
    let db = populated();
    let created = archive::create_conversation(
        &db,
        &json!({"owner":"me@example.com","name":"새 교수","label":"교수"}),
    )
    .unwrap();
    let recipient = text(&created["contact"], "recipient");
    let payload = json!({"owner":"me@example.com","recipient":recipient,"body":"확인했습니다."});
    let first = archive::create_sent_mail(&db, &payload).unwrap();
    let reply = archive::create_reply(&db, &payload).unwrap();
    let second = archive::create_sent_mail(&db, &payload).unwrap();
    let third = archive::create_sent_mail(&db, &payload).unwrap();
    let last = archive::create_reply(&db, &payload).unwrap();
    let keys = |data: &serde_json::Value| {
        array(data, "timeline")
            .iter()
            .map(|i| i["keys"][0].clone())
            .collect::<Vec<_>>()
    };
    let data = archive::conversation(&db, "me@example.com", recipient).unwrap();
    assert_eq!(
        keys(&data),
        vec![
            first["key"].clone(),
            reply["key"].clone(),
            second["key"].clone(),
            third["key"].clone(),
            last["key"].clone()
        ]
    );
    assert!(
        array(&data, "timeline")
            .iter()
            .all(|i| i.get("sent_at").is_none())
    );
    let id = second["id"].as_i64().unwrap();
    let mail = archive::get_mail(&db, id).unwrap();
    assert_eq!(mail["recipients"][0]["role"], "교수");
    let edit = json!({"authored_body":"고친 내 메일","revision":0,"recipients":mail["recipients"]});
    archive::update_mail(&db, id, &edit).unwrap();
    assert!(archive::update_mail(&db, id, &edit).is_err());
    archive::delete_message(
        &db,
        &json!({"owner":"me@example.com","recipient":recipient,"key":reply["key"]}),
    )
    .unwrap();
    let data = archive::conversation(&db, "me@example.com", recipient).unwrap();
    assert_eq!(
        keys(&data),
        vec![
            first["key"].clone(),
            second["key"].clone(),
            third["key"].clone(),
            last["key"].clone()
        ]
    );
    assert_eq!(data["timeline"][1]["body"], "고친 내 메일");
    let fourth = archive::create_sent_mail(&db, &payload).unwrap();
    let data = archive::conversation(&db, "me@example.com", recipient).unwrap();
    assert_eq!(
        array(&data, "timeline").last().unwrap()["keys"][0],
        fourth["key"]
    );
    mailbox::initialize(&db).unwrap();
    let work = Work::default();
    assert_eq!(
        mailbox::read(&db, &work, |db| mailbox::conversations(
            db, "", false, "교수", false, &work
        ))
        .unwrap(),
        archive::conversations(&db, "", false, "교수", false).unwrap()
    );
}

#[test]
fn entered_sent_mail_rejects_empty_input_and_unknown_contacts() {
    let db = populated();
    let before = db::rows(&db, "SELECT * FROM sent_mail", []).unwrap();
    for payload in [
        json!({"owner":"me@example.com","recipient":"boss@example.com","body":" \n\t"}),
        json!({"owner":"me@example.com","recipient":"boss@example.com","body":42}),
        json!({"owner":"me@example.com","recipient":"boss@example.com"}),
        json!({"owner":"other@example.com","recipient":"boss@example.com","body":"내용"}),
        json!({"owner":"me@example.com","recipient":"missing","body":"내용"}),
        json!({"body":"내용"}),
    ] {
        assert!(archive::create_sent_mail(&db, &payload).is_err());
    }
    assert_eq!(
        db::rows(&db, "SELECT * FROM sent_mail", []).unwrap(),
        before
    );
}
