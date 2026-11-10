"""Parse email while keeping conversation text separate from authored text."""

from dataclasses import dataclass
from email import policy
from email.parser import BytesParser
from email.utils import getaddresses
from hashlib import sha256
from html.parser import HTMLParser
from pathlib import Path
import re


def addresses(values):
    return list(dict.fromkeys(address.strip().lower() for _, address in
                             getaddresses(values) if address.strip()))


class HTMLText(HTMLParser):
    def __init__(self, strip_quotes=False):
        super().__init__(convert_charrefs=True)
        self.strip_quotes = strip_quotes
        self.parts = []
        self.stack = []
        self.cut = False

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        quoted = tag == "blockquote" or bool(
            {"gmail_quote", "yahoo_quoted"} & set(attrs.get("class", "").split())
        )
        # Outlook marks the start of the entire prior conversation with this ID.
        if self.strip_quotes and attrs.get("id", "").lower() == "divrplyfwdmsg":
            self.cut = True
        hidden = self.cut or tag in {"script", "style", "head"} or (
            self.strip_quotes and quoted
        ) or (self.stack and self.stack[-1][1])
        if tag not in {"br", "hr", "img", "meta", "link", "input", "wbr", "area",
                       "base", "col", "embed", "param", "source", "track"}:
            self.stack.append((tag, bool(hidden)))
        if not hidden and tag in {"br", "hr", "div", "p", "li", "tr"}:
            self.parts.append("\n")

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)
        self.handle_endtag(tag)

    def handle_endtag(self, tag):
        for index in range(len(self.stack) - 1, -1, -1):
            if self.stack[index][0] == tag:
                del self.stack[index:]
                break
        if not self.cut and not (self.stack and self.stack[-1][1]):
            if tag in {"div", "p", "li", "tr", "blockquote"}:
                self.parts.append("\n")

    def handle_data(self, data):
        if not self.cut and not (self.stack and self.stack[-1][1]):
            self.parts.append(data)


def html_text(content, strip_quotes=False):
    parser = HTMLText(strip_quotes)
    parser.feed(content)
    return "".join(parser.parts)


REPLY_MARKER = re.compile(
    r"^\s*(?:On\s+.+wrote\s*:|.+(?:님이|님께서)\s*(?:작성|쓴 글).*:|"
    r"-{2,}\s*(?:Original Message|Forwarded message|원본 메시지|전달된 메시지)\s*-*|"
    r"Begin forwarded message\s*:)", re.IGNORECASE
)
HEADER = re.compile(r"^\s*(?:From|Sent|Date|To|Subject|보낸 사람|보낸 날짜|받는 사람|제목)\s*:",
                    re.IGNORECASE)


def authored_text(body):
    lines = body.replace("\r\n", "\n").replace("\r", "\n").splitlines()
    result = []
    for index, line in enumerate(lines):
        # Reply introductions can wrap across several lines.
        if REPLY_MARKER.match(line) or (
            re.match(r"^\s*On\s", line) and
            REPLY_MARKER.match(" ".join(lines[index:index + 3]))
        ):
            break
        if HEADER.match(line) and sum(bool(HEADER.match(item))
                                     for item in lines[index:index + 6]) >= 3:
            break
        if not line.lstrip().startswith(">"):
            result.append(line)
    return "\n".join(result).strip()


@dataclass(frozen=True)
class Mail:
    fingerprint: str
    message_id: str
    sender: str
    to: list[str]
    cc: list[str]
    subject: str
    date: str
    body: str
    authored_body: str

    def conversation(self):
        return {"sender": self.sender, "to": self.to, "cc": self.cc,
                "subject": self.subject, "date": self.date, "body": self.body}


def parse_mail(source):
    raw = source if isinstance(source, bytes) else Path(source).read_bytes()
    label = "세션 입력" if isinstance(source, bytes) else source
    message = BytesParser(policy=policy.default).parsebytes(raw)
    senders = addresses(message.get_all("From", []))
    if len(senders) != 1:
        raise ValueError(f"{label}: From 헤더에는 발신자가 한 명 있어야 합니다.")
    part = message.get_body(preferencelist=("plain", "html"))
    if part is None:
        raise ValueError(f"{label}: 읽을 수 있는 텍스트 본문이 없습니다.")
    try:
        content = part.get_content()
    except LookupError:
        content = part.get_payload(decode=True).decode("utf-8", errors="replace")
    if part.get_content_type() == "text/html":
        body = html_text(content)
        own = authored_text(html_text(content, strip_quotes=True))
    else:
        body = content
        own = authored_text(content)
    return Mail(sha256(raw).hexdigest(), str(message.get("Message-ID", "")),
                senders[0], addresses(message.get_all("To", [])),
                addresses(message.get_all("Cc", [])), str(message.get("Subject", "")),
                str(message.get("Date", "")), body.strip(), own)
