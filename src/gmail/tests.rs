use super::*;
use crate::{archive, array, drafting, mailbox};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

type Request = (String, String, String);
struct Mock {
    root: String,
    steps: Arc<Mutex<VecDeque<(u16, Value)>>>,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Mock {
    fn new(steps: Vec<(u16, Value)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let steps = Arc::new(Mutex::new(VecDeque::from(steps)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (replies, calls, cancelled) = (steps.clone(), requests.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !cancelled.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let mut parts = first.split_whitespace();
                calls.lock().unwrap().push((
                    parts.next().unwrap().into(),
                    parts.next().unwrap().into(),
                    String::from_utf8(body).unwrap(),
                ));
                let (status, json) = replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or((418, json!({})));
                let body = json.to_string();
                write!(stream,"HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nRetry-After: 0\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        Self {
            root,
            steps,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    fn client(&self, settings: &Settings, directory: &Path) -> auth::Client {
        auth::Client::mock(settings, &self.root, &directory.join("token.json"))
    }
    fn done(&self) {
        assert!(self.steps.lock().unwrap().is_empty());
    }
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}
struct Model {
    calls: Vec<Value>,
    fail: bool,
}
impl LanguageModel for Model {
    fn complete(&mut self, _: &str, data: &Value, _: &Value) -> Result<Value> {
        self.calls.push(data.clone());
        if self.fail {
            self.fail = false;
            bail!("private model error");
        }
        Ok(
            json!({"purpose":"request","role":"unknown","role_evidence":"","relationship":"unknown","relationship_evidence":""}),
        )
    }
}
fn model() -> Model {
    Model {
        calls: vec![],
        fail: false,
    }
}
fn settings() -> Settings {
    Settings {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        owner: "me@work.test".into(),
        aliases: vec!["me@other.test".into()],
        interval: Duration::from_secs(120),
    }
}
fn database() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db::initialize(&db).unwrap();
    mailbox::initialize(&db).unwrap();
    db
}
fn profile(history: &str) -> (u16, Value) {
    (200, json!({"emailAddress":ACCOUNT,"historyId":history}))
}
fn raw(sender: &str, body: &str, extra: &str) -> Vec<u8> {
    format!("From: Sender <{sender}>\r\nTo: me@work.test, Boss <boss@work.test>\r\nSubject: Request\r\nMessage-ID: <original@work.test>\r\nDate: Wed, 11 Nov 2026 10:00:00 +0900\r\n{extra}Content-Type: text/plain; charset=utf-8\r\n\r\n{body}").into_bytes()
}
fn message(id: &str, raw: &[u8], labels: &[&str]) -> (u16, Value) {
    (
        200,
        json!({"id":id,"raw":URL_SAFE_NO_PAD.encode(raw),"internalDate":"1794358800000","labelIds":labels}),
    )
}
fn cursor(db: &Connection) -> String {
    db.query_row("SELECT history_id FROM gmail_sync", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn full_sync_paginates_deduplicates_and_routes_incoming_as_facts_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db = database();
    let settings = settings();
    let mock = Mock::new(vec![
        profile("100"),
        (
            200,
            json!({"messages":[{"id":"a"},{"id":"b"},{"id":"c"}],"nextPageToken":"page2"}),
        ),
        message("a", &raw("boss@work.test", "문의 me@work.test", ""), &[]),
        message("b", &raw("me@other.test", "작성한 답변", ""), &["SENT"]),
        message(
            "c",
            &raw(
                "boss@work.test",
                "문의 me@work.test",
                "Received: another route\r\n",
            ),
            &[],
        ),
        (200, json!({"messages":[{"id":"bad"},{"id":"spam"}]})),
        message("bad", b"not a mail", &[]),
        message("spam", b"not needed", &["SPAM"]),
    ]);
    let mut client = mock.client(&settings, tmp.path());
    collect(&db, &mut client, &settings, &Work::default()).unwrap();
    assert_eq!(cursor(&db), "100");
    let queued: String = db
        .query_row(
            "SELECT mail_json FROM gmail_messages WHERE gmail_id='a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!queued.contains('@'));
    let mut llm = model();
    analyze(&db, &mut llm, &Work::default()).unwrap();
    assert_eq!(llm.calls.len(), 2);
    assert!(!serde_json::to_string(&llm.calls).unwrap().contains('@'));
    let data = archive::conversation(&db, &settings.owner, "boss@work.test").unwrap();
    assert_eq!(array(&data, "messages").len(), 2);
    let incoming = array(&data, "timeline")
        .iter()
        .find(|i| i["kind"] == "incoming")
        .unwrap();
    assert_eq!(incoming["mine"], false);
    assert_eq!(incoming["sender"], privacy::person_id("boss@work.test"));
    let context = drafting::selected_context(
        &db,
        &settings.owner,
        "boss@work.test",
        "other",
        &[incoming["keys"][0].as_str().unwrap().into()],
        &[],
    )
    .unwrap();
    assert_eq!(array(&context, "style_candidates").len(), 1);
    assert!(
        context["current_facts"]["conversation"][0]["body"]
            .as_str()
            .unwrap()
            .contains("문의")
    );
    let cached = mailbox::read(&db, &Work::default(), |db| {
        mailbox::conversations(db, "문의", false, "", false, &Work::default())
    })
    .unwrap();
    assert_eq!(array(&cached, "items").len(), 1);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM gmail_messages WHERE mail_json IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let requests = mock.requests.lock().unwrap();
    assert!(requests[1].1.contains("includeSpamTrash=true"));
    assert!(!requests[1].1.contains("labelIds"));
    assert!(!requests[1].1.contains("q="));
    assert!(requests[5].1.contains("pageToken=page2"));
    mock.done();
}

#[test]
fn history_pages_use_added_messages_and_revisit_unquarantined_mail() {
    let tmp = tempfile::tempdir().unwrap();
    let db = database();
    let settings = settings();
    db.execute(
        "INSERT INTO gmail_sync VALUES (?,?,'100',NULL)",
        params![
            privacy::person_id(ACCOUNT),
            privacy::person_id(&settings.owner)
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO gmail_messages(account,gmail_id,owner,status) VALUES (?,'spam',?,'excluded')",
        params![
            privacy::person_id(ACCOUNT),
            privacy::person_id(&settings.owner)
        ],
    )
    .unwrap();
    let mock = Mock::new(vec![
        profile("150"),
        (
            200,
            json!({"history":[{"messages":[{"id":"ignored"}],"messagesAdded":[{"message":{"id":"new"}}]}],"nextPageToken":"next","historyId":"160"}),
        ),
        message("new", &raw("boss@work.test", "new", ""), &[]),
        (
            200,
            json!({"history":[{"messagesAdded":[{"message":{"id":"new"}}],"labelsRemoved":[{"message":{"id":"spam"},"labelIds":["SPAM"]}]}],"historyId":"170"}),
        ),
        message("spam", &raw("boss@work.test", "rescued", ""), &[]),
    ]);
    collect(
        &db,
        &mut mock.client(&settings, tmp.path()),
        &settings,
        &Work::default(),
    )
    .unwrap();
    assert_eq!(cursor(&db), "170");
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM gmail_messages WHERE status='pending'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    let calls = mock.requests.lock().unwrap();
    assert!(calls[1].1.contains("startHistoryId=100"));
    assert!(calls[3].1.contains("startHistoryId=100"));
    assert!(calls[3].1.contains("pageToken=next"));
    mock.done();
}

#[test]
fn failed_collection_preserves_jobs_without_advancing_cursor_then_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let db = database();
    let settings = settings();
    let mock = Mock::new(vec![
        profile("100"),
        (200, json!({"messages":[{"id":"a"},{"id":"b"}]})),
        message("a", &raw("boss@work.test", "a", ""), &[]),
        (403, json!({"error":"secret"})),
    ]);
    assert!(
        collect(
            &db,
            &mut mock.client(&settings, tmp.path()),
            &settings,
            &Work::default()
        )
        .is_err()
    );
    assert_eq!(cursor(&db), "");
    analyze(&db, &mut model(), &Work::default()).unwrap();
    mock.done();
    let mock = Mock::new(vec![
        profile("200"),
        (200, json!({"messages":[{"id":"a"},{"id":"b"}]})),
        message("b", &raw("boss@work.test", "b", ""), &[]),
    ]);
    collect(
        &db,
        &mut mock.client(&settings, tmp.path()),
        &settings,
        &Work::default(),
    )
    .unwrap();
    assert_eq!(cursor(&db), "200");
    analyze(&db, &mut model(), &Work::default()).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM mail_archive", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    mock.done();
}

#[test]
fn expired_history_rescans_and_get_404_does_not_block_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let db = database();
    let settings = settings();
    db.execute(
        "INSERT INTO gmail_sync VALUES (?,?,'old',NULL)",
        params![
            privacy::person_id(ACCOUNT),
            privacy::person_id(&settings.owner)
        ],
    )
    .unwrap();
    let mock = Mock::new(vec![
        profile("300"),
        (404, json!({})),
        (200, json!({"messages":[{"id":"gone"},{"id":"a"}]})),
        (404, json!({})),
        message("a", &raw("boss@work.test", "a", ""), &[]),
    ]);
    collect(
        &db,
        &mut mock.client(&settings, tmp.path()),
        &settings,
        &Work::default(),
    )
    .unwrap();
    assert_eq!(cursor(&db), "300");
    assert_eq!(
        db.query_row(
            "SELECT status FROM gmail_messages WHERE gmail_id='gone'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "gone"
    );
    mock.done();
}

#[test]
fn analysis_failures_retry_independently_and_keep_other_messages_moving() {
    let tmp = tempfile::tempdir().unwrap();
    let db = database();
    let settings = settings();
    let mock = Mock::new(vec![
        profile("400"),
        (200, json!({"messages":[{"id":"a"},{"id":"b"}]})),
        message("a", &raw("boss@work.test", "a", ""), &[]),
        message("b", &raw("boss@work.test", "b", ""), &[]),
    ]);
    collect(
        &db,
        &mut mock.client(&settings, tmp.path()),
        &settings,
        &Work::default(),
    )
    .unwrap();
    let mut llm = Model {
        calls: vec![],
        fail: true,
    };
    analyze(&db, &mut llm, &Work::default()).unwrap();
    assert_eq!(cursor(&db), "400");
    assert_eq!(llm.calls.len(), 2);
    assert_eq!(
        db.query_row(
            "SELECT status FROM gmail_messages WHERE gmail_id='a'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "pending"
    );
    analyze(&db, &mut llm, &Work::default()).unwrap();
    assert_eq!(llm.calls.len(), 2);
    db.execute("UPDATE gmail_messages SET retry_at=0", [])
        .unwrap();
    analyze(&db, &mut llm, &Work::default()).unwrap();
    assert_eq!(llm.calls.len(), 3);
    assert_eq!(
        db.query_row("SELECT count(*) FROM mail_archive", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    mock.done();
}

#[test]
fn wrong_account_and_changed_owner_never_fetch_mail() {
    let tmp = tempfile::tempdir().unwrap();
    let db = database();
    let settings = settings();
    let mock = Mock::new(vec![(
        200,
        json!({"emailAddress":"wrong@gmail.com","historyId":"1"}),
    )]);
    assert!(
        collect(
            &db,
            &mut mock.client(&settings, tmp.path()),
            &settings,
            &Work::default()
        )
        .is_err()
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM gmail_sync", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    mock.done();
    db.execute(
        "INSERT INTO gmail_sync VALUES (?,'another-owner','10',NULL)",
        [privacy::person_id(ACCOUNT)],
    )
    .unwrap();
    let mock = Mock::new(vec![profile("11")]);
    assert!(
        collect(
            &db,
            &mut mock.client(&settings, tmp.path()),
            &settings,
            &Work::default()
        )
        .is_err()
    );
    assert_eq!(cursor(&db), "10");
    mock.done();
}

#[test]
fn token_refresh_rejects_broader_scope_and_redacts_error_responses() {
    let tmp = tempfile::tempdir().unwrap();
    let settings = settings();
    let mock = Mock::new(vec![
        (401, json!({})),
        (
            200,
            json!({"access_token":"new-access","token_type":"Bearer","expires_in":3600,"scope":auth::SCOPE}),
        ),
        profile("1"),
    ]);
    mock.client(&settings, tmp.path()).profile().unwrap();
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests[1].0, "POST");
    assert!(requests[1].2.contains("grant_type=refresh_token"));
    assert!(requests[1].2.contains("refresh_token=test-refresh"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(tmp.path().join("token.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    mock.done();
    let mock = Mock::new(vec![
        (401, json!({})),
        (
            200,
            json!({"access_token":"private-access","refresh_token":"private-refresh","token_type":"Bearer","expires_in":3600,"scope":"https://www.googleapis.com/auth/gmail.modify"}),
        ),
    ]);
    let error = mock
        .client(&settings, tmp.path())
        .profile()
        .unwrap_err()
        .to_string();
    assert!(error.contains("gmail.readonly"));
    assert!(!error.contains("private"));
    mock.done();
    let mock = Mock::new(vec![
        (401, json!({})),
        (
            400,
            json!({"error":"invalid_grant","details":"private-refresh"}),
        ),
    ]);
    let error = mock
        .client(&settings, tmp.path())
        .profile()
        .unwrap_err()
        .to_string();
    assert!(error.contains("gmail connect"));
    assert!(!error.contains("private-refresh"));
    mock.done();
}

#[test]
fn deletion_purges_incoming_and_pending_mail_and_blocks_recollection() {
    let tmp = tempfile::tempdir().unwrap();
    let db = database();
    let settings = settings();
    let mock = Mock::new(vec![
        profile("1"),
        (200, json!({"messages":[{"id":"a"}]})),
        message("a", &raw("boss@work.test", "one", ""), &[]),
    ]);
    collect(
        &db,
        &mut mock.client(&settings, tmp.path()),
        &settings,
        &Work::default(),
    )
    .unwrap();
    analyze(&db, &mut model(), &Work::default()).unwrap();
    mock.done();
    let mock = Mock::new(vec![
        profile("2"),
        (
            200,
            json!({"history":[{"messagesAdded":[{"message":{"id":"b"}}]}],"historyId":"2"}),
        ),
        message("b", &raw("boss@work.test", "two", ""), &[]),
    ]);
    collect(
        &db,
        &mut mock.client(&settings, tmp.path()),
        &settings,
        &Work::default(),
    )
    .unwrap();
    mock.done();
    archive::delete_conversation(
        &db,
        &json!({"owner":settings.owner,"recipient":"boss@work.test"}),
    )
    .unwrap();
    for table in ["mail_archive", "incoming_mail"] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM gmail_messages WHERE mail_json IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let mock = Mock::new(vec![
        profile("3"),
        (
            200,
            json!({"history":[{"messagesAdded":[{"message":{"id":"c"}}]}],"historyId":"3"}),
        ),
        message("c", &raw("boss@work.test", "three", ""), &[]),
    ]);
    collect(
        &db,
        &mut mock.client(&settings, tmp.path()),
        &settings,
        &Work::default(),
    )
    .unwrap();
    mock.done();
    let mut llm = model();
    analyze(&db, &mut llm, &Work::default()).unwrap();
    assert!(llm.calls.is_empty());
    assert_eq!(
        db.query_row(
            "SELECT status FROM gmail_messages WHERE gmail_id='c'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "deleted"
    );
    assert!(
        array(
            &archive::conversation(&db, &settings.owner, "boss@work.test").unwrap(),
            "timeline"
        )
        .is_empty()
    );
}

#[test]
fn lock_cancellation_and_rate_limit_retry_are_bounded() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mail.db");
    let first = lock(&path).unwrap();
    assert!(lock(&path).is_err());
    drop(first);
    assert!(lock(&path).is_ok());
    let work = Work::default();
    work.cancel();
    assert!(auth::wait(&work, Duration::from_secs(3600)).is_err());
    let mock = Mock::new(vec![(429, json!({})), profile("1")]);
    mock.client(&settings(), tmp.path()).profile().unwrap();
    mock.done();
}

#[test]
fn background_classification_preserves_labels_edited_during_another_recipient_call() {
    struct EditingModel<'a> {
        db: &'a Connection,
        calls: usize,
    }
    impl LanguageModel for EditingModel<'_> {
        fn complete(&mut self, _: &str, _: &Value, _: &Value) -> Result<Value> {
            self.calls += 1;
            if self.calls == 2 {
                self.db.execute(
                    "UPDATE contact_labels SET label='새 라벨',revision=revision+1",
                    [],
                )?;
            }
            model().complete("", &Value::Null, &Value::Null)
        }
    }
    let db = database();
    let settings = settings();
    let original = raw("me@other.test", "발신 본문", "Cc: second@work.test\r\n");
    let (_, remote) = message("a", &original, &["SENT"]);
    let (mail, recipients, _) = prepare(&remote, &settings).unwrap();
    let owner = privacy::person_id(&settings.owner);
    db.execute(
        "INSERT INTO contact_labels(owner,recipient,label) VALUES (?,?,'이전 라벨')",
        params![owner, privacy::person_id("boss@work.test")],
    )
    .unwrap();
    let mut llm = EditingModel { db: &db, calls: 0 };
    pipeline::index_mail(
        &db,
        &mut llm,
        &mail,
        &owner,
        &recipients,
        "Gmail",
        &Work::default(),
    )
    .unwrap();
    assert_eq!(llm.calls, 2);
    let role: String = db
        .query_row(
            "SELECT role FROM sent_mail WHERE recipient=?",
            [privacy::person_id("boss@work.test")],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(role, "새 라벨");
}
