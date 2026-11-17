//! One read-only Gmail account, durable collection, and retryable analysis.
mod auth;
pub use auth::connect;

use crate::{db, llm::LanguageModel, mail::Mail, pipeline, privacy, text, work::Work};
use anyhow::{Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use mailparse::MailHeaderMap;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs::OpenOptions,
    path::{Path, PathBuf},
    time::Duration,
};

pub const ACCOUNT: &str = "wonwoolee.fwd@gmail.com";

#[derive(Clone)]
pub struct Settings {
    pub client_id: String,
    pub client_secret: String,
    pub owner: String,
    pub aliases: Vec<String>,
    pub interval: Duration,
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.client_id.trim().is_empty() || self.client_secret.trim().is_empty() {
            bail!("GMAIL_CLIENT_ID와 GMAIL_CLIENT_SECRET을 설정하세요.");
        }
        if self.owner.trim().is_empty() || self.interval < Duration::from_secs(30) {
            bail!("MIMICMAIL_USER와 GMAIL_POLL_SECONDS(30 이상)를 확인하세요.");
        }
        Ok(())
    }
    fn own_addresses(&self) -> HashSet<String> {
        self.aliases
            .iter()
            .map(String::as_str)
            .chain([self.owner.as_str(), ACCOUNT])
            .map(|s| s.trim().to_lowercase())
            .collect()
    }
}

/// The OS releases this lock on normal exit or process failure. HTTP/LLM work
/// never holds a SQLite write transaction, so the GUI can keep reading/editing.
fn lock(path: &Path) -> Result<std::fs::File> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path.with_extension("gmail.lock"))?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow!("이 보관함의 Gmail 동기화가 실행 중입니다."))?;
    Ok(file)
}

