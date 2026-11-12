mod common;
use common::*;
use mimicmail::{
    archive, array, db,
    mailbox::{self, Page},
    privacy, text,
    work::Work,
};
use serde_json::{Value, json};

fn list(db: &rusqlite::Connection, q: &str, replies: bool, label: &str, unlabeled: bool) -> Value {
    let work = Work::default();
    mailbox::read(db, &work, |db| {
        mailbox::conversations(db, q, replies, label, unlabeled, &work)
    })
    .unwrap()
}
fn page(db: &rusqlite::Connection, recipient: &str, q: &str, p: Page) -> Value {
    let work = Work::default();
    mailbox::read(db, &work, |db| {
        mailbox::conversation_page(db, "me@example.com", recipient, q, p, &work)
    })
    .unwrap()
}
fn equivalent(db: &rusqlite::Connection) {
    for q in [
        "",
        "검토",
        "팀장",
        "자료",
        "없는검색어",
        "답변",
        "본문",
        "StraSSe",
        "😀",
    ] {
        for (replies, label, unlabeled) in [
            (false, "", false),
            (true, "", false),
            (false, "교수", false),
            (false, "", true),
        ] {
            assert_eq!(
                list(db, q, replies, label, unlabeled),
                archive::conversations(db, q, replies, label, unlabeled).unwrap(),
                "query={q} replies={replies} label={label} unlabeled={unlabeled}"
            );
        }
    }
    let contacts = list(db, "", false, "", false);
    for c in array(&contacts, "items") {
        let recipient = text(c, "recipient");
        let full = archive::conversation(db, "me@example.com", recipient).unwrap();
        let mut before = None;
        let mut joined = Vec::new();
        loop {
            let p = page(
                db,
                recipient,
                "",
                Page {
                    before,
                    limit: 1,
                    ..Page::default()
                },
            );
            let mut items = array(&p, "timeline").to_vec();
            items.extend(joined);
            joined = items;
            let start = p["page"]["start"].as_u64().unwrap() as usize;
            if start == 0 {
                break;
            }
            before = Some(start);
        }
        assert_eq!(joined, array(&full, "timeline"));
    }
}

#[test]
fn projection_matches_legacy_after_edits_labels_deletes_and_replies() {
    let db = populated();
    mailbox::initialize(&db).unwrap();
    equivalent(&db);
    let mail = archive::get_mail(&db, 1).unwrap();
    archive::update_mail(&db,1,&json!({"revision":0,"authored_body":"😀 Straße STRASSE 검토\n본문","recipients":mail["recipients"]})).unwrap();
    equivalent(&db);
    archive::update_contact_label(&db,&json!({"owner":"me@example.com","recipient":"boss@example.com","label":"교수","revision":0,"color":"#3366cc","color_revision":0})).unwrap();
    let received = archive::create_reply(
        &db,
        &json!({"owner":"me@example.com","recipient":"boss@example.com","body":"자료 검토 완료"}),
    )
    .unwrap();
    let manual = archive::create_conversation(
        &db,
        &json!({"owner":"me@example.com","name":"빈 대화","label":"교수"}),
    )
    .unwrap();
    archive::create_reply(&db,&json!({"owner":"me@example.com","recipient":manual["contact"]["recipient"],"body":"본문 답장"})).unwrap();
    equivalent(&db);
    archive::delete_message(
        &db,
        &json!({"owner":"me@example.com","recipient":"boss@example.com","key":received["key"]}),
    )
    .unwrap();
    let full = archive::conversation(&db, "me@example.com", "boss@example.com").unwrap();
    let sent = array(&full, "timeline")
        .iter()
        .find(|i| i["kind"] == "sent")
        .unwrap();
    let revision = array(&full, "messages")
        .iter()
        .find(|m| m["id"] == sent["source_ids"][0])
        .unwrap()["revision"]
        .clone();
    archive::delete_message(&db,&json!({"owner":"me@example.com","recipient":"boss@example.com","key":sent["keys"][0],"revision":revision})).unwrap();
    equivalent(&db);
}

