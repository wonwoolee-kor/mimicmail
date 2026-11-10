mod common;
use common::*;
use mimicmail::{array, db, gui};
use serde_json::json;

#[test]
fn label_colors_are_shared_optional_persistent_and_conflict_checked() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let db = db::connect(&path).unwrap();
    load_tables(&db, &legacy()["tables"]);
    for recipient in ["boss@example.com", "peer@example.com"] {
        gui::update_contact_label(
            &db,
            &json!({"owner":"me@example.com","recipient":recipient,"label":"교수","revision":0}),
        )
        .unwrap();
    }
    assert!(
        array(&gui::conversations(&db, "", false, "").unwrap(), "items")
            .iter()
            .all(|i| i["label_color"] == "")
    );
    let payload = json!({"owner":"me@example.com","recipient":"boss@example.com","label":"교수","revision":1,"color":"#3366CC","color_revision":0});
    gui::update_contact_label(&db, &payload).unwrap();
    drop(db);
    let db = db::connect(&path).unwrap();
    let list = gui::conversations(&db, "", false, "").unwrap();
    assert!(
        array(&list, "items")
            .iter()
            .all(|i| i["label_color"] == "#3366cc")
    );
    let mut stale = payload.clone();
    stale["recipient"] = json!("peer@example.com");
    assert!(gui::update_contact_label(&db, &stale).is_err());
    assert_eq!(gui::conversations(&db, "", false, "").unwrap(), list);
    let mut invalid = payload.clone();
    invalid["revision"] = json!(2);
    invalid["color_revision"] = json!(1);
    invalid["color"] = json!("red");
    assert!(gui::update_contact_label(&db, &invalid).is_err());
    assert_eq!(gui::conversations(&db, "", false, "").unwrap(), list);
    invalid["color"] = json!("");
    gui::update_contact_label(&db, &invalid).unwrap();
    assert!(
        array(&gui::conversations(&db, "", false, "").unwrap(), "items")
            .iter()
            .all(|i| i["label_color"] == "")
    );
}

#[test]
fn unlabeled_filter_does_not_collide_with_user_label_names() {
    let db = populated();
    gui::update_contact_label(&db,&json!({"owner":"me@example.com","recipient":"boss@example.com","label":"미지정","revision":0})).unwrap();
    let unlabeled = gui::conversations_filtered(&db, "", false, "", true).unwrap();
    assert_eq!(array(&unlabeled, "items").len(), 1);
    assert_eq!(unlabeled["items"][0]["recipient"], "peer@example.com");
    let named = gui::conversations_filtered(&db, "", false, "미지정", false).unwrap();
    assert_eq!(named["items"][0]["recipient"], "boss@example.com");
    assert!(
        array(
            &gui::conversations_filtered(&db, "없는검색어", false, "", true).unwrap(),
            "items"
        )
        .is_empty()
    );
}

#[test]
fn search_returns_every_occurrence_with_browser_offsets() {
    let items = vec![
        json!({"keys":["sent:one"],"body":"😀 Straße STRASSE 끝","subject":"strasse"}),
        json!({"keys":["sent:two"],"body":"교환학생 교환학생","subject":""}),
    ];
    let matches = gui::search_matches(&items, "STRASSE");
    assert_eq!(matches.len(), 3);
    assert_eq!(
        (matches[0]["start"].as_u64(), matches[0]["end"].as_u64()),
        (Some(3), Some(9))
    );
    assert_eq!(matches[0]["term"], "Straße");
    assert_eq!(matches[1]["term"], "STRASSE");
    assert_eq!(gui::search_matches(&items, "교환학생").len(), 2);
    assert!(gui::search_matches(&items, "  ").is_empty());
}
