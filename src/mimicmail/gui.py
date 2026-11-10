"""A loopback-only chat-style browser for the local email archive."""

from contextlib import closing
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import re
import secrets
import shlex
import sqlite3
import sys
from urllib.parse import parse_qs, urlsplit
import webbrowser

from .context import readable, timeline
from .pipeline import GENERATE, PURPOSES, RELATIONSHIPS, connect, retrieve


PURPOSE_LABELS = dict(zip(PURPOSES, ("요청", "답변", "보고", "일정", "감사", "사과", "후속 확인", "기타")))
RELATIONSHIP_LABELS = dict(zip(RELATIONSHIPS, ("상사", "동료", "부하", "고객", "공급자", "지도교수",
                                             "지도학생", "개인", "외부 관계자", "미상")))


def has_reply(context):
    return bool(context.get("in_reply_to") or context.get("references") or context.get("quotes"))


def first_search_match(items, query):
    if not query:
        return None
    for item in items:
        for field in ("body", "subject"):
            text = item[field]
            offset = text.casefold().find(query)
            if offset < 0:
                continue
            # Case folding can expand characters, e.g. ß becomes ss.
            positions = [index for index, char in enumerate(text) for _ in char.casefold()]
            start, end = positions[offset], positions[offset + len(query) - 1] + 1
            left = max(0, start - 12)
            right = min(len(text), max(left + 140, end))
            preview = ("…" if left else "") + text[left:right] + ("…" if right < len(text) else "")
            return {"key": item["keys"][0], "field": field, "term": text[start:end], "preview": preview}
    return None


def conversations(db, query="", replies_only=False, label_filter=""):
    rows = db.execute("""SELECT a.owner, s.recipient, a.context_json FROM mail_archive a
        JOIN sent_mail s ON a.owner=s.owner AND a.fingerprint=s.fingerprint
        ORDER BY a.sent_at DESC, a.id DESC""")
    contacts = {}
    labels = {(row["owner"], row["recipient"]): dict(row) for row in db.execute("SELECT * FROM contact_labels")}
    for row in rows:
        key = (row["owner"], row["recipient"])
        name = json.loads(row["context_json"]).get("recipient_names", {}).get(row["recipient"], "")
        if key not in contacts or not contacts[key]:
            contacts[key] = name
    manual = {(row["owner"], row["recipient"]): dict(row)
              for row in db.execute("SELECT * FROM manual_conversations")}
    for key, item in manual.items():
        contacts[key] = item["name"] or contacts.get(key, "")
    groups, all_ids, reply_ids, owners = [], set(), set(), set()
    query = query.casefold().strip()
    for (owner, recipient), name in contacts.items():
        data = conversation(db, owner, recipient)
        registered = manual.get((owner, recipient))
        if not data["timeline"] and not registered:
            continue
        active = [mail for mail in data["messages"] if not mail["deleted"]]
        all_ids.update(mail["id"] for mail in active)
        reply_ids.update(mail["id"] for mail in active if mail["has_reply"])
        owners.add(owner)
        label = labels.get((owner, recipient), {"label": "", "revision": 0})
        latest = data["timeline"][-1] if data["timeline"] else {
            "sent_at": registered["created_at"], "subject": "", "body": ""}
        group = {"owner": owner, "recipient": recipient, "name": name or recipient.split("@")[0],
                 "label": label["label"], "label_revision": label["revision"], "count": len(active),
                 "registered": bool(registered),
                 "reply_count": sum(mail["has_reply"] for mail in active) or
                                sum(item["kind"] == "quote" for item in data["timeline"]),
                 "latest": latest["sent_at"], "subject": latest["subject"], "preview": latest["body"][:140],
                 "search_match": first_search_match(data["timeline"], query)}
        haystack = " ".join([recipient, name, label["label"],
            *[item["subject"] + " " + item["body"] for item in data["timeline"]],
            *[PURPOSE_LABELS[profile["purpose"]] for mail in active for profile in mail["recipients"]]])
        group["matches"] = (not query or query in readable(haystack).casefold()) and (
            not replies_only or bool(group["reply_count"])) and (not label_filter or group["label"] == label_filter)
        groups.append(group)
    groups.sort(key=lambda item: datetime.fromisoformat(item["latest"]).timestamp() if item["latest"] else 0,
                reverse=True)
    return {"items": [group for group in groups if group["matches"]], "total_mails": len(all_ids),
            "total_conversations": len(groups), "reply_mails": len(reply_ids), "owners": sorted(owners),
            "labels": sorted({group["label"] for group in groups if group["label"]})}


