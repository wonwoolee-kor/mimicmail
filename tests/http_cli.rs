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
    for asset in ["/app.js", "/app.css", "/label-colors.mjs"] {
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
        .json(&json!({"owner":"me@example.com","recipient":"new@example.com","label":"교수"}))
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
