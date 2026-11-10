use crate::{Fault, db, gui, pipeline};
use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Request, Response, StatusCode, body::Incoming, server::conn::http1, service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinSet};

const MAX_CONNECTIONS: usize = 16;
const MAX_DB_JOBS: usize = 2;
const MAX_BODY: usize = 1_000_000;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
struct State {
    path: PathBuf,
    token: String,
    port: u16,
    jobs: Arc<Semaphore>,
}
fn response(status: u16, body: impl Into<Bytes>, content_type: &str) -> Response<Full<Bytes>> {
    Response::builder().status(StatusCode::from_u16(status).unwrap()).header("Content-Type",content_type)
        .header("Cache-Control","no-store").header("X-Content-Type-Options","nosniff")
        .header("Content-Security-Policy","default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'")
        .body(Full::new(body.into())).unwrap()
}
fn error(status: u16, message: &str) -> Response<Full<Bytes>> {
    response(
        status,
        json!({"error":message}).to_string(),
        "application/json; charset=utf-8",
    )
}
async fn handle(
    request: Request<Incoming>,
    state: Arc<State>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let host = request
        .headers()
        .get("Host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let hosts = [
        format!("127.0.0.1:{}", state.port),
        format!("localhost:{}", state.port),
    ];
    if !hosts.iter().any(|h| h == host) {
        return Ok(error(403, "로컬 주소로 접속하세요."));
    }
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    if method == "GET" {
        let asset: Option<(&'static [u8], &str)> = match path.as_str() {
            "/" => Some((
                include_bytes!("../assets/index.html"),
                "text/html; charset=utf-8",
            )),
            "/app.js" => Some((
                include_bytes!("../assets/app.js"),
                "text/javascript; charset=utf-8",
            )),
            "/label-colors.mjs" => Some((
                include_bytes!("../assets/label-colors.mjs"),
                "text/javascript; charset=utf-8",
            )),
            "/app.css" => Some((
                include_bytes!("../assets/app.css"),
                "text/css; charset=utf-8",
            )),
            _ => None,
        };
        if let Some((body, mime)) = asset {
            return Ok(response(200, Bytes::from_static(body), mime));
        }
    } else if !matches!(method.as_str(), "POST" | "PUT" | "DELETE") {
        return Ok(error(405, "지원하지 않는 요청입니다."));
    }
    let mut payload = Value::Null;
    let url = reqwest::Url::parse(&format!("http://localhost{}", request.uri())).ok();
    let query: std::collections::HashMap<String, String> = url
        .map(|u| {
            u.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    if method != "GET" {
        let token = request
            .headers()
            .get("X-Mimicmail-Token")
            .and_then(|v| v.to_str().ok());
        let origin = request.headers().get("Origin");
        if token != Some(state.token.as_str())
            || origin.is_some_and(|o| {
                !hosts
                    .iter()
                    .any(|h| o.to_str().ok() == Some(format!("http://{h}").as_str()))
            })
        {
            return Ok(error(403, "보관함 화면에서 다시 시도하세요."));
        }
        if request
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(';').next())
            != Some("application/json")
        {
            return Ok(error(400, "JSON 형식으로 저장하세요."));
        }
        if request
            .headers()
            .get("Content-Length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok())
            .is_some_and(|n| n == 0 || n > MAX_BODY)
        {
            return Ok(error(400, "요청 크기가 올바르지 않습니다."));
        }
        let body = match Limited::new(request.into_body(), MAX_BODY).collect().await {
            Ok(body) => body.to_bytes(),
            Err(_) => return Ok(error(400, "요청 크기가 올바르지 않습니다.")),
        };
        payload = match serde_json::from_slice::<Value>(&body) {
            Ok(value) if value.is_object() => value,
            _ => return Ok(error(400, "저장 요청 형식이 올바르지 않습니다.")),
        };
    }
    let permit = match state.jobs.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return Ok(error(503, "요청 처리 중입니다. 잠시 후 다시 시도하세요.")),
    };
    let result = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let _permit = permit;
        let db = db::open(&state.path, false)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        db.progress_handler(1000, Some(move || Instant::now() > deadline));
        let arg = |key| query.get(key).map(String::as_str).unwrap_or("");
        let id = path
            .strip_prefix("/api/mail/")
            .and_then(|s| s.parse::<i64>().ok());
        let result = match (method.as_str(), path.as_str(), id) {
            ("GET", "/api/conversations", _) => {
                let mut result = gui::conversations_filtered(
                    &db,
                    arg("q"),
                    arg("replies") == "1",
                    arg("label"),
                    arg("unlabeled") == "1",
                )?;
                result["token"] = json!(state.token);
                result["purposes"] = gui::labels(&pipeline::PURPOSES, &gui::PURPOSE_LABELS);
                result["relationships"] =
                    gui::labels(&pipeline::RELATIONSHIPS, &gui::RELATIONSHIP_LABELS);
                result
            }
            ("GET", "/api/conversation", _) => {
                let mut result = gui::conversation(&db, arg("owner"), arg("recipient"))?;
                result["search_matches"] = json!(gui::search_matches(
                    crate::array(&result, "timeline"),
                    arg("q")
                ));
                if arg("compact") == "1" {
                    result["messages"] = crate::array(&result, "messages")
                        .iter()
                        .map(|m| crate::select(m, &["id", "subject", "revision", "deleted"]))
                        .collect();
                }
                result["prompt_reference"] =
                    gui::prompt_reference(&state.path, arg("owner"), arg("recipient"))?;
                result
            }
            ("GET", _, Some(id)) => gui::get_mail(&db, id)?,
            ("POST", "/api/conversation", _) => gui::create_conversation(&db, &payload)?,
            ("POST", "/api/reply", _) => gui::create_reply(&db, &payload)?,
            ("PUT", "/api/contact-label", _) => gui::update_contact_label(&db, &payload)?,
            ("PUT", _, Some(id)) => gui::update_mail(&db, id, &payload)?,
            ("DELETE", "/api/message", _) => gui::delete_message(&db, &payload)?,
            _ => return Err(crate::fault(404, "페이지를 찾을 수 없습니다.")),
        };
        Ok(serde_json::to_vec(&result)?)
    })
    .await;
    Ok(match result {
        Ok(Ok(body)) => response(200, body, "application/json; charset=utf-8"),
        Ok(Err(e)) => {
            if let Some(Fault(status, message)) = e.downcast_ref::<Fault>() {
                error(*status, message)
            } else if e.downcast_ref::<rusqlite::Error>().is_some() {
                error(
                    503,
                    "보관함을 처리하지 못했습니다. 잠시 후 다시 시도하세요.",
                )
            } else {
                error(400, &e.to_string())
            }
        }
        Err(_) => error(500, "요청을 처리하지 못했습니다. 다시 시도하세요."),
    })
}
pub fn serve(path: &Path, port: u16, open_browser: bool) -> Result<()> {
    drop(db::connect(path)?);
    let path = std::path::absolute(path)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(MAX_DB_JOBS)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener=TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,port)).await?;
        let port=listener.local_addr()?.port();
        let mut random=[0u8;32]; getrandom::fill(&mut random).map_err(|e|anyhow::anyhow!("난수 생성 실패: {e}"))?;
        let token=random.iter().map(|b|format!("{b:02x}")).collect();
        let state=Arc::new(State {path,token,port,jobs:Arc::new(Semaphore::new(MAX_DB_JOBS))});
        let url=format!("http://127.0.0.1:{port}");
        println!("Mimicmail · {url}\n종료: Ctrl+C"); std::io::stdout().flush()?;
        if open_browser {
            #[cfg(target_os="macos")]
            let _=std::process::Command::new("open").arg(&url).status();
            #[cfg(target_os="linux")]
            let _=std::process::Command::new("xdg-open").arg(&url).status();
        }
        let connections=Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let mut tasks=JoinSet::new();
        #[cfg(unix)]
        let mut terminate=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        loop {
            tokio::select! {
                _=tokio::signal::ctrl_c()=>break,
                _=async { #[cfg(unix)] { terminate.recv().await; } #[cfg(not(unix))] { std::future::pending::<()>().await; } }=>break,
                Some(_)=tasks.join_next(), if !tasks.is_empty()=>{},
                accepted=listener.accept()=>{
                    let (stream,_)=accepted?;
                    let Ok(permit)=connections.clone().try_acquire_owned() else { drop(stream); continue; };
                    let state=state.clone();
                    tasks.spawn(async move {
                        let _permit=permit;
                        let service=service_fn(move |request| handle(request,state.clone()));
                        let mut builder=http1::Builder::new();
                        builder.timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(5)).max_buf_size(32768).keep_alive(false);
                        let _=tokio::time::timeout(CONNECTION_TIMEOUT,builder.serve_connection(TokioIo::new(stream),service)).await;
                    });
                }
            }
        }
        drop(listener);
        while tasks.join_next().await.is_some() {}
        Ok::<(),anyhow::Error>(())
    })
}
