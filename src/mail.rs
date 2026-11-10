use anyhow::{Result, bail};
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use mailparse::{MailAddr, MailHeaderMap, ParsedMail};
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::HashSet, io::Read, path::Path, sync::LazyLock};

pub const MAX_MAIL_BYTES: u64 = 64 * 1024 * 1024;
pub fn read_limited(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    reader.take(limit + 1).read_to_end(&mut raw)?;
    if raw.len() as u64 > limit {
        bail!("입력이 허용 크기({limit}바이트)를 초과했습니다.");
    }
    Ok(raw)
}
pub fn read_source(path: &Path) -> Result<Vec<u8>> {
    read_limited(std::fs::File::open(path)?, MAX_MAIL_BYTES)
}

#[derive(Default)]
struct HtmlState {
    output: String,
    stack: Vec<(String, bool)>,
    cut: bool,
}
struct HtmlText {
    strip_quotes: bool,
    state: RefCell<HtmlState>,
}
impl HtmlState {
    fn hidden(&self) -> bool {
        self.cut || self.stack.last().is_some_and(|s| s.1)
    }
    fn end(&mut self, tag: &str) {
        if let Some(index) = self.stack.iter().rposition(|s| s.0 == tag) {
            self.stack.truncate(index);
        }
        if !self.hidden() && matches!(tag, "div" | "p" | "li" | "tr" | "blockquote") {
            self.output.push('\n');
        }
    }
}
impl TokenSink for HtmlText {
    type Handle = ();
    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let mut s = self.state.borrow_mut();
        match token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag => {
                let name = tag.name.as_ref();
                let attr = |key: &str| {
                    tag.attrs
                        .iter()
                        .find(|a| a.name.local.as_ref() == key)
                        .map(|a| a.value.as_ref())
                        .unwrap_or("")
                };
                let quoted = name == "blockquote"
                    || attr("class")
                        .split_whitespace()
                        .any(|c| matches!(c, "gmail_quote" | "yahoo_quoted"));
                if self.strip_quotes && attr("id").eq_ignore_ascii_case("divrplyfwdmsg") {
                    s.cut = true;
                }
                let hidden = s.hidden()
                    || matches!(name, "head" | "script" | "style")
                    || (self.strip_quotes && quoted);
                // Keep the original parser's line boundaries for HTML fragments.
                let start_break = !s.stack.is_empty() || matches!(name, "br" | "hr");
                if !matches!(
                    name,
                    "br" | "hr"
                        | "img"
                        | "meta"
                        | "link"
                        | "input"
                        | "wbr"
                        | "area"
                        | "base"
                        | "col"
                        | "embed"
                        | "param"
                        | "source"
                        | "track"
                ) {
                    s.stack.push((name.to_owned(), hidden));
                }
                if !hidden && start_break && matches!(name, "br" | "hr" | "div" | "p" | "li" | "tr")
                {
                    s.output.push('\n');
                }
                if tag.self_closing {
                    s.end(name);
                }
                if name == "script" {
                    return TokenSinkResult::RawData(
                        html5ever::tokenizer::states::RawKind::ScriptData,
                    );
                }
                if name == "style" {
                    return TokenSinkResult::RawData(
                        html5ever::tokenizer::states::RawKind::Rawtext,
                    );
                }
            }
            Token::TagToken(tag) => s.end(tag.name.as_ref()),
            Token::CharacterTokens(data) if !s.hidden() => s.output.push_str(&data),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}