def create_conversation(db, payload):
    owner, recipient, name, label = (payload.get(key, "") for key in ("owner", "recipient", "name", "label"))
    if not all(isinstance(value, str) for value in (owner, recipient, name, label)):
        raise ValueError("수신자 정보를 확인하세요.")
    owner, recipient, name, label = owner.strip().lower(), recipient.strip().lower(), name.strip(), label.strip()
    if not all(re.fullmatch(r"[^\s@<>,;]+@[^\s@<>,;]+\.[^\s@<>,;]+", value) for value in (owner, recipient)):
        raise ValueError("올바른 발신자·수신자 이메일을 입력하세요.")
    if len(name) > 100 or len(label) > 100:
        raise ValueError("이름과 직급은 100자 이내로 입력하세요.")
    with db:
        db.execute("BEGIN IMMEDIATE")
        existing = next((item for item in conversations(db)["items"]
                         if item["owner"] == owner and item["recipient"] == recipient), None)
        if existing:
            return {"contact": existing, "created": False}
        db.execute("INSERT INTO manual_conversations VALUES (?, ?, ?, ?)",
                   (owner, recipient, name, datetime.now(timezone.utc).isoformat()))
        if label:
            revision = db.execute("SELECT revision FROM contact_labels WHERE owner=? AND recipient=?",
                                  (owner, recipient)).fetchone()
            update_contact_label(db, {"owner": owner, "recipient": recipient, "label": label,
                                      "revision": revision[0] if revision else 0})
    contact = next(item for item in conversations(db)["items"]
                   if item["owner"] == owner and item["recipient"] == recipient)
    return {"contact": contact, "created": True}


def prompt_reference(db_path, owner, recipient):
    path = Path(db_path).resolve()
    command = shlex.join([sys.executable, "-m", "mimicmail", "--db", str(path),
                          "--user", owner, "context", "--to", recipient])
    return {"database_path": str(path), "project_path": str(Path(__file__).resolve().parents[2]),
            "context_command": command}


def draft_context(db, owner, recipient, purpose="other"):
    owner, recipient = owner.strip().lower(), recipient.strip().lower()
    data = conversation(db, owner, recipient)
    active = [mail for mail in data["messages"] if not mail["deleted"]]
    profile = next((item.copy() for item in active[-1]["recipients"] if item["recipient"] == recipient), {}) if active else {}
    label = db.execute("SELECT label FROM contact_labels WHERE owner=? AND recipient=?", (owner, recipient)).fetchone()
    profile = {"role": (label[0] or "unknown") if label else profile.get("role", "unknown"),
               "relationship": profile.get("relationship", "unknown"), "purpose": purpose}
    by_id = {mail["id"]: mail for mail in active}
    facts = [{"sender": item["sender"] or "작성자 미확인", "date": item["sent_at"], "subject": item["subject"],
              "body": by_id[item["source_ids"][0]]["display_original_authored"] if item["kind"] == "sent" else item["body"]}
             for item in data["timeline"]]
    references = retrieve(db, owner, recipient, profile)
    return {"instructions": GENERATE, "current_facts": {"user": owner, "recipient": recipient, "conversation": facts},
            "recipient_context": profile, "style_references": [{key: row[key] for key in
                ("recipient", "role", "relationship", "purpose", "authored_body")} for row in references]}