#[test]
fn search_navigation_spans_pages_without_losing_unicode_offsets() {
    let db = populated();
    mailbox::initialize(&db).unwrap();
    let mail = archive::get_mail(&db, 1).unwrap();
    archive::update_mail(&db,1,&json!({"revision":0,"authored_body":"😀 Straße STRASSE 끝","recipients":mail["recipients"]})).unwrap();
    archive::create_reply(
        &db,
        &json!({"owner":"me@example.com","recipient":"boss@example.com","body":"strasse"}),
    )
    .unwrap();
    for index in 0..3 {
        let p = page(
            &db,
            "boss@example.com",
            "STRASSE",
            Page {
                limit: 1,
                match_index: Some(index),
                ..Page::default()
            },
        );
        assert_eq!(p["match_total"], 3);
        assert_eq!(p["match_index"], index);
        let m = array(&p, "search_matches")
            .iter()
            .find(|m| m["index"] == index)
            .unwrap();
        let item = array(&p, "timeline")
            .iter()
            .find(|i| i["keys"][0] == m["key"])
            .unwrap();
        let units: Vec<_> = text(item, text(m, "field")).encode_utf16().collect();
        assert_eq!(
            String::from_utf16(
                &units[m["start"].as_u64().unwrap() as usize..m["end"].as_u64().unwrap() as usize]
            )
            .unwrap(),
            text(m, "term")
        );
    }
}

#[test]
fn saved_reply_can_be_revealed_on_its_page_after_insert_and_edit() {
    let db = populated();
    mailbox::initialize(&db).unwrap();
    let contact =
        archive::create_conversation(&db, &json!({"owner":"me@example.com","name":"날짜 검토"}))
            .unwrap();
    let recipient = text(&contact["contact"], "recipient");
    let empty = page(
        &db,
        recipient,
        "",
        Page {
            limit: 1,
            ..Page::default()
        },
    );
    assert_eq!(empty["page"]["end"], 0);
    let first = archive::create_reply(
        &db,
        &json!({"owner":"me@example.com","recipient":recipient,"body":"첫 답장"}),
    )
    .unwrap();
    let old = archive::create_reply(
        &db,
        &json!({"owner":"me@example.com","recipient":recipient,"body":"과거 답장"}),
    )
    .unwrap();
    for saved in [&first, &old] {
        let result = page(
            &db,
            recipient,
            "첫",
            Page {
                limit: 1,
                before: Some(0),
                message_key: Some(text(saved, "key").to_owned()),
                ..Page::default()
            },
        );
        assert_eq!(result["timeline"][0]["keys"][0], saved["key"]);
    }
    let before = page(
        &db,
        recipient,
        "",
        Page {
            limit: 1,
            message_key: Some(text(&old, "key").to_owned()),
            ..Page::default()
        },
    );
    archive::update_reply(&db, &json!({"owner":"me@example.com","recipient":recipient,"key":old["key"],"body":"수정 답장","original_body":before["timeline"][0]["body"]})).unwrap();
    let after = page(
        &db,
        recipient,
        "",
        Page {
            limit: 1,
            message_key: Some(text(&old, "key").to_owned()),
            ..Page::default()
        },
    );
    assert_eq!(after["timeline"][0]["body"], "수정 답장");
    assert_eq!(after["page"]["end"], 2);
    assert_eq!(
        list(&db, "수정 답장", false, "", false)["items"][0]["recipient"],
        recipient
    );
    archive::rename_conversation(&db, &json!({"owner":"me@example.com","recipient":recipient,"name":"새 이름","original_name":"날짜 검토"})).unwrap();
    assert_eq!(
        list(&db, "새 이름", false, "", false)["items"][0]["name"],
        "새 이름"
    );
}

