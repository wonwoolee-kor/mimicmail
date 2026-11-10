#![cfg(unix)]
use mimicmail::llm::{Codex, LanguageModel, OpenAI, object_schema, response_object};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, time::Duration};
fn script(tmp: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
    let path = tmp.path().join("codex");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}
#[test]
fn codex_uses_stdin_schema_ephemeral_and_removes_temporary_files() {
    let tmp = tempfile::tempdir().unwrap();
    let capture = tmp.path().join("capture");
    std::fs::create_dir(&capture).unwrap();
    let path = script(
        &tmp,
        &format!(
            "pwd > '{0}/cwd'\nprintf '%s\\n' \"$@\" > '{0}/args'\ncat > '{0}/input'\ncp schema.json '{0}/schema'\nprintf '%s' '{{\"value\":\"완료\"}}' > result.json",
            capture.display()
        ),
    );
    let schema = object_schema(json!({"value":{"type":"string"}}));
    let data = json!({"메일":"본문"});
    let result = Codex {
        executable: path,
        model: Some("chosen-model".into()),
        timeout: Duration::from_secs(2),
    }
    .complete("분류", &data, &schema)
    .unwrap();
    assert_eq!(result, json!({"value":"완료"}));
    let args = std::fs::read_to_string(capture.join("args")).unwrap();
    assert!(args.contains("--ephemeral\n"));
    assert!(args.contains("--sandbox\nread-only\n"));
    assert!(args.contains("--model\nchosen-model\n"));
    assert!(!args.contains("본문"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(capture.join("input")).unwrap())
            .unwrap(),
        data
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(capture.join("schema")).unwrap()
        )
        .unwrap(),
        schema
    );
    let cwd = std::fs::read_to_string(capture.join("cwd")).unwrap();
    assert!(!std::path::Path::new(cwd.trim()).exists());
}
#[test]
fn codex_default_model_is_omitted() {
    let tmp = tempfile::tempdir().unwrap();
    let path = script(
        &tmp,
        "for arg in \"$@\"; do [ \"$arg\" = --model ] && exit 2; done\ncat >/dev/null\nprintf '{}' > result.json",
    );
    assert_eq!(
        Codex {
            executable: path,
            model: None,
            timeout: Duration::from_secs(2)
        }
        .complete("", &json!({}), &json!({}))
        .unwrap(),
        json!({})
    );
}
#[test]
fn codex_failures_missing_json_and_nonobjects_are_errors() {
    for body in [
        "exit 1",
        "cat >/dev/null",
        "cat >/dev/null; printf nope > result.json",
        "cat >/dev/null; printf '[]' > result.json",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = script(&tmp, body);
        assert!(
            Codex {
                executable: path,
                model: None,
                timeout: Duration::from_secs(2)
            }
            .complete("", &json!({}), &json!({}))
            .is_err()
        );
    }
}
#[test]
fn codex_timeout_includes_blocked_stdin_and_cleans_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let captured = tmp.path().join("cwd");
    let path = script(&tmp, &format!("pwd > '{}'\nsleep 20", captured.display()));
    let error = Codex {
        executable: path,
        model: None,
        timeout: Duration::from_secs(2),
    }
    .complete("", &json!({"body":"x".repeat(1024*1024)}), &json!({}))
    .unwrap_err();
    assert!(error.to_string().contains("초과"));
    let cwd = std::fs::read_to_string(captured).unwrap();
    assert!(!std::path::Path::new(cwd.trim()).exists());
}
#[test]
fn responses_payload_and_reasoning_contract() {
    let client = OpenAI::new("test-key".into(), "test-model".into()).unwrap();
    let payload = client.payload(
        "instructions",
        &json!({"facts":"text"}),
        &object_schema(json!({"value":{"type":"string"}})),
    );
    assert_eq!(payload["store"], false);
    assert_eq!(payload["text"]["format"]["strict"], true);
    assert_eq!(payload["model"], "test-model");
    let response = json!({"status":"completed","output":[{"type":"reasoning","summary":[]},{"type":"message","content":[{"type":"output_text","text":"{\"value\":\"ok\"}"}]}]});
    assert_eq!(response_object(&response).unwrap(), json!({"value":"ok"}));
}
#[test]
fn responses_refusals_incomplete_invalid_json_and_credentials_fail() {
    for response in [
        json!({"status":"incomplete"}),
        json!({"status":"completed","output":[{"type":"message","content":[{"type":"refusal"}]}]}),
        json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"[]"}]}]}),
    ] {
        assert!(response_object(&response).is_err());
    }
    assert!(OpenAI::new("".into(), "m".into()).is_err());
    assert!(OpenAI::new("k".into(), "".into()).is_err());
}
