use super::{ACCOUNT, Settings};
use crate::{mail::read_limited, work::Work};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{StatusCode, blocking::Client as HttpClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener},
    path::Path,
    time::{Duration, Instant},
};

pub const SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const API_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

#[derive(Serialize, Deserialize)]
struct Tokens {
    client_id: String,
    account: String,
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    scope: String,
}

enum TokenStore {
    #[cfg(target_os = "macos")]
    Keychain,
    #[cfg(any(not(target_os = "macos"), test))]
    File(std::path::PathBuf),
}
impl TokenStore {
    fn load(&self) -> Result<Tokens> {
        let bytes = match self {
            #[cfg(target_os = "macos")]
            Self::Keychain => security_framework::passwords::generic_password(
                security_framework::passwords::PasswordOptions::new_generic_password(
                    "mimicmail.gmail",
                    ACCOUNT,
                ),
            )
            .map_err(|_| anyhow!("Gmail 연결을 확인하고 gmail connect를 실행하세요."))?,
            #[cfg(any(not(target_os = "macos"), test))]
            Self::File(path) => {
                let file = std::fs::File::open(path)
                    .map_err(|_| anyhow!("gmail connect를 실행하세요."))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if file.metadata()?.permissions().mode() & 0o077 != 0 {
                        bail!("Gmail 토큰 파일의 권한을 600으로 설정하세요.");
                    }
                }
                read_limited(file, 64 * 1024)?
            }
        };
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("gmail connect로 다시 연결하세요."))
    }
    fn save(&self, tokens: &Tokens) -> Result<()> {
        let bytes = serde_json::to_vec(tokens)?;
        match self {
            #[cfg(target_os = "macos")]
            Self::Keychain => security_framework::passwords::set_generic_password(
                "mimicmail.gmail",
                ACCOUNT,
                &bytes,
            )
            .map_err(|_| anyhow!("키체인 접근 권한을 확인한 뒤 gmail connect를 실행하세요."))?,
            #[cfg(any(not(target_os = "macos"), test))]
            Self::File(path) => {
                let directory = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                std::fs::create_dir_all(directory)?;
                // NamedTempFile creates the file with mode 0600 on Unix. Rename is atomic.
                let mut file = tempfile::NamedTempFile::new_in(directory)?;
                file.write_all(&bytes)?;
                file.as_file().sync_all()?;
                file.persist(path)
                    .map_err(|_| anyhow!("Gmail 토큰을 저장할 경로의 권한을 확인하세요."))?;
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct HttpFailure(pub u16);
impl std::fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Gmail API HTTP {}: 연결·API 권한·사용 한도를 확인하세요.",
            self.0
        )
    }
}
impl std::error::Error for HttpFailure {}

