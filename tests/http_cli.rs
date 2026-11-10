#![cfg(unix)]
mod common;
use common::*;
use mimicmail::{db, gui};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    time::Duration,
};
const BIN: &str = env!("CARGO_BIN_EXE_mimicmail");
struct Server {
    child: Child,
    url: String,
    tmp: tempfile::TempDir,
}
impl Server {
    fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mail.sqlite3");
        let db = db::connect(&path).unwrap();
        load_tables(&db, &legacy()["tables"]);
        drop(db);
        let mut child = Command::new(BIN)
            .args([
                "--db",
                path.to_str().unwrap(),
                "gui",
                "--port",
                "0",
                "--no-open",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let url = line
            .trim()
            .strip_prefix("Mimicmail · ")
            .expect("server URL")
            .to_owned();
        Self { child, url, tmp }
    }
    fn dbpath(&self) -> std::path::PathBuf {
        self.tmp.path().join("mail.sqlite3")
    }
    fn token(&self, client: &reqwest::blocking::Client) -> String {
        client
            .get(format!("{}/api/conversations", self.url))
            .send()
            .unwrap()
            .json::<Value>()
            .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}
#[test]
fn paged_http_keeps_full_timeline_and_rejects_stale_revisions() {
    let server = Server::start();
    let client = client();
    let endpoint = format!("{}/api/conversation", server.url);
    let contact = [
        ("owner", "me@example.com"),
        ("recipient", "boss@example.com"),
    ];
    let full = client
        .get(&endpoint)
        .query(&contact)
        .send()
        .unwrap()
        .json::<Value>()
        .unwrap();
    let mut before = None;
    let mut combined = Vec::new();
    let revision = loop {
        let mut request = client
            .get(&endpoint)
            .query(&contact)
            .query(&[("compact", "1"), ("limit", "1")]);
        if let Some(before) = before {
            request = request.query(&[("before", before)]);
        }
        let page = request.send().unwrap().json::<Value>().unwrap();
        let mut items = page["timeline"].as_array().unwrap().clone();
        assert!(items.len() <= 1);
        items.extend(combined);
        combined = items;
        let start = page["page"]["start"].as_u64().unwrap();
        if start == 0 {
            break page["page"]["revision"].clone();
        }
        before = Some(start);
    };
    assert_eq!(json!(combined), full["timeline"]);
    let token = server.token(&client);
    let source = &full["messages"][0];
    assert_eq!(client.put(format!("{}/api/mail/{}",server.url,source["id"])).header("X-Mimicmail-Token",token)
        .json(&json!({"revision":source["revision"],"authored_body":"페이지 수정 검증","recipients":source["recipients"]})).send().unwrap().status(),200);
    assert_eq!(
        client
            .get(&endpoint)
            .query(&contact)
            .query(&[
                ("compact", "1"),
                ("limit", "1"),
                ("revision", &revision.to_string())
            ])
            .send()
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        client
            .get(&endpoint)
            .query(&contact)
            .query(&[("compact", "1"), ("limit", "-1")])
            .send()
            .unwrap()
            .status(),
        400
    );
}
#[test]
fn http_reply_edit_and_conversation_rename_persist_and_reveal_saved_message() {
    let server = Server::start();
    let client = client();
    let token = server.token(&client);
    let created: Value = client
        .post(format!("{}/api/conversation", server.url))
        .header("X-Mimicmail-Token", &token)
        .json(&json!({"owner":"me@example.com","name":"오타"}))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    let recipient = &created["contact"]["recipient"];
    let payload = json!({"owner":"me@example.com","recipient":recipient,"body":"처음 답장"});
    let saved: Value = client
        .post(format!("{}/api/reply", server.url))
        .header("X-Mimicmail-Token", &token)
        .json(&payload)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    let edit = json!({"owner":"me@example.com","recipient":recipient,"key":saved["key"],"body":"바꾼 답장","original_body":"처음 답장"});
    assert_eq!(
        client
            .put(format!("{}/api/reply", server.url))
            .json(&edit)
            .send()
            .unwrap()
            .status(),
        403
    );
    client
        .put(format!("{}/api/reply", server.url))
        .header("X-Mimicmail-Token", &token)
        .json(&edit)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    client.put(format!("{}/api/conversation", server.url)).header("X-Mimicmail-Token", &token)
        .json(&json!({"owner":"me@example.com","recipient":recipient,"name":"수정된 이름","original_name":"오타"}))
        .send().unwrap().error_for_status().unwrap();
    let result: Value = client
        .get(format!("{}/api/conversation", server.url))
        .query(&[
            ("owner", "me@example.com"),
            ("recipient", recipient.as_str().unwrap()),
            ("compact", "1"),
            ("limit", "1"),
            ("before", "0"),
            ("key", saved["key"].as_str().unwrap()),
        ])
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(result["timeline"][0]["body"], "바꾼 답장");
    assert_eq!(result["timeline"][0]["sender_name"], "수정된 이름");
    assert!(result["timeline"][0].get("sent_at").is_none());
    let db = db::open(&server.dbpath(), true).unwrap();
    assert!(
        !db::rows(&db, "PRAGMA table_info(received_replies)", [])
            .unwrap()
            .iter()
            .any(|r| r["name"] == "sent_at")
    );
}
#[test]
fn http_static_assets_host_origin_token_and_edit_conflict() {
    let server = Server::start();
    let client = client();
    let root = client.get(&server.url).send().unwrap();
    assert_eq!(root.status(), 200);
    assert!(
        root.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    for asset in ["/app.js", "/app.css", "/label-colors.mjs", "/api.mjs"] {
        assert_eq!(
            client
                .get(format!("{}{asset}", server.url))
                .send()
                .unwrap()
                .status(),
            200
        );
    }
    assert_eq!(
        client
            .get(format!("{}/api/conversations", server.url))
            .header("Host", "untrusted.example")
            .send()
            .unwrap()
            .status(),
        403
    );
    let token = server.token(&client);
    let db = db::open(&server.dbpath(), true).unwrap();
    let mail = gui::get_mail(&db, 1).unwrap();
    let payload =
        json!({"revision":0,"authored_body":"수정한 내 문체","recipients":mail["recipients"]});
    let endpoint = format!("{}/api/mail/1", server.url);
    assert_eq!(
        client
            .put(&endpoint)
            .json(&payload)
            .send()
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .put(&endpoint)
            .header("X-Mimicmail-Token", &token)
            .header("Origin", "https://untrusted.example")
            .json(&payload)
            .send()
            .unwrap()
            .status(),
        403
    );
    let saved = client
        .put(&endpoint)
        .header("X-Mimicmail-Token", &token)
        .header("Origin", &server.url)
        .json(&payload)
        .send()
        .unwrap();
    assert_eq!(saved.status(), 200);
    assert_eq!(saved.json::<Value>().unwrap()["revision"], 1);
    assert_eq!(
        client
            .put(&endpoint)
            .header("X-Mimicmail-Token", &token)
            .json(&payload)
            .send()
            .unwrap()
            .status(),
        409
    );
}
#[test]
fn http_create_delete_validation_and_readonly_context() {
    let server = Server::start();
    let client = client();
    let token = server.token(&client);
    let created = client
        .post(format!("{}/api/conversation", server.url))
        .header("X-Mimicmail-Token", &token)
        .json(&json!({"owner":"me@example.com","name":"새 수신자","label":"교수"}))
        .send()
        .unwrap();
    assert_eq!(created.status(), 200);
    assert_eq!(created.json::<Value>().unwrap()["created"], true);
    let data = client
        .get(format!(
            "{}/api/conversation?owner=me%40example.com&recipient=boss%40example.com",
            server.url
        ))
        .send()
        .unwrap()
        .json::<Value>()
        .unwrap();
    assert!(
        data["prompt_reference"]["context_command"]
            .as_str()
            .unwrap()
            .contains("mimicmail' '--db'")
    );
    let item = data["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "sent")
        .unwrap();
    let payload = json!({"owner":"me@example.com","recipient":"boss@example.com","key":item["keys"][0],"revision":0});
    assert_eq!(
        client
            .delete(format!("{}/api/message", server.url))
            .header("X-Mimicmail-Token", &token)
            .json(&payload)
            .send()
            .unwrap()
            .json::<Value>()
            .unwrap(),
        json!({"deleted":true})
    );
    assert_eq!(
        client
            .post(format!("{}/api/conversation", server.url))
            .header("X-Mimicmail-Token", &token)
            .header("Content-Type", "application/json")
            .body("[]")
            .send()
            .unwrap()
            .status(),
        400
    );
    let before = std::fs::read(server.dbpath()).unwrap();
    let result = Command::new(BIN)
        .args([
            "--db",
            server.dbpath().to_str().unwrap(),
            "--user",
            "me@example.com",
            "context",
            "--to",
            "new@example.com",
        ])
        .env_remove("OPENAI_API_KEY")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["current_facts"]["conversation"],
        json!([])
    );
    assert_eq!(before, std::fs::read(server.dbpath()).unwrap());
}
#[test]
fn http_reply_save_requires_token_and_is_available_to_readonly_context() {
    let server = Server::start();
    let client = client();
    let token = server.token(&client);
    let endpoint = format!("{}/api/reply", server.url);
    let payload = json!({"owner":"me@example.com","recipient":"boss@example.com","body":"HTTP로 저장한 상대 답장"});
    assert_eq!(
        client
            .post(&endpoint)
            .json(&payload)
            .send()
            .unwrap()
            .status(),
        403
    );
    let saved = client
        .post(&endpoint)
        .header("X-Mimicmail-Token", &token)
        .json(&payload)
        .send()
        .unwrap();
    assert_eq!(saved.status(), 200);
    let saved: Value = saved.json().unwrap();
    let result = Command::new(BIN)
        .args([
            "--db",
            server.dbpath().to_str().unwrap(),
            "--user",
            "me@example.com",
            "context",
            "--to",
            "boss@example.com",
            "--history-key",
            saved["key"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    let context: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        context["current_facts"]["conversation"][0]["body"],
        payload["body"]
    );
    assert_eq!(context["style_references"], json!([]));
    assert_eq!(
        client
            .post(&endpoint)
            .header("X-Mimicmail-Token", &token)
            .json(&json!({"owner":"me@example.com","recipient":"boss@example.com","body":"  "}))
            .send()
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .post(&endpoint)
            .header("X-Mimicmail-Token", &token)
            .json(&json!({"owner":"me@example.com","recipient":"missing","body":"내용"}))
            .send()
            .unwrap()
            .status(),
        404
    );
}

#[test]
fn oversized_http_bodies_and_unsupported_routes_are_rejected() {
    let server = Server::start();
    let client = client();
    let token = server.token(&client);
    let addr = server.url.strip_prefix("http://").unwrap();
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    write!(stream,"POST /api/conversation HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nX-Mimicmail-Token: {token}\r\nContent-Length: 1000001\r\n\r\n").unwrap();
    let mut status = String::new();
    BufReader::new(stream).read_line(&mut status).unwrap();
    assert!(status.contains("400"));
    assert_eq!(
        client
            .get(format!("{}/missing", server.url))
            .send()
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .patch(format!("{}/api/conversation", server.url))
            .send()
            .unwrap()
            .status(),
        405
    );
}
#[test]
fn incomplete_http_headers_time_out() {
    let server = Server::start();
    let addr = server.url.strip_prefix("http://").unwrap();
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream.write_all(b"GET / HTTP/1.1\r\nHost:").unwrap();
    let mut buffer = [0u8; 128];
    match stream.read(&mut buffer) {
        Ok(0) => {}
        Ok(n) => assert!(String::from_utf8_lossy(&buffer[..n]).contains("408")),
        Err(e) => assert_ne!(e.kind(), std::io::ErrorKind::WouldBlock),
    }
}
#[test]
fn cli_missing_credentials_and_missing_readonly_db_have_no_side_effect() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("missing.sqlite3");
    let result = Command::new(BIN)
        .args([
            "--db",
            path.to_str().unwrap(),
            "--user",
            "me@example.com",
            "--backend",
            "openai",
            "ingest",
            tmp.path().to_str().unwrap(),
        ])
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENAI_MODEL")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("OPENAI_API_KEY"));
    assert!(!path.exists());
    let result = Command::new(BIN)
        .args([
            "--db",
            path.to_str().unwrap(),
            "--user",
            "me@example.com",
            "context",
            "--to",
            "boss@example.com",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!path.exists());
}
#[test]
fn cli_ingest_and_stdin_draft_use_rust_and_do_not_save_current_mail() {
    let tmp = tempfile::tempdir().unwrap();
    let executable = tmp.path().join("codex");
    std::fs::write(&executable,"#!/bin/sh\ncat > /dev/null\nif /usr/bin/grep -q missing_facts schema.json; then\nprintf '%s' '{\"subject\":\"답장\",\"body\":\"금요일까지 전달드리겠습니다.\",\"missing_facts\":[]}' > result.json\nelif /usr/bin/grep -q history_keys schema.json; then\nprintf '%s' '{\"history_keys\":[],\"style_ids\":[]}' > result.json\nelse\nprintf '%s' '{\"purpose\":\"request\",\"role\":\"unknown\",\"role_evidence\":\"\",\"relationship\":\"unknown\",\"relationship_evidence\":\"\"}' > result.json\nfi\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = tmp.path().join("mail.sqlite3");
    let sent = tmp.path().join("sent.eml");
    std::fs::write(
        &sent,
        raw("me@example.com", "boss@example.com", "보낸 문체", ""),
    )
    .unwrap();
    let result = Command::new(BIN)
        .args([
            "--db",
            path.to_str().unwrap(),
            "--user",
            "me@example.com",
            "ingest",
            sent.to_str().unwrap(),
        ])
        .env("PATH", format!("{}:/usr/bin:/bin", tmp.path().display()))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap(),
        json!({"indexed":1,"skipped":0})
    );
    let mut child = Command::new(BIN)
        .args([
            "--db",
            path.to_str().unwrap(),
            "--user",
            "me@example.com",
            "draft",
            "-",
            "--to",
            "boss@example.com",
            "--request",
            "금요일까지 전달하겠다고 답장",
        ])
        .env("PATH", format!("{}:/usr/bin:/bin", tmp.path().display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&raw(
            "boss@example.com",
            "me@example.com",
            "검토 일정 알려 주세요.",
            "",
        ))
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap()["body"],
        "금요일까지 전달드리겠습니다."
    );
    let db = db::open(&path, true).unwrap();
    assert_eq!(
        db::one(&db, "SELECT count(*) AS n FROM mail_archive", [])
            .unwrap()
            .unwrap()["n"],
        1
    );
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 3);
}

#[test]
fn http_stale_session_rejects_without_writing_and_fresh_token_saves_color() {
    let client = client();
    let previous = Server::start();
    let old_token = previous.token(&client);
    drop(previous);
    let server = Server::start();
    let endpoint = format!("{}/api/contact-label", server.url);
    let payload = json!({"owner":"me@example.com","recipient":"boss@example.com","label":"교수","revision":0,"color":"#b95f5b","color_revision":0});
    let rejected = client
        .put(&endpoint)
        .header("X-Mimicmail-Token", &old_token)
        .header("Origin", &server.url)
        .json(&payload)
        .send()
        .unwrap();
    assert_eq!(rejected.status(), 403);
    assert_eq!(rejected.json::<Value>().unwrap()["code"], "session_expired");
    let blocked_origin = client
        .put(&endpoint)
        .header("X-Mimicmail-Token", &old_token)
        .header("Origin", "https://untrusted.example")
        .json(&payload)
        .send()
        .unwrap();
    assert_eq!(blocked_origin.status(), 403);
    assert!(
        blocked_origin
            .json::<Value>()
            .unwrap()
            .get("code")
            .is_none()
    );
    let saved = client
        .put(&endpoint)
        .header("X-Mimicmail-Token", server.token(&client))
        .header("Origin", &server.url)
        .json(&payload)
        .send()
        .unwrap();
    assert_eq!(saved.status(), 200);
    assert_eq!(saved.json::<Value>().unwrap()["revision"], 1);
    let data = client
        .get(format!("{}/api/conversations", server.url))
        .send()
        .unwrap()
        .json::<Value>()
        .unwrap();
    assert_eq!(data["label_colors"]["교수"]["color"], "#b95f5b");
}
