mod common;
use common::*;
use mimicmail::{array, db, gui, pipeline, text};
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
    assert_eq!(
        unlabeled["items"][0]["recipient"],
        mimicmail::privacy::person_id("peer@example.com")
    );
    let named = gui::conversations_filtered(&db, "", false, "미지정", false).unwrap();
    assert_eq!(
        named["items"][0]["recipient"],
        mimicmail::privacy::person_id("boss@example.com")
    );
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

#[test]
fn history_is_opt_in_and_preserves_original_facts_separate_from_edited_style() {
    let db = populated();
    let mail = gui::get_mail(&db, 1).unwrap();
    gui::update_mail(
        &db,
        1,
        &json!({"revision":0,"authored_body":"편집된 문체입니다.","recipients":mail["recipients"]}),
    )
    .unwrap();
    let catalog = gui::draft_context(&db, "me@example.com", "boss@example.com", "other").unwrap();
    assert_eq!(catalog["current_facts"]["conversation"], json!([]));
    assert_eq!(catalog["style_references"], json!([]));
    assert!(!catalog.to_string().contains("편집된 문체입니다."));
    let id = text(&catalog["style_candidates"][0], "fingerprint").to_owned();
    let key = format!("sent:{id}");
    let selected = gui::selected_context(
        &db,
        "me@example.com",
        "boss@example.com",
        "other",
        std::slice::from_ref(&key),
        std::slice::from_ref(&id),
    )
    .unwrap();
    assert_eq!(array(&selected["current_facts"], "conversation").len(), 1);
    assert_eq!(
        selected["current_facts"]["conversation"][0]["body"],
        mail["display_original_authored"]
    );
    assert_eq!(
        selected["style_references"][0]["authored_body"],
        "편집된 문체입니다."
    );
    gui::delete_message(
        &db,
        &json!({"owner":"me@example.com","recipient":"boss@example.com","key":key,"revision":1}),
    )
    .unwrap();
    assert!(
        gui::selected_context(
            &db,
            "me@example.com",
            "boss@example.com",
            "other",
            &[key],
            &[id]
        )
        .is_err()
    );
}

#[test]
fn new_person_candidates_cover_all_label_members_without_other_labels_or_owners() {
    let db = memory();
    for i in 0..9 {
        let recipient = format!("person{i}@example.com");
        let label = if i == 8 { "다른 라벨" } else { "교수" };
        let owner = if i == 7 {
            "another@example.com"
        } else {
            "me@example.com"
        };
        db.execute(
            "INSERT INTO sent_mail VALUES (?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                owner,
                format!("fp{i}"),
                format!("id{i}"),
                owner,
                recipient,
                label,
                "peer",
                "thanks",
                format!("본문{i}")
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO contact_labels VALUES (?,?,?,1)",
            rusqlite::params![owner, recipient, label],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO deleted_messages VALUES ('me@example.com','sent:fp6')",
        [],
    )
    .unwrap();
    db.pragma_update(None, "user_version", 0).unwrap();
    db::initialize(&db).unwrap();
    let created = gui::create_conversation(
        &db,
        &json!({"owner":"me@example.com","name":"새 수신자","label":"교수"}),
    )
    .unwrap();
    let recipient = created["contact"]["recipient"].as_str().unwrap();
    let catalog = gui::draft_context(&db, "me@example.com", recipient, "thanks").unwrap();
    assert_eq!(array(&catalog, "style_candidates").len(), 6);
    assert!(array(&catalog, "history_candidates").is_empty());
    assert_eq!(catalog["style_references"], json!([]));
    let selected = gui::selected_context(
        &db,
        "me@example.com",
        recipient,
        "thanks",
        &[],
        &["fp5".into()],
    )
    .unwrap();
    assert_eq!(array(&selected, "style_references").len(), 1);
    assert_eq!(selected["style_references"][0]["authored_body"], "본문5");
    assert_eq!(selected["current_facts"]["conversation"], json!([]));
    for id in ["fp6", "fp7", "fp8"] {
        assert!(
            gui::selected_context(
                &db,
                "me@example.com",
                recipient,
                "thanks",
                &[],
                &[id.into()]
            )
            .is_err()
        );
    }
    assert!(
        array(
            &gui::draft_context(&db, "me@example.com", "unlabeled@example.com", "thanks").unwrap(),
            "style_candidates"
        )
        .is_empty()
    );
}

#[test]
fn draft_model_can_skip_history_or_select_only_a_needed_message() {
    let db = populated();
    let catalog = gui::draft_context(&db, "me@example.com", "boss@example.com", "other").unwrap();
    let key = catalog["history_candidates"][0]["key"].clone();
    for keys in [json!([]), json!([key])] {
        let mut llm = Fake::new(vec![
            unknown(),
            json!({"history_keys":keys,"style_ids":[]}),
            json!({"subject":"초안","body":"감사합니다.","missing_facts":[]}),
        ]);
        pipeline::draft(
            &db,
            &mut llm,
            &raw("boss@example.com", "me@example.com", "현재 요청", ""),
            pipeline::DraftOptions {
                owner: "me@example.com",
                recipient: "boss@example.com",
                request: "답장",
                role: None,
                relationship: None,
                limit: 5,
            },
        )
        .unwrap();
        let generation = &llm.calls[2].1;
        assert_eq!(
            array(&generation["current_facts"], "previous_conversation").len(),
            keys.as_array().unwrap().len()
        );
        assert_eq!(generation["style_references"], json!([]));
        assert!(generation.get("history_candidates").is_none());
        assert!(
            !llm.calls[1]
                .1
                .to_string()
                .contains("안녕하세요. 검토 부탁드립니다.")
        );
    }
}

#[test]
fn invalid_model_reference_selection_never_reaches_generation() {
    let db = populated();
    for choice in [
        json!({"history_keys":["sent:other-person"],"style_ids":[]}),
        json!({"history_keys":[],"style_ids":["other-person"]}),
        json!({"history_keys":[],"style_ids":["one","two"]}),
    ] {
        let mut llm = Fake::new(vec![unknown(), choice]);
        assert!(
            pipeline::draft(
                &db,
                &mut llm,
                &raw("boss@example.com", "me@example.com", "현재 요청", ""),
                pipeline::DraftOptions {
                    owner: "me@example.com",
                    recipient: "boss@example.com",
                    request: "답장",
                    role: None,
                    relationship: None,
                    limit: 1
                }
            )
            .is_err()
        );
        assert_eq!(llm.calls.len(), 2);
    }
}