pub fn html_text(content: &str, strip_quotes: bool) -> String {
    let tokenizer = Tokenizer::new(
        HtmlText {
            strip_quotes,
            state: RefCell::new(HtmlState::default()),
        },
        TokenizerOpts::default(),
    );
    let input = BufferQueue::default();
    input.push_back(content.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.state.into_inner().output
}
static REPLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:On\s+.+wrote\s*:|.+(?:님이|님께서)\s*(?:작성|쓴 글).*:|-{2,}\s*(?:Original Message|Forwarded message|원본 메시지|전달된 메시지)\s*-*|Begin forwarded message\s*:)").unwrap()
});
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:From|Sent|Date|To|Subject|보낸 사람|보낸 날짜|받는 사람|제목)\s*:")
        .unwrap()
});
pub fn authored_text(body: &str) -> String {
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<_> = normalized.lines().collect();
    let mut result = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if REPLY.is_match(line)
            || (line.trim_start().starts_with("On ")
                && REPLY.is_match(&lines[i..(i + 3).min(lines.len())].join(" ")))
        {
            break;
        }
        if HEADER.is_match(line)
            && lines[i..(i + 6).min(lines.len())]
                .iter()
                .filter(|l| HEADER.is_match(l))
                .count()
                >= 3
        {
            break;
        }
        if !line.trim_start().starts_with('>') {
            result.push(*line);
        }
    }
    result.join("\n").trim().to_owned()
}
pub fn address_pairs(value: &str) -> Vec<(String, String)> {
    let Ok(addresses) = mailparse::addrparse(value) else {
        return vec![];
    };
    addresses
        .iter()
        .flat_map(|a| match a {
            MailAddr::Single(s) => vec![s],
            MailAddr::Group(g) => g.addrs.iter().collect(),
        })
        .map(|s| {
            (
                s.display_name.clone().unwrap_or_default(),
                s.addr.trim().to_lowercase(),
            )
        })
        .collect()
}
fn header_addresses(mail: &ParsedMail<'_>, name: &str) -> Vec<(String, String)> {
    mail.headers
        .get_all_values(name)
        .iter()
        .flat_map(|v| address_pairs(v))
        .collect()
}
fn addresses(pairs: &[(String, String)]) -> Vec<String> {
    let mut seen = HashSet::new();
    pairs
        .iter()
        .map(|(_, a)| a)
        .filter(|a| !a.is_empty() && seen.insert((*a).clone()))
        .cloned()
        .collect()
}
fn body_part<'a>(mail: &'a ParsedMail<'a>, mime: &str) -> Option<&'a ParsedMail<'a>> {
    if mail.get_content_disposition().disposition == mailparse::DispositionType::Attachment {
        return None;
    }
    if mail.ctype.mimetype == mime {
        return Some(mail);
    }
    if !mail.ctype.mimetype.starts_with("multipart/") {
        return None;
    }
    if mail.ctype.mimetype == "multipart/related" {
        let start = mail.ctype.params.get("start");
        let root = start
            .and_then(|id| {
                mail.subparts
                    .iter()
                    .find(|p| p.headers.get_first_value("Content-ID").as_ref() == Some(id))
            })
            .or_else(|| mail.subparts.first())?;
        return body_part(root, mime);
    }
    mail.subparts.iter().find_map(|p| body_part(p, mime))
}
#[derive(Debug, Serialize)]
pub struct Mail {
    pub fingerprint: String,
    pub message_id: String,
    pub sender: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub date: String,
    pub body: String,
    pub authored_body: String,
    pub context: Value,
}
impl Mail {
    pub fn conversation(&self) -> Value {
        json!({"sender":self.sender,"to":self.to,"cc":self.cc,"subject":self.subject,"date":self.date,"body":self.body})
    }
}
pub fn parse_mail(raw: &[u8]) -> Result<Mail> {
    if raw.len() as u64 > MAX_MAIL_BYTES {
        bail!("메일이 64 MiB를 초과했습니다.");
    }
    let message = mailparse::parse_mail(raw)?;
    let senders = addresses(&header_addresses(&message, "From"));
    if senders.len() != 1 {
        bail!("From 헤더에는 발신자가 한 명 있어야 합니다.");
    }
    let plain = body_part(&message, "text/plain");
    let html = body_part(&message, "text/html");
    let Some(part) = plain.or(html) else {
        bail!("읽을 수 있는 텍스트 본문이 없습니다.");
    };
    let content = part.get_body().or_else(|_| {
        part.get_body_raw()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    })?;
    let (body, own) = if part.ctype.mimetype == "text/html" {
        (
            html_text(&content, false),
            authored_text(&html_text(&content, true)),
        )
    } else {
        (content.clone(), authored_text(&content))
    };
    let to = header_addresses(&message, "To");
    let cc = header_addresses(&message, "Cc");
    let names: serde_json::Map<_, _> = to
        .iter()
        .chain(&cc)
        .filter(|(n, a)| !n.is_empty() && !a.is_empty())
        .map(|(n, a)| (a.clone(), json!(n)))
        .collect();
    let mut quotes = crate::context::quoted_messages(&body, &own)?;
    if let Some(part) = html
        && let Ok(content) = part.get_body()
    {
        let other = crate::context::quoted_messages(&html_text(&content, false), &own)?;
        if other.len() >= quotes.len() {
            quotes = other;
        }
    }
    static REFERENCES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
    let header = |name| message.headers.get_first_value(name).unwrap_or_default();
    Ok(Mail {
        fingerprint: crate::sha256(raw),
        message_id: header("Message-ID"),
        sender: senders[0].clone(),
        to: addresses(&to),
        cc: addresses(&cc),
        subject: header("Subject"),
        date: header("Date"),
        body: body.trim().to_owned(),
        authored_body: own,
        context: json!({"recipient_names":names,"in_reply_to":header("In-Reply-To"),"references":REFERENCES.find_iter(&header("References")).map(|m| m.as_str()).collect::<Vec<_>>(),"quotes":quotes}),
    })
}