def get_mail(db, mail_id):
    row = db.execute("SELECT * FROM mail_archive WHERE id=?", (mail_id,)).fetchone()
    if row is None:
        raise LookupError("메일을 찾을 수 없습니다.")
    recipients = [dict(item) for item in db.execute("""SELECT recipient, role, relationship, purpose,
        authored_body FROM sent_mail WHERE owner=? AND fingerprint=? ORDER BY recipient""",
        (row["owner"], row["fingerprint"]))]
    item = dict(row)
    item["to"] = json.loads(item.pop("to_json"))
    item["cc"] = json.loads(item.pop("cc_json"))
    item["context"] = json.loads(item.pop("context_json"))
    item["authored_body"] = recipients[0]["authored_body"] if recipients else item["original_authored_body"]
    item["recipients"] = [{key: value for key, value in recipient.items() if key != "authored_body"}
                          for recipient in recipients]
    item["display_body"] = readable(item["authored_body"])
    item["display_original"] = readable(item["original_body"])
    item["display_original_authored"] = readable(item["original_authored_body"])
    item["has_reply"] = has_reply(item["context"])
    item["deleted"] = db.execute("SELECT 1 FROM deleted_messages WHERE owner=? AND message_key=?",
                                 (item["owner"], "sent:" + item["fingerprint"])).fetchone() is not None
    return item


def conversation(db, owner, recipient):
    ids = db.execute("""SELECT a.id FROM mail_archive a JOIN sent_mail s
        ON a.owner=s.owner AND a.fingerprint=s.fingerprint
        WHERE a.owner=? AND s.recipient=? ORDER BY a.sent_at, a.id""", (owner, recipient))
    messages = [get_mail(db, row["id"]) for row in ids]
    deleted = {row[0] for row in db.execute("SELECT message_key FROM deleted_messages WHERE owner=?", (owner,))}
    return {"messages": messages, "timeline": timeline(messages, owner, deleted)}


def delete_message(db, payload):
    owner, recipient, key = (payload.get(field) for field in ("owner", "recipient", "key"))
    if not all(isinstance(value, str) and value for value in (owner, recipient, key)):
        raise ValueError("삭제할 메시지 정보가 올바르지 않습니다.")
    with db:
        db.execute("BEGIN IMMEDIATE")
        data = conversation(db, owner, recipient)
        item = next((item for item in data["timeline"] if key in item["keys"]), None)
        if item is None:
            raise LookupError("이미 삭제되었거나 찾을 수 없는 메시지입니다. 새로고침하세요.")
        if item["kind"] == "sent":
            mail = next(mail for mail in data["messages"] if mail["id"] == item["source_ids"][0])
            if type(payload.get("revision")) is not int or payload["revision"] != mail["revision"]:
                raise ConflictError("다른 창에서 변경된 메시지입니다. 새로고침 후 다시 삭제하세요.")
        # Include copies in other recipients' conversations and any original sent record.
        all_mail = [get_mail(db, row[0]) for row in db.execute(
            "SELECT id FROM mail_archive WHERE owner=?", (owner,))]
        keys = set(item["keys"])
        for related in timeline(all_mail, owner):
            if keys.intersection(related["keys"]):
                keys.update(related["keys"])
        db.executemany("INSERT OR IGNORE INTO deleted_messages VALUES (?, ?)",
                       [(owner, value) for value in keys])
    return {"deleted": True}


