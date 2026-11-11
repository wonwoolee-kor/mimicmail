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
fn imported_chat_deletion_removes_originals_and_preserves_other_chats() {
    let db = populated();
    mailbox::initialize(&db).unwrap();
    let before = list(&db, "");
    let peer = gui::conversation(&db, "me@example.com", "peer@example.com").unwrap();
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
        db::rows(&db, "SELECT id FROM mail_archive", []).unwrap(),
        vec![json!({"id":2})]
    );
    assert!(gui::get_mail(&db, 1).is_err());
    assert_eq!(
        db::rows(&db, "SELECT * FROM sent_mail", []).unwrap().len(),
        1
    );
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
    assert_eq!(
        gui::conversation(&db, &owner, &peer).unwrap()["timeline"],
        before["timeline"]
    );
    assert!(gui::get_mail(&db, 1).is_ok());
    assert!(
        !db::exists(
            &db,
            "SELECT 1 FROM sent_mail WHERE owner=? AND recipient=?",
            params![owner, boss]
        )
        .unwrap()
    );
    let candidates = pipeline::style_candidates(&db, &owner, "new-person", "교수", false).unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().all(|c| c["recipient"] == peer));
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
    gui::delete_conversation(&db, &json!({"owner":owner,"recipient":peer})).unwrap();
    assert!(
        db::rows(&db, "SELECT * FROM mail_archive", [])
            .unwrap()
            .is_empty()
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
    let sent = gui::create_sent_mail(&db, &payload).unwrap();
    let reply = gui::create_reply(&db, &payload).unwrap();
    assert_eq!(array(&list(&db, ""), "items").len(), 1);
    gui::delete_conversation(&db, &payload).unwrap();
    for table in [
        "sent_mail",
        "mail_archive",
        "received_replies",
        "manual_conversations",
        "contact_labels",
        "browse_items",
        "browse_contacts",
    ] {
        assert!(
            db::rows(&db, &format!("SELECT * FROM {table}"), [])
                .unwrap()
                .is_empty(),
            "{table}"
        );
    }
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
    let fresh_payload =
        json!({"owner":"local","recipient":fresh["contact"]["recipient"],"body":"새 내용"});
    let fresh_sent = gui::create_sent_mail(&db, &fresh_payload).unwrap();
    let fresh_reply = gui::create_reply(&db, &fresh_payload).unwrap();
    assert!(fresh_sent["id"].as_i64().unwrap() > sent["id"].as_i64().unwrap());
    assert_ne!(fresh_reply["key"], reply["key"]);
    gui::delete_conversation(&db, &fresh["contact"]).unwrap();
    drop(db);
    let db = db::connect(&path).unwrap();
    mailbox::initialize(&db).unwrap();
    assert!(array(&list(&db, ""), "items").is_empty());
    assert!(array(&gui::conversations(&db, "", false, "").unwrap(), "items").is_empty());
    drop(db);
    assert!(
        !std::fs::read(&path)
            .unwrap()
            .windows("저장한 내용".len())
            .any(|bytes| bytes == "저장한 내용".as_bytes())
    );
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

#[test]
fn startup_purges_legacy_deleted_chats_and_preserves_ids_and_index_triggers() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("old.sqlite3");
    let old = rusqlite::Connection::open(&path).unwrap();
    old.execute_batch(&include_str!("../assets/schema.sql").replace(" AUTOINCREMENT", ""))
        .unwrap();
    old.execute(
        "ALTER TABLE mail_archive ADD COLUMN context_json TEXT NOT NULL DEFAULT '{}'",
        [],
    )
    .unwrap();
    load_legacy_tables(&old, &legacy()["tables"]);
    privacy::migrate(&old).unwrap();
    mailbox::initialize(&old).unwrap();
    let reply = gui::create_reply(
        &old,
        &json!({"owner":"me@example.com","recipient":"boss@example.com","body":"以前の削除対象"}),
    )
    .unwrap();
    assert_eq!(array(&list(&old, ""), "items").len(), 2);
    let owner = privacy::person_id("me@example.com");
    let recipient = privacy::person_id("boss@example.com");
    old.execute(
        "INSERT INTO deleted_messages VALUES (?,?)",
        params![owner, format!("conversation:{recipient}")],
    )
    .unwrap();
    drop(old);
    let db = db::connect(&path).unwrap();
    assert!(gui::get_mail(&db, 1).is_err());
    assert!(gui::get_mail(&db, 2).is_ok());
    assert!(
        db::rows(&db, "SELECT * FROM received_replies", [])
            .unwrap()
            .is_empty()
    );
    for table in ["browse_items", "browse_contacts"] {
        assert!(
            !db::exists(
                &db,
                &format!("SELECT 1 FROM {table} WHERE owner=? AND recipient=?"),
                params![owner, recipient]
            )
            .unwrap()
        );
    }
    assert_eq!(array(&list(&db, ""), "items").len(), 1);
    let sent = gui::create_sent_mail(
        &db,
        &json!({"owner":"me@example.com","recipient":"peer@example.com","body":"새 본문"}),
    )
    .unwrap();
    assert!(sent["id"].as_i64().unwrap() > 2);
    let fresh = gui::create_reply(
        &db,
        &json!({"owner":"me@example.com","recipient":"peer@example.com","body":"새 답장"}),
    )
    .unwrap();
    assert_ne!(fresh["key"], reply["key"]);
    assert_eq!(array(&list(&db, "새 답장"), "items").len(), 1);
    db::initialize(&db).unwrap();
    assert_eq!(array(&list(&db, "새 답장"), "items").len(), 1);
}