fn capture(
    db: &Connection,
    client: &mut auth::Client,
    settings: &Settings,
    id: &str,
) -> Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        bail!("Gmail 메시지 식별자 형식을 확인하세요.");
    }
    let account = privacy::person_id(ACCOUNT);
    let owner = privacy::person_id(&settings.owner);
    if db::exists(
        db,
        "SELECT 1 FROM gmail_messages WHERE account=? AND gmail_id=? AND status NOT IN ('excluded','failed')",
        params![account, id],
    )? {
        return Ok(());
    }
    let remote = match client.get(&format!("/messages/{id}"), &[("format", "raw")]) {
        Ok(value) => value,
        Err(error)
            if error
                .downcast_ref::<auth::HttpFailure>()
                .is_some_and(|e| e.0 == 404) =>
        {
            db.execute("INSERT OR REPLACE INTO gmail_messages(account,gmail_id,owner,status) VALUES (?,?,?,'gone')", params![account,id,owner])?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if remote["id"] != id {
        bail!("Gmail 메시지 응답 식별자를 확인하세요.");
    }
    if crate::array(&remote, "labelIds")
        .iter()
        .any(|label| ["SPAM", "TRASH", "DRAFT"].iter().any(|s| label == s))
    {
        db.execute("INSERT OR REPLACE INTO gmail_messages(account,gmail_id,owner,status) VALUES (?,?,?,'excluded')", params![account,id,owner])?;
        return Ok(());
    }
    let prepared = prepare(&remote, settings);
    let tx = db::immediate(db)?;
    let db = &tx;
    match prepared {
        Ok((mail, recipients, dedup_key)) => {
            // Delivery IDs are independent of logical mail identity. Reused Message-ID
            // headers alone never merge different contents.
            let prior = db::one(
                db,
                "SELECT fingerprint FROM gmail_messages WHERE owner=? AND dedup_key=? AND status='done' LIMIT 1",
                params![owner, dedup_key],
            )?;
            let mut active = false;
            for recipient in &recipients {
                active |= !crate::archive::conversation_deleted(db, &owner, recipient)?;
            }
            let deleted = (!recipients.is_empty() && !active)
                || db::exists(
                    db,
                    "SELECT 1 FROM deleted_messages WHERE owner=? AND message_key=?",
                    params![owner, format!("sent:{}", mail.fingerprint)],
                )?;
            let done = prior.is_some();
            let fingerprint = prior
                .as_ref()
                .map(|p| text(p, "fingerprint"))
                .unwrap_or(&mail.fingerprint);
            db.execute("INSERT OR REPLACE INTO gmail_messages(account,gmail_id,owner,fingerprint,dedup_key,mail_json,recipients_json,status) VALUES (?,?,?,?,?,?,?,?)",
                params![account,id,owner,fingerprint,dedup_key,(!done && !deleted).then(|| serde_json::to_string(&mail)).transpose()?,(!done && !deleted).then(|| serde_json::to_string(&recipients)).transpose()?,if deleted {"deleted"} else if done {"done"} else {"pending"}])?;
        }
        Err(_) => {
            // MIME errors can contain attacker-controlled source text; persist only a code.
            db.execute("INSERT OR REPLACE INTO gmail_messages(account,gmail_id,owner,status,error) VALUES (?,?,?,'failed','mime')", params![account,id,owner])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn prepare(remote: &Value, settings: &Settings) -> Result<(Mail, Vec<String>, String)> {
    let raw = URL_SAFE_NO_PAD.decode(text(remote, "raw").trim_end_matches('='))?;
    let mut mail = crate::mail::parse_mail(&raw)?;
    let dedup_key = if mail.message_id.is_empty() {
        mail.fingerprint.clone()
    } else {
        crate::sha256(
            serde_json::to_vec(&json!([
                mail.message_id,
                mail.sender,
                mail.to,
                mail.cc,
                mail.subject,
                mail.date,
                mail.body
            ]))?
            .as_slice(),
        )
    };
    // Gmail's internalDate is a reliable ordering fallback when Date is absent.
    if crate::context::mail_datetime(&mail.date).is_none() {
        mail.date = text(remote, "internalDate")
            .parse::<i64>()
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|d| d.to_rfc2822())
            .unwrap_or_default();
    }
    let own = settings.own_addresses();
    let incoming = !own.contains(&mail.sender);
    let recipients: Vec<String> = if incoming {
        let headers = mailparse::parse_headers(&raw)?.0;
        let name = crate::mail::address_pairs(&headers.get_first_value("From").unwrap_or_default())
            .into_iter()
            .find(|(_, address)| address == &mail.sender)
            .map(|(name, _)| name)
            .unwrap_or_default();
        mail.context["recipient_names"][&mail.sender] = json!(name);
        vec![mail.sender.clone()]
    } else {
        let mut seen = HashSet::new();
        mail.to
            .iter()
            .chain(&mail.cc)
            .filter(|r| !own.contains(*r) && seen.insert((*r).clone()))
            .cloned()
            .collect()
    };
    mail.context["incoming"] = json!(incoming);
    // Neither queued mail nor model input contains unredacted addresses.
    Ok((
        privacy::stored_mail(&mail),
        recipients.iter().map(|r| privacy::person_id(r)).collect(),
        dedup_key,
    ))
}

fn collect(
    db: &Connection,
    client: &mut auth::Client,
    settings: &Settings,
    work: &Work,
) -> Result<()> {
    // Validate the account before any list/get-message request or state update.
    let profile = client.profile()?;
    let account = privacy::person_id(ACCOUNT);
    let owner = privacy::person_id(&settings.owner);
    if let Some(state) = db::one(
        db,
        "SELECT owner FROM gmail_sync WHERE account=?",
        [&account],
    )? && state["owner"] != owner
    {
        bail!(
            "이 DB에 처음 연결한 MIMICMAIL_USER를 사용하세요. 소유자를 바꾸려면 별도 DB를 지정하세요."
        );
    }
    db.execute(
        "INSERT OR IGNORE INTO gmail_sync(account,owner) VALUES (?,?)",
        params![account, owner],
    )?;
    let cursor: String = db.query_row(
        "SELECT history_id FROM gmail_sync WHERE account=?",
        [&account],
        |r| r.get(0),
    )?;
    let mut full = cursor.is_empty();
    let mut page = String::new();
    let mut pages = HashSet::new();
    let boundary;
    loop {
        work.check()?;
        let result = if full {
            client.get(
                "/messages",
                &[
                    ("maxResults", "500"),
                    ("includeSpamTrash", "true"),
                    ("pageToken", &page),
                ],
            )
        } else {
            client.get(
                "/history",
                &[
                    ("maxResults", "500"),
                    ("startHistoryId", &cursor),
                    ("pageToken", &page),
                ],
            )
        };
        let result = match result {
            Ok(value) => value,
            Err(error)
                if !full
                    && error
                        .downcast_ref::<auth::HttpFailure>()
                        .is_some_and(|e| e.0 == 404) =>
            {
                full = true;
                page.clear();
                pages.clear();
                continue;
            }
            Err(error) => return Err(error),
        };
        if full {
            for message in crate::array(&result, "messages") {
                work.check()?;
                capture(db, client, settings, text(message, "id"))?;
            }
        } else {
            for history in crate::array(&result, "history") {
                for event in crate::array(history, "messagesAdded")
                    .iter()
                    .chain(crate::array(history, "labelsRemoved"))
                {
                    work.check()?;
                    capture(db, client, settings, text(&event["message"], "id"))?;
                }
            }
        }
        page = text(&result, "nextPageToken").to_owned();
        if page.is_empty() {
            // Full sync anchors before enumeration; the next history pass catches
            // mail delivered while the full scan was running.
            boundary = if full {
                text(&profile, "historyId")
            } else {
                text(&result, "historyId")
            }
            .to_owned();
            if boundary.is_empty() {
                bail!("Gmail 동기화 위치가 없습니다. 다시 실행하세요.");
            }
            break;
        }
        if !pages.insert(page.clone()) {
            bail!("Gmail 페이지가 반복됩니다. 다시 실행하세요.");
        }
    }
    work.check()?;
    // Every fetched page has durable jobs before the cursor advances. Analysis
    // failures below never cause a missed delivery or hold this cursor back.
    db.execute(
        "UPDATE gmail_sync SET history_id=?,synced_at=? WHERE account=?",
        params![boundary, chrono::Utc::now().to_rfc3339(), account],
    )?;
    Ok(())
}

fn analyze(db: &Connection, llm: &mut dyn LanguageModel, work: &Work) -> Result<()> {
    let account = privacy::person_id(ACCOUNT);
    let jobs = db::rows(
        db,
        "SELECT gmail_id FROM gmail_messages WHERE account=? AND status='pending' AND retry_at<=? ORDER BY rowid LIMIT 100",
        params![account, chrono::Utc::now().timestamp()],
    )?;
    for job in jobs {
        work.check()?;
        // Re-read because the GUI may have deleted this conversation since collection.
        let Some(job) = db::one(
            db,
            "SELECT * FROM gmail_messages WHERE account=? AND gmail_id=? AND status='pending'",
            params![account, text(&job, "gmail_id")],
        )?
        else {
            continue;
        };
        if let Some(prior) = db::one(
            db,
            "SELECT fingerprint FROM gmail_messages WHERE owner=? AND dedup_key=? AND status='done' LIMIT 1",
            params![text(&job, "owner"), text(&job, "dedup_key")],
        )? {
            db.execute("UPDATE gmail_messages SET status='done',fingerprint=?,mail_json=NULL,recipients_json=NULL,error=NULL WHERE account=? AND gmail_id=? AND status='pending'",params![text(&prior,"fingerprint"),account,text(&job,"gmail_id")])?;
            continue;
        }
        let mail: Mail = serde_json::from_str(text(&job, "mail_json"))?;
        let recipients: Vec<String> = serde_json::from_str(text(&job, "recipients_json"))?;
        let result = if recipients.is_empty()
            || (mail.context["incoming"] != true && mail.authored_body.trim().is_empty())
        {
            Ok(false)
        } else {
            pipeline::index_mail(
                db,
                llm,
                &mail,
                text(&job, "owner"),
                &recipients,
                "Gmail",
                work,
            )
        };
        work.check()?;
        match result {
            Ok(_) => {
                db.execute("UPDATE gmail_messages SET status='done',mail_json=NULL,recipients_json=NULL,error=NULL WHERE account=? AND gmail_id=? AND status='pending'", params![account,text(&job,"gmail_id")])?;
            }
            Err(_) => {
                let attempts = job["attempts"].as_u64().unwrap_or(0) + 1;
                let retry_at =
                    chrono::Utc::now().timestamp() + (30 * (1i64 << attempts.min(7))).min(3600);
                db.execute("UPDATE gmail_messages SET attempts=attempts+1,error='analysis',retry_at=? WHERE account=? AND gmail_id=? AND status='pending'", params![retry_at,account,text(&job,"gmail_id")])?;
            }
        }
    }
    Ok(())
}

pub fn status(db: &Connection) -> Result<Value> {
    if !db::exists(
        db,
        "SELECT 1 FROM sqlite_master WHERE name='gmail_sync'",
        [],
    )? {
        return Ok(json!({"account":ACCOUNT,"sync":null,"messages":[],"errors":[]}));
    }
    let account = privacy::person_id(ACCOUNT);
    let state = db::one(
        db,
        "SELECT owner,history_id,synced_at FROM gmail_sync WHERE account=?",
        [&account],
    )?;
    let counts = db::rows(
        db,
        "SELECT status,count(*) AS count FROM gmail_messages WHERE account=? GROUP BY status",
        [&account],
    )?;
    let errors = db::rows(
        db,
        "SELECT gmail_id,error,attempts FROM gmail_messages WHERE account=? AND error IS NOT NULL ORDER BY rowid LIMIT 20",
        [&account],
    )?;
    Ok(json!({"account":ACCOUNT,"sync":state,"messages":counts,"errors":errors}))
}

pub fn synchronize(
    path: &Path,
    settings: &Settings,
    work: &Work,
    make_model: &dyn Fn() -> Result<Box<dyn LanguageModel>>,
    retry_failed: bool,
) -> Result<Value> {
    let mut client = auth::Client::new(settings, path, work, true)?;
    let _lock = lock(path)?;
    let db = db::connect(path)?;
    collect(&db, &mut client, settings, work)?;
    if retry_failed {
        let account = privacy::person_id(ACCOUNT);
        for job in db::rows(
            &db,
            "SELECT gmail_id FROM gmail_messages WHERE account=? AND status='failed'",
            [&account],
        )? {
            work.check()?;
            capture(&db, &mut client, settings, text(&job, "gmail_id"))?;
        }
        db.execute(
            "UPDATE gmail_messages SET retry_at=0 WHERE account=? AND status='pending'",
            [account],
        )?;
    }
    if db::exists(
        &db,
        "SELECT 1 FROM gmail_messages WHERE account=? AND status='pending' AND retry_at<=?",
        params![privacy::person_id(ACCOUNT), chrono::Utc::now().timestamp()],
    )? {
        analyze(&db, make_model()?.as_mut(), work)?;
    }
    status(&db)
}

pub struct Background {
    work: Work,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Background {
    pub fn start(
        path: PathBuf,
        settings: Settings,
        make_model: impl Fn() -> Result<Box<dyn LanguageModel>> + Send + 'static,
    ) -> Result<Self> {
        settings.validate()?;
        let work = Work::default();
        let worker = work.clone();
        let thread = std::thread::Builder::new()
            .name("gmail-sync".into())
            .spawn(move || {
                while worker.check().is_ok() {
                    match synchronize(&path, &settings, &worker, &make_model, false) {
                        Ok(status) => eprintln!("Gmail: {}", status["messages"]),
                        Err(error) if worker.check().is_ok() => eprintln!("Gmail: {error}"),
                        Err(_) => break,
                    }
                    if auth::wait(&worker, settings.interval).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            work,
            thread: Some(thread),
        })
    }
}
impl Drop for Background {
    fn drop(&mut self) {
        self.work.cancel();
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