#[test]
fn external_writes_invalidate_only_changed_contacts_and_reject_stale_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let db = db::connect(&path).unwrap();
    load_tables(&db, &legacy()["tables"]);
    mailbox::initialize(&db).unwrap();
    let old = page(
        &db,
        "boss@example.com",
        "",
        Page {
            limit: 1,
            ..Page::default()
        },
    );
    let other = page(
        &db,
        "peer@example.com",
        "",
        Page {
            limit: 1,
            ..Page::default()
        },
    );
    let before = std::fs::read(&path).unwrap();
    list(&db, "", false, "", false);
    assert_eq!(
        before,
        std::fs::read(&path).unwrap(),
        "warm reads must not rewrite the index"
    );
    let writer = db::open(&path, false).unwrap();
    let mail = archive::get_mail(&writer, 1).unwrap();
    archive::update_mail(
        &writer,
        1,
        &json!({"revision":0,"authored_body":"변경 본문","recipients":mail["recipients"]}),
    )
    .unwrap();
    let work = Work::default();
    let result = mailbox::read(&db, &work, |db| {
        mailbox::conversation_page(
            db,
            "me@example.com",
            "boss@example.com",
            "",
            Page {
                limit: 1,
                revision: old["page"]["revision"].as_i64(),
                ..Page::default()
            },
            &work,
        )
    });
    assert_eq!(
        result
            .unwrap_err()
            .downcast_ref::<mimicmail::Fault>()
            .unwrap()
            .0,
        409
    );
    assert_eq!(
        page(
            &db,
            "peer@example.com",
            "",
            Page {
                limit: 1,
                ..Page::default()
            }
        )["page"]["revision"],
        other["page"]["revision"]
    );
    equivalent(&db);
}

#[test]
fn cancelled_rebuild_rolls_back_and_can_be_retried() {
    let db = populated();
    mailbox::initialize(&db).unwrap();
    let work = Work::default();
    work.cancel();
    assert!(
        mailbox::read(&db, &work, |db| mailbox::conversations(
            db, "", false, "", false, &work
        ))
        .is_err()
    );
    assert!(db::exists(&db, "SELECT 1 FROM browse_dirty", []).unwrap());
    assert!(!db::exists(&db, "SELECT 1 FROM browse_contacts", []).unwrap());
    equivalent(&db);
    let owner = privacy::person_id("me@example.com");
    assert!(db::exists(&db, "SELECT 1 FROM browse_contacts WHERE owner=?", [owner]).unwrap());
}

#[test]
fn shared_mail_changes_and_search_normalization_match_uncached_queries() {
    let db = populated();
    mailbox::initialize(&db).unwrap();
    let mail = archive::get_mail(&db, 1).unwrap();
    let owner = text(&mail, "owner");
    let fp = text(&mail, "fingerprint");
    db.execute("INSERT INTO sent_mail SELECT owner,fingerprint,message_id,sender,'person:shared',role,relationship,purpose,authored_body FROM sent_mail WHERE owner=? AND fingerprint=? LIMIT 1",rusqlite::params![owner,fp]).unwrap();
    list(&db, "", false, "", false);
    db.execute("UPDATE sent_mail SET authored_body='&amp;amp;Bonjour 😀 Straße' WHERE owner=? AND fingerprint=? AND recipient=(SELECT min(recipient) FROM sent_mail WHERE owner=? AND fingerprint=?)",rusqlite::params![owner,fp,owner,fp]).unwrap();
    for query in ["&Bonjour", "&amp;Bonjour", "STRASSE", "😀"] {
        assert_eq!(
            list(&db, query, false, "", false),
            archive::conversations(&db, query, false, "", false).unwrap(),
            "{query}"
        );
    }
    let full = archive::conversation(&db, owner, "person:shared").unwrap();
    let paged = page(
        &db,
        "person:shared",
        "",
        Page {
            limit: 100,
            ..Page::default()
        },
    );
    assert_eq!(paged["timeline"], full["timeline"]);
}