def update_contact_label(db, payload):
    owner, recipient, label = (payload.get(key) for key in ("owner", "recipient", "label"))
    revision = payload.get("revision")
    if not all(isinstance(value, str) for value in (owner, recipient, label)) or len(label) > 100:
        raise ValueError("직급 라벨은 100자 이내로 입력하세요.")
    if type(revision) is not int or revision < 0:
        raise ValueError("라벨의 수정 버전이 올바르지 않습니다.")
    if not db.execute("""SELECT 1 FROM sent_mail WHERE owner=? AND recipient=?
        UNION ALL SELECT 1 FROM manual_conversations WHERE owner=? AND recipient=?""",
        (owner, recipient, owner, recipient)).fetchone():
        raise LookupError("대화를 찾을 수 없습니다.")
    current = db.execute("SELECT revision FROM contact_labels WHERE owner=? AND recipient=?", (owner, recipient)).fetchone()
    if (current["revision"] if current else 0) != revision:
        raise ConflictError("다른 창에서 변경된 라벨입니다. 새로고침 후 다시 지정하세요.")
    label = label.strip()
    with db:
        changed = db.execute("""INSERT INTO contact_labels(owner, recipient, label, revision) VALUES(?, ?, ?, 1)
            ON CONFLICT(owner, recipient) DO UPDATE SET label=excluded.label, revision=contact_labels.revision+1
            WHERE contact_labels.revision=?""", (owner, recipient, label, revision)).rowcount
        if not changed:
            raise ConflictError("다른 창에서 변경된 라벨입니다. 새로고침 후 다시 지정하세요.")
        db.execute("UPDATE sent_mail SET role=? WHERE owner=? AND recipient=?", (label or "unknown", owner, recipient))
        db.execute("""UPDATE mail_archive SET revision=revision+1, updated_at=? WHERE owner=? AND fingerprint IN
            (SELECT fingerprint FROM sent_mail WHERE owner=? AND recipient=?)""",
            (datetime.now(timezone.utc).isoformat(), owner, owner, recipient))
    return {"label": label, "revision": revision + 1}


class ConflictError(ValueError):
    pass


def update_mail(db, mail_id, payload):
    current = get_mail(db, mail_id)
    if current["deleted"]:
        raise ConflictError("삭제된 메시지입니다. 편집기를 닫고 새로고침하세요.")
    body, profiles = payload.get("authored_body"), payload.get("recipients")
    if not isinstance(body, str) or not body.strip():
        raise ValueError("문체 참조 본문을 입력하세요.")
    if type(payload.get("revision")) is not int:
        raise ValueError("메일의 수정 버전이 없습니다. 메일을 다시 여세요.")
    expected = {item["recipient"] for item in current["recipients"]}
    if not isinstance(profiles, list) or not all(isinstance(item, dict) for item in profiles):
        raise ValueError("수신자 정보 형식이 올바르지 않습니다.")
    if len(profiles) != len(expected) or {item.get("recipient") for item in profiles} != expected:
        raise ValueError("기존 수신자의 분류만 수정할 수 있습니다.")
    for profile in profiles:
        if profile.get("purpose") not in PURPOSES or profile.get("relationship") not in RELATIONSHIPS:
            raise ValueError("유효한 목적과 관계를 선택하세요.")
        if not isinstance(profile.get("role"), str) or len(profile["role"]) > 100:
            raise ValueError("직책은 100자 이내로 입력하세요.")
    with db:
        changed = db.execute("UPDATE mail_archive SET revision=revision+1, updated_at=? WHERE id=? AND revision=?",
            (datetime.now(timezone.utc).isoformat(), mail_id, payload["revision"])).rowcount
        if not changed:
            raise ConflictError("다른 창에서 변경된 메일입니다. 입력 내용을 복사한 뒤 메일을 다시 여세요.")
        for profile in profiles:
            label = db.execute("SELECT label FROM contact_labels WHERE owner=? AND recipient=?",
                               (current["owner"], profile["recipient"])).fetchone()
            role = label["label"] if label is not None else profile["role"].strip()
            db.execute("""UPDATE sent_mail SET authored_body=?, role=?, relationship=?, purpose=?
                WHERE owner=? AND fingerprint=? AND recipient=?""",
                (body, role or "unknown", profile["relationship"], profile["purpose"],
                 current["owner"], current["fingerprint"], profile["recipient"]))
    return get_mail(db, mail_id)