pub(super) struct Client {
    http: HttpClient,
    settings: Settings,
    tokens: Option<Tokens>,
    store: TokenStore,
    api_url: String,
    token_url: String,
    work: Work,
    persist_refresh: bool,
}
impl Client {
    pub(super) fn new(
        settings: &Settings,
        db_path: &Path,
        work: &Work,
        connected: bool,
    ) -> Result<Self> {
        settings.validate()?;
        #[cfg(target_os = "macos")]
        let store = {
            let _ = db_path;
            TokenStore::Keychain
        };
        #[cfg(not(target_os = "macos"))]
        let store = TokenStore::File(db_path.with_extension("gmail-token.json"));
        let tokens = if connected { Some(store.load()?) } else { None };
        if let Some(tokens) = &tokens
            && (tokens.client_id != settings.client_id
                || tokens.account != ACCOUNT
                || tokens.scope != SCOPE)
        {
            bail!("설정한 전용 계정과 읽기 전용 권한으로 gmail connect를 다시 실행하세요.");
        }
        Ok(Self {
            http: HttpClient::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            settings: settings.clone(),
            tokens,
            store,
            api_url: API_URL.to_owned(),
            token_url: TOKEN_URL.to_owned(),
            work: work.clone(),
            persist_refresh: connected,
        })
    }
    fn request(
        &self,
        build: impl Fn() -> reqwest::blocking::RequestBuilder,
        limit: u64,
    ) -> Result<Value> {
        for attempt in 0..4 {
            self.work.check()?;
            let response = build().send();
            let retry = match &response {
                Ok(r) => {
                    r.status() == StatusCode::TOO_MANY_REQUESTS || r.status().is_server_error()
                }
                Err(_) => true,
            };
            if retry && attempt < 3 {
                let seconds = response
                    .as_ref()
                    .ok()
                    .and_then(|r| r.headers().get("Retry-After"))
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1 << attempt)
                    .min(60);
                wait(&self.work, Duration::from_secs(seconds))?;
                continue;
            }
            let response = response.map_err(|_| anyhow!("Gmail API 연결을 확인하세요."))?;
            if !response.status().is_success() {
                // Never print response bodies, URLs, or OAuth form values.
                return Err(HttpFailure(response.status().as_u16()).into());
            }
            let raw = read_limited(response, limit)?;
            return serde_json::from_slice(&raw)
                .map_err(|_| anyhow!("Gmail API 응답 형식을 확인하세요."));
        }
        unreachable!()
    }
    fn exchange(&mut self, grant: &[(&str, &str)]) -> Result<()> {
        let mut form = vec![
            ("client_id", self.settings.client_id.as_str()),
            ("client_secret", self.settings.client_secret.as_str()),
        ];
        form.extend_from_slice(grant);
        let result = self
            .request(|| self.http.post(&self.token_url).form(&form), 64 * 1024)
            .map_err(|_| anyhow!("OAuth 설정을 확인하고 gmail connect로 다시 연결하세요."))?;
        let prior = self.tokens.as_ref();
        let scope = result["scope"]
            .as_str()
            .or_else(|| prior.map(|t| t.scope.as_str()))
            .unwrap_or("");
        if scope != SCOPE {
            bail!("Google 계정에서 기존 앱 권한을 해제하고 gmail.readonly 권한만 승인하세요.");
        }
        let refresh = result["refresh_token"]
            .as_str()
            .or_else(|| prior.map(|t| t.refresh_token.as_str()))
            .unwrap_or("");
        let access = result["access_token"].as_str().unwrap_or("");
        let expires = result["expires_in"].as_i64().unwrap_or(0);
        if refresh.is_empty()
            || access.is_empty()
            || expires <= 0
            || result["token_type"]
                .as_str()
                .is_none_or(|s| !s.eq_ignore_ascii_case("Bearer"))
        {
            bail!("Google 앱 권한을 해제한 뒤 gmail connect로 다시 연결하세요.");
        }
        self.tokens = Some(Tokens {
            client_id: self.settings.client_id.clone(),
            account: ACCOUNT.to_owned(),
            access_token: access.to_owned(),
            refresh_token: refresh.to_owned(),
            expires_at: chrono::Utc::now().timestamp() + expires,
            scope: scope.to_owned(),
        });
        Ok(())
    }
    fn refresh(&mut self) -> Result<()> {
        let refresh = self
            .tokens
            .as_ref()
            .context("gmail connect를 실행하세요.")?
            .refresh_token
            .clone();
        self.exchange(&[("grant_type", "refresh_token"), ("refresh_token", &refresh)])?;
        if self.persist_refresh {
            self.store.save(self.tokens.as_ref().unwrap())?;
        }
        Ok(())
    }
    pub(super) fn get(&mut self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        if self
            .tokens
            .as_ref()
            .is_none_or(|t| t.expires_at <= chrono::Utc::now().timestamp() + 60)
        {
            self.refresh()?;
        }
        for attempt in 0..2 {
            let token = &self.tokens.as_ref().unwrap().access_token;
            let url = format!("{}{path}", self.api_url);
            let result = self.request(
                || self.http.get(&url).bearer_auth(token).query(query),
                crate::mail::MAX_MAIL_BYTES * 4 / 3 + 1024 * 1024,
            );
            if attempt == 0
                && result
                    .as_ref()
                    .err()
                    .and_then(|e| e.downcast_ref::<HttpFailure>())
                    .is_some_and(|e| e.0 == 401)
            {
                self.refresh()?;
            } else {
                return result;
            }
        }
        unreachable!()
    }
    pub(super) fn profile(&mut self) -> Result<Value> {
        let profile = self.get("/profile", &[])?;
        if profile["emailAddress"]
            .as_str()
            .is_none_or(|email| !email.eq_ignore_ascii_case(ACCOUNT))
        {
            bail!("{ACCOUNT} 계정으로 gmail connect를 다시 실행하세요.");
        }
        if profile["historyId"].as_str().is_none_or(str::is_empty) {
            bail!("Gmail 동기화 위치를 확인하지 못했습니다. 다시 실행하세요.");
        }
        Ok(profile)
    }

    #[cfg(test)]
    pub(super) fn mock(settings: &Settings, root: &str, token_path: &Path) -> Self {
        Self {
            http: HttpClient::new(),
            settings: settings.clone(),
            tokens: Some(Tokens {
                client_id: settings.client_id.clone(),
                account: ACCOUNT.to_owned(),
                access_token: "test-access".into(),
                refresh_token: "test-refresh".into(),
                expires_at: chrono::Utc::now().timestamp() + 3600,
                scope: SCOPE.into(),
            }),
            store: TokenStore::File(token_path.into()),
            api_url: root.into(),
            token_url: format!("{root}/token"),
            work: Work::default(),
            persist_refresh: true,
        }
    }
}

