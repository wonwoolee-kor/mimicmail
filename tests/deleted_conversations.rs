mod common;
use common::*;
use mimicmail::{array, db, gui, mailbox, pipeline, privacy, text, work::Work};
use rusqlite::params;
use serde_json::{Value, json};

fn list(db: &rusqlite::Connection, query: &str) -> Value {
    let work = Work::default();
    mailbox::read(db, &work, |db| {
        mailbox::conversations(db, query, false, "", false, &work)
    })
    .unwrap()
}

#[test]
fn imported_chat_deletion_invalidates_search_and_preserves_other_chats_and_originals() {
    let db = populated();
    mailbox::initialize(&db).unwrap();
    let before = list(&db, "");
    let peer = gui::conversation(&db, "me@example.com", "peer@example.com").unwrap();
    let originals = db::rows(&db, "SELECT * FROM mail_archive", []).unwrap();
    let sent = db::rows(&db, "SELECT * FROM sent_mail", []).unwrap();
    let payload = json!({"owner":"me@example.com","recipient":"boss@example.com"});
    gui::delete_conversation(&db, &payload).unwrap();
    gui::delete_conversation(&db, &payload).unwrap();
    let after = list(&db, "");
    assert_eq!(
        array(&after, "items").len() + 1,
        array(&before, "items").len()
    );
    assert_eq!(after, gui::conversations(&db, "", false, "").unwrap());
    assert!(array(&list(&db, "팀장"), "items").is_empty());
    assert_eq!(
        gui::conversation(&db, "me@example.com", "boss@example.com").unwrap(),
        json!({"messages":[],"timeline":[]})
    );
    assert_eq!(
        gui::conversation(&db, "me@example.com", "peer@example.com").unwrap(),
        peer
    );
    assert_eq!(
        db::rows(&db, "SELECT * FROM mail_archive", []).unwrap(),
        originals
    );
    assert_eq!(db::rows(&db, "SELECT * FROM sent_mail", []).unwrap(), sent);
    assert!(gui::draft_context(&db, "me@example.com", "boss@example.com", "other").is_err());
    assert!(
        pipeline::style_candidates(&db, "me@example.com", "boss@example.com", "unknown", true)
            .unwrap()
            .is_empty()
    );
    let work = Work::default();
    let page = mailbox::read(&db, &work, |db| {
        mailbox::conversation_page(
            db,
            "me@example.com",
            "boss@example.com",
            "",
            mailbox::Page::default(),
            &work,
        )
    })
    .unwrap();
    assert_eq!(page["timeline"], json!([]));
}

#[test]
fn deletion_is_scoped_to_one_recipient_even_for_shared_mail_and_style() {
    let db = populated();
    let owner = privacy::person_id("me@example.com");
    let boss = privacy::person_id("boss@example.com");
    let peer = privacy::person_id("peer@example.com");
    db.execute("INSERT INTO sent_mail SELECT owner,fingerprint,message_id,sender,?,role,relationship,purpose,authored_body FROM sent_mail WHERE owner=? AND recipient=?",params![peer,owner,boss]).unwrap();
    for recipient in [&boss, &peer] {
        gui::update_contact_label(
            &db,
            &json!({"owner":owner,"recipient":recipient,"label":"교수","revision":0}),
        )
        .unwrap();
    }
    let before = gui::conversation(&db, &owner, &peer).unwrap();
    gui::delete_conversation(&db, &json!({"owner":owner,"recipient":boss})).unwrap();
    assert_eq!(gui::conversation(&db, &owner, &peer).unwrap(), before);
    let candidates = pipeline::style_candidates(&db, &owner, "new-person", "교수", false).unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().all(|c| c["recipient"] == peer));
    // Reindexing source records must not bring the deleted contact back.
    db.execute(
        "UPDATE sent_mail SET purpose='thanks' WHERE owner=? AND recipient=?",
        params![owner, boss],
    )
    .unwrap();
    mailbox::initialize(&db).unwrap();
    assert!(
        array(&list(&db, ""), "items")
            .iter()
            .all(|c| c["recipient"] != boss)
    );
}

#[test]
fn manual_chat_deletion_persists_blocks_stale_writes_and_allows_a_fresh_same_name() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let db = db::connect(&path).unwrap();
    mailbox::initialize(&db).unwrap();
    let created = gui::create_conversation(
        &db,
        &json!({"owner":"local","name":"테스트","label":"교수"}),
    )
    .unwrap();
    let recipient = text(&created["contact"], "recipient");
    let payload = json!({"owner":"local","recipient":recipient,"body":"저장한 내용"});
    gui::create_sent_mail(&db, &payload).unwrap();
    let reply = gui::create_reply(&db, &payload).unwrap();
    assert_eq!(array(&list(&db, ""), "items").len(), 1);
    gui::delete_conversation(&db, &payload).unwrap();
    assert!(array(&list(&db, ""), "items").is_empty());
    assert!(gui::create_sent_mail(&db, &payload).is_err());
    assert!(gui::create_reply(&db, &payload).is_err());
    assert!(gui::update_reply(&db,&json!({"owner":"local","recipient":recipient,"key":reply["key"],"body":"수정","original_body":"저장한 내용"})).is_err());
    assert!(
        gui::rename_conversation(
            &db,
            &json!({"owner":"local","recipient":recipient,"name":"수정","original_name":"테스트"})
        )
        .is_err()
    );
    assert!(
        gui::update_contact_label(
            &db,
            &json!({"owner":"local","recipient":recipient,"label":"수정","revision":1})
        )
        .is_err()
    );
    let fresh = gui::create_conversation(&db, &json!({"owner":"local","name":"테스트"})).unwrap();
    assert_eq!(fresh["created"], true);
    assert_ne!(fresh["contact"]["recipient"], recipient);
    gui::delete_conversation(&db, &fresh["contact"]).unwrap();
    drop(db);
    let db = db::connect(&path).unwrap();
    mailbox::initialize(&db).unwrap();
    assert!(array(&list(&db, ""), "items").is_empty());
    assert!(array(&gui::conversations(&db, "", false, "").unwrap(), "items").is_empty());
}

#[test]
fn invalid_deletion_does_not_hide_any_chat() {
    let db = populated();
    let before = gui::conversations(&db, "", false, "").unwrap();
    for payload in [
        json!({}),
        json!({"owner":"","recipient":"boss@example.com"}),
        json!({"owner":"other","recipient":"boss@example.com"}),
        json!({"owner":"me@example.com","recipient":"unknown"}),
    ] {
        assert!(gui::delete_conversation(&db, &payload).is_err());
    }
    assert_eq!(gui::conversations(&db, "", false, "").unwrap(), before);
    assert!(
        db::rows(&db, "SELECT * FROM deleted_messages", [])
            .unwrap()
            .is_empty()
    );
}
