mod common;
use common::*;
use mimicmail::{array, context, db, gui, pipeline, privacy, text};
use serde_json::json;
use std::collections::HashSet;

fn assert_no_addresses(db: &rusqlite::Connection) {
    for table in [
        "sent_mail",
        "mail_archive",
        "contact_labels",
        "label_colors",
        "deleted_messages",
        "manual_conversations",
    ] {
        for row in db::rows(db, &format!("SELECT * FROM {table}"), []).unwrap() {
            assert!(!row.to_string().contains('@'), "{table}: {row}");
        }
    }
}

#[test]
fn migration_scrubs_disk_and_preserves_contacts_labels_and_deleted_quotes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("legacy.sqlite3");
    let db = db::open(&path, false).unwrap();
    db.execute_batch(include_str!("../assets/schema.sql"))
        .unwrap();
    db.execute(
        "ALTER TABLE mail_archive ADD COLUMN context_json TEXT NOT NULL DEFAULT '{}'",
        [],
    )
    .unwrap();
    load_legacy_tables(&db, &legacy()["tables"]);
    let messages = [gui::get_mail(&db, 1).unwrap()];
    let before = context::timeline(&messages, "me@example.com", &HashSet::new());
    let quoted = before.iter().find(|m| m["kind"] == "quote").unwrap();
    db.execute(
        "INSERT INTO deleted_messages VALUES (?,?)",
        ["me@example.com", quoted["keys"][0].as_str().unwrap()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO contact_labels VALUES ('me@example.com','boss@example.com','교수',3)",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO manual_conversations VALUES ('me@example.com','new@example.com','새 수신자','2026-01-02')", []).unwrap();
    drop(db);
    assert!(
        std::fs::read(&path)
            .unwrap()
            .windows(14)
            .any(|s| s == b"me@example.com")
    );

    let db = db::connect(&path).unwrap();
    assert_no_addresses(&db);
    let list = gui::conversations(&db, "", false, "").unwrap();
    assert_eq!(list["total_mails"], 2);
    assert_eq!(list["total_conversations"], 3);
    let boss = array(&list, "items")
        .iter()
        .find(|m| m["name"] == "팀장")
        .unwrap();
    assert_eq!(boss["label"], "교수");
    assert_eq!(boss["label_revision"], 3);
    let data = gui::conversation(&db, text(boss, "owner"), text(boss, "recipient")).unwrap();
    assert!(
        !array(&data, "timeline")
            .iter()
            .any(|m| m["keys"] == quoted["keys"])
    );
    db::initialize(&db).unwrap();
    assert_eq!(gui::conversations(&db, "", false, "").unwrap(), list);
    drop(db);
    let bytes = std::fs::read(&path).unwrap();
    for address in [
        "me@example.com",
        "boss@example.com",
        "peer@example.com",
        "new@example.com",
    ] {
        assert!(
            !bytes
                .windows(address.len())
                .any(|s| s == address.as_bytes())
        );
    }
}

#[test]
fn ingest_reingest_and_edits_do_not_restore_addresses_or_deleted_quotes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("me@example.com.eml");
    let body = "연락처 me@example.com\n\n-----Original Message-----\nFrom: 팀장 <boss@example.com>\nSent: 2026-01-01 09:00:00 +0900\nTo: me@example.com\nSubject: 확인\n연락처 boss@example.com";
    std::fs::write(
        &path,
        raw(
            "me@example.com",
            "팀장 <boss@example.com>",
            body,
            "Thu, 01 Jan 2026 10:00:00 +0900",
        ),
    )
    .unwrap();
    let mut db = memory();
    let mut llm = Fake::new(vec![unknown()]);
    pipeline::ingest(
        &mut db,
        &mut llm,
        [Ok(path.clone())],
        "me@example.com",
        &[],
        None,
    )
    .unwrap();
    assert_no_addresses(&db);
    let data = gui::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    let quoted = array(&data, "timeline")
        .iter()
        .find(|m| m["kind"] == "quote")
        .unwrap();
    gui::delete_message(
        &db,
        &json!({"owner":"me@example.com","recipient":"boss@example.com","key":quoted["keys"][0]}),
    )
    .unwrap();
    let mail = gui::get_mail(&db, 1).unwrap();
    gui::update_mail(&db, 1, &json!({"revision":0,"authored_body":"new@example.com으로 연락","recipients":mail["recipients"]})).unwrap();
    gui::update_contact_label(&db, &json!({"owner":"me@example.com","recipient":"boss@example.com","label":"교수 new@example.com","revision":0})).unwrap();
    let result =
        pipeline::ingest(&mut db, &mut llm, [Ok(path)], "me@example.com", &[], None).unwrap();
    assert_eq!(result["skipped"], 1);
    assert_eq!(llm.calls.len(), 1);
    assert_no_addresses(&db);
    let data = gui::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    assert_eq!(array(&data, "timeline").len(), 1);
    assert_eq!(
        data["messages"][0]["authored_body"],
        "[이메일 삭제됨]으로 연락"
    );
}

#[test]
fn new_conversation_needs_only_name_and_retains_its_id_after_reopening() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let db = db::connect(&path).unwrap();
    for name in [
        json!(""),
        json!("   "),
        json!(null),
        json!(42),
        json!("가".repeat(101)),
    ] {
        assert!(gui::create_conversation(&db, &json!({"name":name})).is_err());
    }
    assert_eq!(
        gui::conversations(&db, "", false, "").unwrap()["total_conversations"],
        0
    );
    let first = gui::create_conversation(&db, &json!({"name":"새 사람","label":"교수"})).unwrap();
    assert_eq!(first["contact"]["owner"], "local");
    assert!(text(&first["contact"], "recipient").starts_with("person:"));
    assert_no_addresses(&db);
    drop(db);
    let db = db::connect(&path).unwrap();
    let next = gui::create_conversation(&db, &json!({"name":"새 사람"})).unwrap();
    assert_eq!(next["created"], false);
    assert_eq!(first["contact"], next["contact"]);
    assert_eq!(
        gui::draft_context(&db, "local", text(&first["contact"], "recipient"), "other").unwrap()["current_facts"]
            ["recipient_name"],
        "새 사람"
    );
    assert_eq!(
        privacy::person_id(" ME@example.com "),
        privacy::person_id("me@example.com")
    );
}
