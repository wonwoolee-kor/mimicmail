#![cfg(unix)]
use mimicmail::{db, privacy, settings};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

const BIN: &str = env!("CARGO_BIN_EXE_mimicmail");

fn command(directory: &Path) -> Command {
    let mut command = Command::new(BIN);
    command.current_dir(directory).env_clear();
    command
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cli_overrides_environment_which_overrides_local_dotenv() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, source) in [
        ("file db.sqlite3", "file"),
        ("env.sqlite3", "env"),
        ("cli.sqlite3", "cli"),
    ] {
        let db = db::connect(&tmp.path().join(name)).unwrap();
        settings::update(
            &db,
            &json!({"revision":0,"entries":[{"key":"source","value":source}]}),
        )
        .unwrap();
    }
    std::fs::write(tmp.path().join(".env"), "MIMICMAIL_DB='file db.sqlite3' # quoted path\nMIMICMAIL_USER=file@example.com\nMIMICMAIL_BACKEND=openai\n").unwrap();
    let context = ["context", "--to", "recipient@example.com"];
    let file = success(command(tmp.path()).args(context).output().unwrap());
    assert_eq!(file["current_facts"]["general_context"]["source"], "file");
    assert_eq!(
        file["current_facts"]["user"],
        privacy::person_id("file@example.com")
    );

    let env = success(
        command(tmp.path())
            .env("MIMICMAIL_DB", "env.sqlite3")
            .env("MIMICMAIL_USER", "env@example.com")
            .args(context)
            .output()
            .unwrap(),
    );
    assert_eq!(env["current_facts"]["general_context"]["source"], "env");
    assert_eq!(
        env["current_facts"]["user"],
        privacy::person_id("env@example.com")
    );

    let cli = success(
        command(tmp.path())
            .env("MIMICMAIL_DB", "env.sqlite3")
            .env("MIMICMAIL_USER", "env@example.com")
            .env("MIMICMAIL_BACKEND", "invalid")
            .args([
                "--db",
                "cli.sqlite3",
                "--user",
                "cli@example.com",
                "--backend",
                "codex",
            ])
            .args(context)
            .output()
            .unwrap(),
    );
    assert_eq!(cli["current_facts"]["general_context"]["source"], "cli");
    assert_eq!(
        cli["current_facts"]["user"],
        privacy::person_id("cli@example.com")
    );
}

#[test]
fn dotenv_model_precedence_does_not_export_file_secrets_to_children() {
    let tmp = tempfile::tempdir().unwrap();
    let executable = tmp.path().join("codex");
    std::fs::write(&executable, r#"#!/bin/sh
chosen=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--model' ]; then shift; chosen="$1"; fi
  shift
done
[ "$chosen" = "$EXPECTED_MODEL" ] || exit 21
[ -z "${OPENAI_API_KEY+x}" ] || exit 22
cat >/dev/null
printf '%s' '{"purpose":"request","role":"unknown","role_evidence":"","relationship":"unknown","relationship_evidence":""}' > result.json
"#).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(tmp.path().join(".env"), "MIMICMAIL_USER=me@example.com\nCODEX_MODEL='file-model'\nOPENAI_API_KEY='not-exported#secret'\n").unwrap();
    std::fs::write(tmp.path().join("mail.eml"), "From: me@example.com\r\nTo: recipient@example.com\r\nSubject: Test\r\n\r\nPlease review this message.\r\n").unwrap();

    for source in ["file", "env", "cli"] {
        let expected = format!("{source}-model");
        let mut cmd = command(tmp.path());
        cmd.env("PATH", format!("{}:/usr/bin:/bin", tmp.path().display()))
            .env("EXPECTED_MODEL", &expected)
            .arg("--db")
            .arg(format!("{source}.sqlite3"));
        if source != "file" {
            cmd.env("CODEX_MODEL", "env-model");
        }
        if source == "cli" {
            cmd.args(["--model", "cli-model"]);
        }
        let result = success(cmd.args(["ingest", "mail.eml"]).output().unwrap());
        assert_eq!(result, json!({"indexed":1,"skipped":0}));
    }
}

#[test]
fn invalid_dotenv_fails_without_exposing_values_or_creating_database() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".env"),
        "OPENAI_API_KEY='secret-never-print\n",
    )
    .unwrap();
    let output = command(tmp.path())
        .args(["gui", "--no-open"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains(".env"));
    assert!(!error.contains("secret-never-print"));
    assert!(!tmp.path().join("data").exists());
    for flag in ["--help", "--version"] {
        assert!(
            command(tmp.path())
                .arg(flag)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
}

#[test]
fn missing_dotenv_does_not_search_parent_directories() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".env"),
        "OPENAI_API_KEY='invalid-parent-secret\n",
    )
    .unwrap();
    let child = tmp.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let path = child.join("mail.sqlite3");
    drop(db::connect(&path).unwrap());
    let before = std::fs::read(&path).unwrap();
    let result = success(
        command(&child)
            .args([
                "--db",
                "mail.sqlite3",
                "--user",
                "local",
                "context",
                "--to",
                "recipient",
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(result["current_facts"]["user"], "local");
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn openai_credentials_load_from_dotenv_without_making_an_api_call() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(".env"), "MIMICMAIL_USER=me@example.com\nMIMICMAIL_BACKEND=openai\nOPENAI_MODEL=test-model\nOPENAI_API_KEY='test#secret'\n").unwrap();
    let output = command(tmp.path())
        .args(["ingest", "missing.eml"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("경로가 없습니다"), "{error}");
    assert!(!error.contains("test#secret"));
    assert!(!tmp.path().join("data").exists());

    let output = command(tmp.path())
        .env("OPENAI_API_KEY", "")
        .args(["ingest", "missing.eml"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("OPENAI_API_KEY"));
}

#[test]
fn invalid_gui_port_is_rejected_before_database_initialization() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(".env"), "MIMICMAIL_PORT=70000\n").unwrap();
    let output = command(tmp.path())
        .args(["gui", "--no-open"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("MIMICMAIL_PORT"));
    assert!(!tmp.path().join("data").exists());
}
