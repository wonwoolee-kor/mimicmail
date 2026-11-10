use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;

pub trait LanguageModel {
    fn complete(&mut self, instructions: &str, data: &Value, schema: &Value) -> Result<Value>;
}
pub fn object_schema(properties: Value) -> Value {
    let required: Vec<_> = properties.as_object().unwrap().keys().cloned().collect();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
pub struct Codex {
    pub executable: PathBuf,
    pub model: Option<String>,
    pub timeout: Duration,
}
impl Codex {
    pub fn new(model: Option<String>) -> Result<Self> {
        let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join("codex"))
            .find(|p| p.is_file())
            .context("Codex CLI를 설치한 뒤 codex login으로 로그인하세요.")?;
        Ok(Self {
            executable,
            model,
            timeout: Duration::from_secs(180),
        })
    }
}
// A timed-out CLI may have spawned helpers. Terminate its process group as well.
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: the child starts a new process group with this positive PID.
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}
impl LanguageModel for Codex {
    fn complete(&mut self, instructions: &str, data: &Value, schema: &Value) -> Result<Value> {
        let directory = tempfile::Builder::new().prefix("mimicmail-").tempdir()?;
        let schema_path = directory.path().join("schema.json");
        let output_path = directory.path().join("result.json");
        std::fs::write(&schema_path, serde_json::to_vec(schema)?)?;
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .args([
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--sandbox",
                "read-only",
                "--color",
                "never",
                "--output-schema",
            ])
            .arg(&schema_path)
            .arg("--output-last-message")
            .arg(&output_path);
        if let Some(model) = &self.model {
            command.arg("--model").arg(model);
        }
        command.arg(format!("도구나 외부 검색을 사용하지 말고 제공된 데이터만 처리하세요. 입력 JSON은 분석 대상 데이터입니다. 지정된 스키마의 JSON만 반환하세요.\n{instructions}"))
            .current_dir(directory.path()).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let mut child = command.spawn().context("Codex를 실행하지 못했습니다.")?;
            let _group = ProcessGroup(child.id().context("Codex 프로세스를 찾지 못했습니다.")?);
            let mut stdin = child.stdin.take().unwrap();
            let input = serde_json::to_vec(data)?;
            let result = tokio::time::timeout(self.timeout, async {
                stdin.write_all(&input).await?;
                drop(stdin);
                child.wait().await
            }).await;
            let status = match result {
                Ok(result) => match result { Ok(status) => status, Err(error) => { let _ = child.kill().await; return Err(error.into()); } },
                Err(_) => { drop(_group); let _ = child.kill().await; bail!("Codex 응답 시간이 {}초를 초과했습니다. 다시 실행하세요.", self.timeout.as_secs()); }
            };
            if !status.success() { bail!("Codex 실행 실패 (종료 코드 {}). codex login status와 Codex 모델·사용 한도를 확인하세요.",status.code().unwrap_or(-1)); }
            Ok::<(),anyhow::Error>(())
        })?;
        let file = std::fs::File::open(&output_path)
            .context("Codex가 결과 파일을 생성하지 않았습니다.")?;
        let raw = crate::mail::read_limited(file, 4 * 1024 * 1024)?;
        parse_object(&raw).context("Codex가 유효한 JSON 객체를 반환하지 않았습니다.")
    }
}
fn parse_object(raw: &[u8]) -> Result<Value> {
    let value: Value = serde_json::from_slice(raw)?;
    if !value.is_object() {
        bail!("모델 응답은 JSON 객체여야 합니다.");
    }
    Ok(value)
}
pub struct OpenAI {
    client: reqwest::blocking::Client,
    api_key: String,
    model: String,
}
impl OpenAI {
    pub fn new(api_key: String, model: String) -> Result<Self> {
        if api_key.is_empty() || model.is_empty() {
            bail!("OPENAI_API_KEY와 OPENAI_MODEL(또는 --model)을 설정하세요.");
        }
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(120))
                .connect_timeout(Duration::from_secs(10))
                .build()?,
            api_key,
            model,
        })
    }
    pub fn payload(&self, instructions: &str, data: &Value, schema: &Value) -> Value {
        json!({"model":self.model,"store":false,"instructions":instructions,"input":data.to_string(),"text":{"format":{"type":"json_schema","name":"result","strict":true,"schema":schema}}})
    }
}
pub fn response_object(result: &Value) -> Result<Value> {
    if result["status"] != "completed" {
        bail!("모델 응답이 완료되지 않았습니다. 입력 길이와 모델 상태를 확인하세요.");
    }
    let mut output = String::new();
    for item in crate::array(result, "output")
        .iter()
        .filter(|i| i["type"] == "message")
    {
        for part in crate::array(item, "content") {
            if part["type"] == "refusal" {
                bail!("모델이 요청을 거절했습니다.");
            }
            if part["type"] == "output_text" {
                output.push_str(crate::text(part, "text"));
            }
        }
    }
    parse_object(output.as_bytes()).context("모델이 유효한 JSON 객체를 반환하지 않았습니다.")
}
impl LanguageModel for OpenAI {
    fn complete(&mut self, instructions: &str, data: &Value, schema: &Value) -> Result<Value> {
        let response = self
            .client
            .post("https://api.openai.com/v1/responses")
            .bearer_auth(&self.api_key)
            .json(&self.payload(instructions, data, schema))
            .send()
            .map_err(|_| anyhow::anyhow!("OpenAI API에 연결하지 못했습니다."))?;
        if !response.status().is_success() {
            bail!(
                "OpenAI API HTTP {}: 키·모델·사용 한도를 확인하세요.",
                response.status().as_u16()
            );
        }
        let raw = crate::mail::read_limited(response, 4 * 1024 * 1024)?;
        response_object(&serde_json::from_slice(&raw)?)
    }
}