#[test]
fn reingestion_does_not_restore_deleted_content_or_shared_recipient_links() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("sent.eml");
    std::fs::write(
        &path,
        raw(
            "me@example.com",
            "boss@example.com,peer@example.com",
            "shared-original-unique-text",
            "",
        ),
    )
    .unwrap();
    let mut db = memory();
    let mut llm = Fake::new(vec![unknown(), unknown()]);
    pipeline::ingest(
        &mut db,
        &mut llm,
        vec![Ok(path.clone())],
        "me@example.com",
        &[],
        None,
    )
    .unwrap();
    gui::delete_conversation(
        &db,
        &json!({"owner":"me@example.com","recipient":"boss@example.com"}),
    )
    .unwrap();
    let mut unused = Fake::new(vec![]);
    let counts = pipeline::ingest(
        &mut db,
        &mut unused,
        vec![Ok(path.clone())],
        "me@example.com",
        &[],
        None,
    )
    .unwrap();
    assert_eq!(counts, json!({"indexed":0,"skipped":1}));
    assert_eq!(
        db::rows(&db, "SELECT recipient FROM sent_mail", []).unwrap(),
        vec![json!({"recipient":privacy::person_id("peer@example.com")})]
    );
    gui::delete_conversation(
        &db,
        &json!({"owner":"me@example.com","recipient":"peer@example.com"}),
    )
    .unwrap();
    let counts = pipeline::ingest(
        &mut db,
        &mut unused,
        vec![Ok(path.clone())],
        "me@example.com",
        &[],
        None,
    )
    .unwrap();
    assert_eq!(counts, json!({"indexed":0,"skipped":1}));
    assert!(
        db::rows(&db, "SELECT * FROM sent_mail", [])
            .unwrap()
            .is_empty()
    );
    assert!(
        db::rows(&db, "SELECT * FROM mail_archive", [])
            .unwrap()
            .is_empty()
    );
    assert!(unused.calls.is_empty());
    assert!(path.exists());
}

#[test]
fn failed_purge_rolls_back_content_metadata_cache_and_deletion_marker() {
    let db = populated();
    mailbox::initialize(&db).unwrap();
    let before = list(&db, "");
    let tables = [
        "sent_mail",
        "mail_archive",
        "received_replies",
        "contact_labels",
        "manual_conversations",
        "deleted_messages",
        "browse_items",
        "browse_contacts",
        "browse_dirty",
    ];
    let snapshots: Vec<_> = tables
        .iter()
        .map(|table| db::rows(&db, &format!("SELECT * FROM {table}"), []).unwrap())
        .collect();
    db.execute_batch("CREATE TRIGGER fail_purge BEFORE DELETE ON mail_archive BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    assert!(
        gui::delete_conversation(
            &db,
            &json!({"owner":"me@example.com","recipient":"boss@example.com"})
        )
        .is_err()
    );
    for (table, snapshot) in tables.iter().zip(snapshots) {
        assert_eq!(
            db::rows(&db, &format!("SELECT * FROM {table}"), []).unwrap(),
            snapshot,
            "{table}"
        );
    }
    assert_eq!(list(&db, ""), before);
}