def create_server(db_path, port=8765):
    db_path = Path(db_path).resolve()
    with closing(connect(db_path)):
        pass
    token = secrets.token_urlsafe(32)
    static = Path(__file__).with_name("static")

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, format, *args):
            pass

        def respond(self, status, value, content_type="application/json; charset=utf-8"):
            body = value if isinstance(value, bytes) else json.dumps(value, ensure_ascii=False).encode()
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self'; "
                             "connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'")
            self.end_headers()
            self.wfile.write(body)

        def allowed_host(self):
            return self.headers.get("Host") in {f"127.0.0.1:{self.server.server_port}",
                                                f"localhost:{self.server.server_port}"}

        def do_GET(self):
            if not self.allowed_host():
                return self.respond(403, {"error": "로컬 주소로 접속하세요."})
            url = urlsplit(self.path)
            assets = {"/": ("index.html", "text/html"), "/app.js": ("app.js", "text/javascript"),
                      "/app.css": ("app.css", "text/css")}
            if url.path in assets:
                name, mime = assets[url.path]
                return self.respond(200, (static / name).read_bytes(), mime + "; charset=utf-8")
            args = parse_qs(url.query)
            try:
                with closing(connect(db_path)) as db:
                    if url.path == "/api/conversations":
                        result = conversations(db, args.get("q", [""])[0], args.get("replies", ["0"])[0] == "1",
                                               args.get("label", [""])[0])
                        result.update(token=token, purposes=PURPOSE_LABELS, relationships=RELATIONSHIP_LABELS)
                    elif url.path == "/api/conversation":
                        result = conversation(db, args.get("owner", [""])[0], args.get("recipient", [""])[0])
                        result["prompt_reference"] = prompt_reference(
                            db_path, args.get("owner", [""])[0], args.get("recipient", [""])[0])
                    elif re.fullmatch(r"/api/mail/\d+", url.path):
                        result = get_mail(db, int(url.path.rsplit("/", 1)[1]))
                    else:
                        return self.respond(404, {"error": "페이지를 찾을 수 없습니다."})
                self.respond(200, result)
            except LookupError as error:
                self.respond(404, {"error": str(error)})
            except sqlite3.Error:
                self.respond(503, {"error": "보관함을 읽지 못했습니다. 잠시 후 다시 시도하세요."})

        def do_PUT(self):
            self.mutate()

        def do_DELETE(self):
            self.mutate()

        def do_POST(self):
            self.mutate()

        def mutate(self):
            origin = self.headers.get("Origin")
            allowed_origins = {f"http://127.0.0.1:{self.server.server_port}",
                               f"http://localhost:{self.server.server_port}"}
            if (not self.allowed_host() or self.headers.get("X-Mimicmail-Token") != token
                    or (origin and origin not in allowed_origins)):
                return self.respond(403, {"error": "보관함 화면에서 다시 시도하세요."})
            path = urlsplit(self.path).path
            if ((self.command == "DELETE" and path != "/api/message") or
                    (self.command == "POST" and path != "/api/conversation") or
                    (self.command == "PUT" and not re.fullmatch(r"/api/mail/\d+", path)
                     and path != "/api/contact-label")):
                return self.respond(404, {"error": "메일을 찾을 수 없습니다."})
            try:
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size <= 1_000_000:
                    raise ValueError("요청 크기가 올바르지 않습니다.")
                if self.headers.get("Content-Type", "").split(";")[0] != "application/json":
                    raise ValueError("JSON 형식으로 저장하세요.")
                payload = json.loads(self.rfile.read(size))
                if not isinstance(payload, dict):
                    raise ValueError("저장 요청 형식이 올바르지 않습니다.")
                with closing(connect(db_path)) as db:
                    if self.command == "POST":
                        result = create_conversation(db, payload)
                    elif self.command == "DELETE":
                        result = delete_message(db, payload)
                    elif path == "/api/contact-label":
                        result = update_contact_label(db, payload)
                    else:
                        result = update_mail(db, int(path.rsplit("/", 1)[1]), payload)
                self.respond(200, result)
            except ConflictError as error:
                self.respond(409, {"error": str(error)})
            except LookupError as error:
                self.respond(404, {"error": str(error)})
            except (ValueError, UnicodeError) as error:
                self.respond(400, {"error": str(error)})
            except sqlite3.Error:
                self.respond(503, {"error": "저장을 완료하지 못했습니다. 내용을 보관하고 다시 시도하세요."})

    return ThreadingHTTPServer(("127.0.0.1", port), Handler)


def serve(db_path, port=8765, open_browser=True):
    server = create_server(db_path, port)
    url = f"http://127.0.0.1:{server.server_port}"
    print(f"Mimicmail · {url}\n종료: Ctrl+C", flush=True)
    if open_browser:
        webbrowser.open(url)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
