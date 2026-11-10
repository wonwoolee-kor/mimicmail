"""Keep reply evidence and quoted conversation separate from style examples."""

from email import policy
from datetime import datetime
from hashlib import sha256
from email.parser import BytesParser
from email.utils import getaddresses
from html import unescape
from pathlib import Path
import re
import unicodedata

from .mail import html_text


QUOTE_BOUNDARY = re.compile(
    r"-{2,}\s*(?:Original Message|Forwarded message|원본 메시지|전달된 메시지)\s*-+",
    re.IGNORECASE,
)


def readable(text):
    text = unicodedata.normalize("NFC", unescape(text).replace("\xa0", " "))
    return re.sub(r"\n[ \t]*\n(?:[ \t]*\n)+", "\n\n", text).strip()


def quote_timestamp(value):
    match = re.search(r"(\d{4}-\d{2}-\d{2})\s*(?:\([^)]*\))?\s*"
                      r"(\d{2}:\d{2}(?::\d{2})?)\s*\(?(?:UTC)?([+-]\d{2}:?\d{2})", value)
    if match:
        try:
            return datetime.fromisoformat(f"{match[1]}T{match[2]}{match[3]}").isoformat()
        except ValueError:
            pass
    return ""


def quoted_messages(body, authored):
    text = readable(body)
    chunks = QUOTE_BOUNDARY.split(text)[1:]
    if not chunks:
        parts = re.split(r"(?m)^\s*(?:On .+wrote:|.+님이\s*작성:|Begin forwarded message:)\s*$", text)
        chunks = parts[1:]
    quotes = []
    for chunk in chunks:
        chunk = chunk.strip()
        if not chunk:
            continue
        # Exporters sometimes concatenate From/To/Sent/Subject into one line.
        sender_match = re.search(r"(?:^|\n)\s*(?:From|보낸 사람)\s*:\s*(.*?)"
                                 r"(?=\s*(?:To|받는 사람|Sent|Date|보낸 날짜)\s*:|\n|$)",
                                 chunk, re.IGNORECASE)
        sender_name, sender = "", ""
        if sender_match:
            parsed = getaddresses([sender_match.group(1)])
            if len(parsed) == 1 and "@" in parsed[0][1]:
                sender_name, sender = parsed[0][0], parsed[0][1].lower()
        date_match = re.search(r"(?:Sent|Date|보낸 날짜)\s*:\s*(.*?)"
                               r"(?=\s*(?:Subject|제목)\s*:|\n|$)", chunk, re.IGNORECASE)
        subject_match = re.search(r"(?m)^\s*(?:Subject|제목)\s*:\s*([^\n]*)\n", chunk)
        # HTML alternatives retain the line break that many text/plain exports lose.
        display = chunk[subject_match.end():].strip() if subject_match else chunk
        quotes.append({"sender": sender, "sender_name": sender_name,
                       "date": date_match.group(1).strip() if date_match else "",
                       "sent_at": quote_timestamp(date_match.group(1)) if date_match else "",
                       "subject": subject_match.group(1).strip() if subject_match else "",
                       "body": readable(display)})
    if not quotes and " ".join(text.split()) != " ".join(readable(authored).split()):
        quoted_lines = [line.lstrip()[1:].lstrip() for line in text.splitlines()
                        if line.lstrip().startswith(">")]
        if quoted_lines:
            quotes.append({"sender": "", "sender_name": "", "date": "",
                           "body": "\n".join(quoted_lines)})
        elif text.startswith(readable(authored).strip()):
            remainder = text[len(readable(authored).strip()):].strip()
            if remainder:
                quotes.append({"sender": "", "sender_name": "", "date": "", "body": remainder})
    return quotes


def read_context(path, mail):
    headers = BytesParser(policy=policy.default).parsebytes(Path(path).read_bytes())
    names = {address.lower(): name for name, address in
             getaddresses(headers.get_all("To", []) + headers.get_all("Cc", []))
             if address and name}
    quotes = quoted_messages(mail.body, mail.authored_body)
    html = headers.get_body(preferencelist=("html",))
    if html is not None:
        try:
            html_quotes = quoted_messages(html_text(html.get_content()), mail.authored_body)
            if len(html_quotes) >= len(quotes):
                quotes = html_quotes
        except LookupError:
            pass
    return {"recipient_names": names, "in_reply_to": str(headers.get("In-Reply-To", "")),
            "references": re.findall(r"<[^>]+>", str(headers.get("References", ""))),
            "quotes": quotes}


def topic(subject):
    return re.sub(r"^(?:(?:re|fw|fwd)\s*:\s*)+", "", readable(subject), flags=re.IGNORECASE).strip()


def timeline(messages, owner, deleted=()):
    """Prefer actual sent records; show repeated quoted copies only once."""
    items = []
    for mail in messages:
        items.append({"kind": "sent", "sender": owner, "sender_name": "나", "mine": True,
                      "sent_at": mail["sent_at"], "subject": topic(mail["subject"]),
                      "body": mail["display_body"], "original_body": readable(mail["original_authored_body"]),
                      "source_ids": [mail["id"]], "sort_at": mail["sent_at"], "has_reply": mail["has_reply"],
                      "keys": ["sent:" + mail.get("fingerprint", str(mail["id"]))]})
    for mail in messages:
        quotes = mail["context"].get("quotes", [])
        for index, quote in enumerate(reversed(quotes)):
            body = readable(quote["body"])
            stamp = quote.get("sent_at") or quote_timestamp(quote.get("date", ""))
            normalized = re.sub(r"\s+", "", body)
            identity = (quote["sender"], stamp, normalized)
            if not quote["sender"] or not stamp:
                identity += (mail.get("fingerprint", str(mail["id"])), index)
            key = "quote:" + sha256(repr(identity).encode()).hexdigest()
            duplicate = None
            for existing in items:
                if not quote["sender"] or existing["sender"] != quote["sender"]:
                    continue
                if re.sub(r"\s+", "", existing["original_body"]) != normalized:
                    continue
                if stamp and existing["sent_at"] and abs((datetime.fromisoformat(stamp) -
                        datetime.fromisoformat(existing["sent_at"])).total_seconds()) <= 2:
                    duplicate = existing
                    break
            if duplicate:
                if key not in duplicate["keys"]:
                    duplicate["keys"].append(key)
                if mail["id"] not in duplicate["source_ids"]:
                    duplicate["source_ids"].append(mail["id"])
                continue
            items.append({"kind": "quote", "sender": quote["sender"],
                          "sender_name": quote["sender_name"] or quote["sender"] or "작성자 미확인",
                          "mine": quote["sender"] == owner, "sent_at": stamp,
                          "subject": topic(quote.get("subject") or mail["subject"]), "body": body,
                          "original_body": body, "source_ids": [mail["id"]],
                          "sort_at": stamp or mail["sent_at"], "quote_order": index, "has_reply": True,
                          "keys": [key]})
    items.sort(key=lambda item: (item["sort_at"], item["kind"] == "sent", item.get("quote_order", 0)))
    for item in items:
        item.pop("original_body", None)
        item.pop("sort_at", None)
        item.pop("quote_order", None)
    return [item for item in items if not any(key in deleted for key in item["keys"])]