fn random() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| anyhow!("OAuth 난수를 생성하지 못했습니다."))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn authorization_url(
    client_id: &str,
    redirect: &str,
    state: &str,
    verifier: &str,
) -> Result<reqwest::Url> {
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = reqwest::Url::parse(AUTH_URL)?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("login_hint", ACCOUNT),
        ("state", state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("include_granted_scopes", "false"),
    ]);
    Ok(url)
}
fn callback(request: &str, redirect: &str, state: &str) -> Result<String> {
    let target = request
        .lines()
        .next()
        .and_then(|s| s.strip_prefix("GET "))
        .and_then(|s| s.strip_suffix(" HTTP/1.1"))
        .filter(|s| s.starts_with("/oauth/callback?"))
        .context("OAuth 콜백 경로를 확인하세요.")?;
    let url = reqwest::Url::parse(redirect)?.join(target)?;
    let pairs: Vec<_> = url.query_pairs().collect();
    if pairs.iter().filter(|(k, _)| k == "state").count() != 1
        || !pairs.iter().any(|(k, v)| k == "state" && v == state)
    {
        bail!("OAuth state가 일치하지 않습니다.");
    }
    if pairs.iter().any(|(k, _)| k == "error") {
        return Err(crate::fault(
            403,
            "Google에서 Gmail 읽기 권한을 승인하세요.",
        ));
    }
    let codes: Vec<_> = pairs
        .iter()
        .filter(|(k, v)| k == "code" && !v.is_empty())
        .collect();
    if codes.len() != 1 {
        bail!("Google 인증 코드를 확인하세요.");
    }
    Ok(codes[0].1.to_string())
}
pub(super) fn wait(work: &Work, duration: Duration) -> Result<()> {
    let until = Instant::now() + duration;
    loop {
        work.check()?;
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        std::thread::park_timeout(remaining.min(Duration::from_millis(100)));
    }
}

pub fn connect(settings: &Settings, db_path: &Path, open_browser: bool) -> Result<()> {
    let work = Work::until(Instant::now() + Duration::from_secs(180));
    let mut client = Client::new(settings, db_path, &work, false)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let redirect = format!(
        "http://127.0.0.1:{}/oauth/callback",
        listener.local_addr()?.port()
    );
    let state = random()?;
    let verifier = random()?;
    let url = authorization_url(&settings.client_id, &redirect, &state, &verifier)?;
    println!("{url}");
    std::io::stdout().flush()?;
    if open_browser {
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open")
            .arg(url.as_str())
            .status();
        #[cfg(target_os = "linux")]
        let _ = std::process::Command::new("xdg-open")
            .arg(url.as_str())
            .status();
    }
    loop {
        work.check()
            .context("인증 시간이 초과되었습니다. gmail connect를 다시 실행하세요.")?;
        let (mut stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait(&work, Duration::from_millis(50))?;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        let mut request = Vec::new();
        let mut byte = [0];
        while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => request.push(byte[0]),
                _ => break,
            }
        }
        let code = callback(&String::from_utf8_lossy(&request), &redirect, &state);
        let code = match code {
            Ok(code) => code,
            Err(error) => {
                let _ = stream.write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                if error.downcast_ref::<crate::Fault>().is_some() {
                    return Err(error);
                }
                continue;
            }
        };
        let result = (|| -> Result<()> {
            client.exchange(&[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", &redirect),
                ("code_verifier", &verifier),
            ])?;
            client.profile()?;
            client.store.save(client.tokens.as_ref().unwrap())
        })();
        let body = if result.is_ok() {
            "연결 완료. 이 창을 닫으세요."
        } else {
            "연결 실패. 터미널 오류를 확인하세요."
        };
        let status = if result.is_ok() {
            "200 OK"
        } else {
            "400 Bad Request"
        };
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        result?;
        println!("{ACCOUNT} · 읽기 전용 연결 완료");
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oauth_uses_pkce_account_hint_and_only_readonly_scope() {
        let url = authorization_url(
            "client",
            "http://127.0.0.1:1234/oauth/callback",
            "state",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        )
        .unwrap();
        let p: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            p["code_challenge"],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(p["scope"], SCOPE);
        assert_eq!(p["login_hint"], ACCOUNT);
        assert_eq!(p["include_granted_scopes"], "false");
        let redirect = "http://127.0.0.1:1234/oauth/callback";
        assert_eq!(
            callback(
                "GET /oauth/callback?code=a%2Bb&state=good HTTP/1.1\r\n",
                redirect,
                "good"
            )
            .unwrap(),
            "a+b"
        );
        for target in [
            "/oauth/callback?code=secret&state=bad",
            "/oauth/callback?code=secret&state=good&state=bad",
            "/other?code=secret&state=good",
            "/oauth/callback?error=access_denied&state=good",
        ] {
            assert!(callback(&format!("GET {target} HTTP/1.1"), redirect, "good").is_err());
        }
    }
}
