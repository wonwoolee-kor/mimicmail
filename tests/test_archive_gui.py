from contextlib import closing, redirect_stdout, redirect_stderr
from datetime import date
from email.message import EmailMessage
import json
import io
import shlex
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from mimicmail.context import quoted_messages, read_context, timeline
from mimicmail.gui import ConflictError, create_conversation, draft_context, prompt_reference, delete_message, conversation, conversations, create_server, get_mail, update_contact_label, update_mail
from mimicmail.pipeline import connect, draft, ingest, retrieve
from mimicmail.mail import parse_mail
from mimicmail.__main__ import main
from test_pipeline import FakeLLM, UNKNOWN


class Classifier:
    def __init__(self):
        self.calls = 0

    def complete(self, instructions, data, schema):
        self.calls += 1
        return {"purpose": "request", "role": "unknown", "role_evidence": "",
                "relationship": "unknown", "relationship_evidence": ""}


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.path = self.root / "data" / "mail.sqlite3"
        self.db = connect(self.path)
        self.addCleanup(self.db.close)
        self.llm = Classifier()

    def mail(self, name, when="Thu, 01 Jan 2026 10:00:00 +0900", to="팀장 <boss@example.com>", reply=False):
        message = EmailMessage()
        message["From"] = "나 <me@example.com>"
        message["To"] = to
        message["Subject"] = name
        if when:
            message["Date"] = when
        body = "안녕하세요. 검토 부탁드립니다.\n감사합니다."
        if reply:
            message["In-Reply-To"] = "<incoming@example.com>"
            message["References"] = "<earlier@example.com> <incoming@example.com>"
            body += ('\n\n-----Original Message-----From: "팀장" &lt;boss@example.com&gt;'
                     'To: &lt;me@example.com&gt;;Sent: 2025-12-31 11:00:00 (UTC+09:00)'
                     'Subject: 이전 요청\n상대방이 작성한 중요한 답변입니다.')
        message.set_content(body)
        path = self.root / f"{name}.eml"
        path.write_bytes(message.as_bytes())
        return path

    def populate(self):
        ingest(self.db, self.llm, [self.mail("답장 메일", reply=True),
                                   self.mail("다른 대화", to="Peer <peer@example.com>")], "me@example.com")
        return self.db.execute("SELECT id FROM mail_archive WHERE subject='답장 메일'").fetchone()[0]

    def payload(self, mail_id):
        current = get_mail(self.db, mail_id)
        return {"revision": current["revision"], "authored_body": "수정한 내 문체입니다.",
                "recipients": current["recipients"]}

    def test_since_is_inclusive_in_korea_and_excludes_undated(self):
        files = [self.mail("before", "Wed, 31 Dec 2025 14:59:59 +0000"),
                 self.mail("boundary", "Wed, 31 Dec 2025 15:00:00 +0000"),
                 self.mail("after", "Wed, 31 Dec 2025 16:00:00 +0000"),
                 self.mail("missing", None),
                 self.mail("naive", "Thu, 01 Jan 2026 00:00:00")]
        counts = ingest(self.db, self.llm, files, "me@example.com", since=date(2026, 1, 1))
        self.assertEqual(counts, {"indexed": 2, "skipped": 3, "excluded_before_date": 1,
                                  "excluded_unknown_date": 2})
        self.assertEqual(self.llm.calls, 2)
        subjects = {r[0] for r in self.db.execute("SELECT subject FROM mail_archive")}
        self.assertEqual(subjects, {"boundary", "after"})

    def test_reply_context_is_preserved_but_never_used_as_style(self):
        mail_id = self.populate()
        mail = get_mail(self.db, mail_id)
        self.assertTrue(mail["has_reply"])
        self.assertEqual(mail["context"]["recipient_names"]["boss@example.com"], "팀장")
        self.assertEqual(mail["context"]["quotes"][0]["sender"], "boss@example.com")
        self.assertIn("2025-12-31", mail["context"]["quotes"][0]["date"])
        self.assertIn("중요한 답변", mail["original_body"])
        self.assertNotIn("중요한 답변", mail["authored_body"])
        self.assertNotIn("Original Message", mail["authored_body"])

    def test_contacts_search_and_reply_filter(self):
        self.populate()
        results = conversations(self.db)
        self.assertEqual(results["total_mails"], 2)
        self.assertEqual(results["total_conversations"], 2)
        self.assertEqual(results["reply_mails"], 1)
        replies = conversations(self.db, replies_only=True)["items"]
        self.assertEqual([c["recipient"] for c in replies], ["boss@example.com"])
        self.assertEqual(conversations(self.db, "중요한 답변")["items"][0]["name"], "팀장")
        self.assertEqual(conversations(self.db, "no-matching-string")["items"], [])
        self.assertEqual(len(conversation(self.db, "me@example.com", "boss@example.com")["messages"]), 1)
        self.assertEqual(conversation(self.db, "someone@example.com", "boss@example.com")["messages"], [])

    def test_search_points_to_matching_quote_instead_of_latest_mail(self):
        self.populate()
        result = conversations(self.db, "중요한 답변")["items"][0]
        match = result["search_match"]
        self.assertIn("중요한 답변", match["preview"])
        self.assertNotIn("중요한 답변", result["preview"])
        message = next(item for item in conversation(self.db, result["owner"], result["recipient"])["timeline"]
                       if match["key"] in item["keys"])
        self.assertEqual(message["kind"], "quote")
        self.assertEqual(match["field"], "body")
        self.assertEqual(match["term"], "중요한 답변")

    def test_search_preview_centers_late_match_and_preserves_unicode(self):
        mail_id = self.populate()
        payload = self.payload(mail_id)
        payload["authored_body"] = "앞부분 😀ß " * 40 + "Straße <검토>" + " 뒤의 내용" * 40
        update_mail(self.db, mail_id, payload)
        match = conversations(self.db, "STRASSE")["items"][0]["search_match"]
        self.assertEqual(match["term"], "Straße")
        self.assertIn("Straße <검토>", match["preview"])
        self.assertTrue(match["preview"].startswith("…"))
        self.assertTrue(match["preview"].endswith("…"))
        self.assertLessEqual(len(match["preview"]), 142)

    def test_search_supports_subject_and_contact_only_matches(self):
        self.populate()
        match = conversations(self.db, "답장 메일")["items"][0]["search_match"]
        self.assertEqual(match["field"], "subject")
        self.assertEqual(match["preview"], "답장 메일")
        for query in ("peer@example.com", "Peer", ""):
            with self.subTest(query=query):
                result = next(item for item in conversations(self.db, query)["items"]
                              if item["recipient"] == "peer@example.com")
                self.assertIsNone(result["search_match"])

    def test_edit_updates_retrieval_preserves_original_and_detects_conflict(self):
        mail_id = self.populate()
        before = get_mail(self.db, mail_id)
        payload = self.payload(mail_id)
        payload["recipients"][0].update(role="팀장", relationship="superior", purpose="thanks")
        updated = update_mail(self.db, mail_id, payload)
        self.assertEqual(updated["revision"], 1)
        self.assertEqual(updated["original_body"], before["original_body"])
        self.assertEqual(updated["original_authored_body"], before["original_authored_body"])
        self.assertEqual(updated["context"], before["context"])
        result = retrieve(self.db, "me@example.com", "boss@example.com", payload["recipients"][0])
        self.assertEqual(result[0]["authored_body"], payload["authored_body"])
        with self.assertRaises(ConflictError):
            update_mail(self.db, mail_id, payload)
        self.assertEqual(get_mail(self.db, mail_id)["revision"], 1)

    def test_validation_does_not_change_archive(self):
        mail_id = self.populate()
        for field, value in (("authored_body", " "), ("recipients", []), ("revision", "0")):
            payload = self.payload(mail_id)
            payload[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                update_mail(self.db, mail_id, payload)
        self.assertEqual(get_mail(self.db, mail_id)["revision"], 0)

    def test_reimport_backfills_context_without_overwriting_edit(self):
        path = self.mail("reply", reply=True)
        ingest(self.db, self.llm, [path], "me@example.com")
        mail_id = self.db.execute("SELECT id FROM mail_archive").fetchone()[0]
        update_mail(self.db, mail_id, self.payload(mail_id))
        with self.db:
            self.db.execute("UPDATE mail_archive SET context_json='{}'")
        counts = ingest(self.db, self.llm, [path], "me@example.com")
        self.assertEqual(counts, {"indexed": 0, "skipped": 1})
        self.assertEqual(self.llm.calls, 1)
        updated = get_mail(self.db, mail_id)
        self.assertTrue(updated["has_reply"])
        self.assertEqual(updated["authored_body"], "수정한 내 문체입니다.")
        self.assertEqual(updated["revision"], 1)

    def test_unattributed_inline_quotes_do_not_invent_sender(self):
        result = quoted_messages("> 相手の質問\n私の返事", "私の返事")
        self.assertEqual(result[0]["sender"], "")
        self.assertEqual(result[0]["body"], "相手の質問")

    def test_manual_rank_applies_to_contact_and_future_mail(self):
        mail_id = self.populate()
        before = get_mail(self.db, mail_id)
        stale_edit = self.payload(mail_id)
        payload = {"owner": "me@example.com", "recipient": "boss@example.com",
                   "label": "  교수  ", "revision": 0}
        self.assertEqual(update_contact_label(self.db, payload), {"label": "교수", "revision": 1})
        after = get_mail(self.db, mail_id)
        self.assertEqual(after["recipients"][0]["role"], "교수")
        self.assertEqual(after["original_body"], before["original_body"])
        self.assertEqual(after["context"], before["context"])
        self.assertEqual(conversations(self.db, label_filter="교수")["items"][0]["recipient"], "boss@example.com")
        self.assertEqual(conversations(self.db, "교수")["labels"], ["교수"])
        with self.assertRaises(ConflictError):
            update_contact_label(self.db, payload)
        with self.assertRaises(ConflictError):
            update_mail(self.db, mail_id, stale_edit)
        edit = self.payload(mail_id)
        edit["recipients"][0]["role"] = "stale automatic role"
        self.assertEqual(update_mail(self.db, mail_id, edit)["recipients"][0]["role"], "교수")
        ingest(self.db, self.llm, [self.mail("새 발신 메일")], "me@example.com")
        self.assertEqual({r[0] for r in self.db.execute(
            "SELECT role FROM sent_mail WHERE recipient='boss@example.com'")}, {"교수"})
        update_contact_label(self.db, dict(payload, label="", revision=1))
        self.assertEqual(conversations(self.db)["labels"], [])
        self.assertEqual(get_mail(self.db, mail_id)["recipients"][0]["role"], "unknown")

    def test_draft_uses_manual_rank_and_explicit_override(self):
        self.populate()
        update_contact_label(self.db, {"owner": "me@example.com", "recipient": "boss@example.com",
                                      "label": "교수", "revision": 0})
        for override, expected in ((None, "교수"), ("학과장", "학과장")):
            model = FakeLLM(UNKNOWN, {"subject": "답장", "body": "감사합니다.", "missing_facts": []})
            draft(self.db, model, self.mail("현재 메일"), "me@example.com", "boss@example.com",
                  "감사 인사", role=override)
            self.assertEqual(model.calls[-1][1]["recipient_context"]["role"], expected)

    def test_timeline_deduplicates_quotes_but_preserves_repeated_messages(self):
        def sent(mail_id, stamp, body, quotes=()):
            return {"id": mail_id, "sent_at": stamp, "subject": "RE: 일정",
                    "display_body": "편집한 문체" if mail_id == 1 else body,
                    "original_authored_body": body, "has_reply": bool(quotes),
                    "context": {"quotes": list(quotes)}}
        first = sent(1, "2026-01-01T10:00:00+09:00", "확인했습니다. 감사합니다.")
        own_quote = {"sender": "me@example.com", "sender_name": "나",
                     "sent_at": "2026-01-01T10:00:01+09:00", "body": "확인했습니다.\n감사합니다."}
        reply = {"sender": "boss@example.com", "sender_name": "교수",
                 "sent_at": "2026-01-02T10:00:00+09:00", "body": "자료도 보내주세요."}
        later = sent(2, "2026-01-03T10:00:00+09:00", "확인했습니다. 감사합니다.", [reply, own_quote])
        last = sent(3, "2026-01-04T10:00:00+09:00", "자료 보냅니다.", [reply, own_quote])
        result = timeline([first, later, last], "me@example.com")
        self.assertEqual([item["kind"] for item in result], ["sent", "quote", "sent", "sent"])
        self.assertEqual(result[0]["body"], "편집한 문체")
        self.assertEqual(result[0]["source_ids"], [1, 2, 3])
        self.assertEqual(result[1]["source_ids"], [2, 3])
        self.assertTrue(all(item["subject"] == "일정" for item in result))

    def test_html_quote_headers_are_separated_from_message(self):
        message = EmailMessage()
        message["From"] = "me@example.com"
        message["To"] = "boss@example.com"
        message.set_content("감사합니다.\n-----Original Message-----From: boss@example.comTo: me@example.comSent: 2026-01-01 10:00:00 (UTC+09:00)Subject: 일정본문")
        message.add_alternative('<p>감사합니다.</p><p>-----Original Message-----</p>'
            '<p>From: 교수 &lt;boss@example.com&gt;</p><p>To: me@example.com</p>'
            '<p>Sent: 2026-01-01 10:00:00 (UTC+09:00)</p><p>Subject: 일정</p>'
            '<p>금요일에 만나요.</p>', subtype="html")
        path = self.root / "html.eml"
        path.write_bytes(message.as_bytes())
        quotes = read_context(path, parse_mail(path))["quotes"]
        self.assertEqual(quotes[0]["sender"], "boss@example.com")
        self.assertEqual(quotes[0]["subject"], "일정")
        self.assertEqual(quotes[0]["body"], "금요일에 만나요.")

    def deletion(self, recipient="boss@example.com", kind="sent"):
        data = conversation(self.db, "me@example.com", recipient)
        item = next(item for item in data["timeline"] if item["kind"] == kind)
        mail = next(mail for mail in data["messages"] if mail["id"] == item["source_ids"][0])
        return {"owner": "me@example.com", "recipient": recipient,
                "key": item["keys"][0], "revision": mail["revision"]}

    def test_delete_sent_excludes_style_preserves_replies_and_survives_reimport(self):
        mail_id = self.populate()
        before = get_mail(self.db, mail_id)
        delete_message(self.db, self.deletion())
        after = get_mail(self.db, mail_id)
        self.assertTrue(after["deleted"])
        self.assertEqual(after["original_body"], before["original_body"])
        remaining = conversation(self.db, "me@example.com", "boss@example.com")["timeline"]
        self.assertEqual([item["kind"] for item in remaining], ["quote"])
        references = retrieve(self.db, "me@example.com", "boss@example.com", before["recipients"][0])
        self.assertNotIn(before["fingerprint"], [item["fingerprint"] for item in references])
        self.assertEqual(conversations(self.db)["total_mails"], 1)
        with self.assertRaises(ConflictError):
            update_mail(self.db, mail_id, self.payload(mail_id))
        calls = self.llm.calls
        ingest(self.db, self.llm, [self.root / "답장 메일.eml"], "me@example.com")
        self.assertEqual(self.llm.calls, calls)
        self.assertTrue(get_mail(self.db, mail_id)["deleted"])
        self.assertEqual(len(conversation(self.db, "me@example.com", "boss@example.com")["timeline"]), 1)

    def test_delete_repeated_quote_preserves_sent_records_and_original(self):
        first = self.mail("답장1", reply=True)
        second = self.mail("답장2", reply=True)
        ingest(self.db, self.llm, [first, second], "me@example.com")
        before = conversation(self.db, "me@example.com", "boss@example.com")
        self.assertEqual(len(before["timeline"]), 3)
        payload = self.deletion(kind="quote")
        delete_message(self.db, payload)
        after = conversation(self.db, "me@example.com", "boss@example.com")
        self.assertEqual(len(after["timeline"]), 2)
        self.assertTrue(all(item["kind"] == "sent" for item in after["timeline"]))
        self.assertEqual([mail["original_body"] for mail in before["messages"]],
                         [mail["original_body"] for mail in after["messages"]])
        self.assertEqual(conversations(self.db, "중요한 답변")["items"], [])
        with self.assertRaises(LookupError):
            delete_message(self.db, payload)

    def test_delete_last_message_removes_empty_contact_and_rejects_stale_revision(self):
        mail_id = self.populate()
        payload = self.deletion()
        update_mail(self.db, mail_id, self.payload(mail_id))
        with self.assertRaises(ConflictError):
            delete_message(self.db, payload)
        self.assertFalse(get_mail(self.db, mail_id)["deleted"])
        delete_message(self.db, self.deletion("peer@example.com"))
        self.assertEqual([item["recipient"] for item in conversations(self.db)["items"]], ["boss@example.com"])
        for payload in ({}, {"owner": "other@example.com", "recipient": "boss@example.com", "key": "sent:fake"}):
            with self.assertRaises((ValueError, LookupError)):
                delete_message(self.db, payload)

    def test_delete_sent_hides_repeated_quote_copies(self):
        first = self.mail("original")
        ingest(self.db, self.llm, [first], "me@example.com")
        first_id = self.db.execute("SELECT id FROM mail_archive").fetchone()[0]
        original = get_mail(self.db, first_id)
        later = self.mail("later", when="Fri, 02 Jan 2026 10:00:00 +0900")
        ingest(self.db, self.llm, [later], "me@example.com")
        with self.db:
            self.db.execute("UPDATE mail_archive SET context_json=? WHERE subject='later'", (json.dumps({
                "quotes": [{"sender": "me@example.com", "sender_name": "나", "body": original["authored_body"],
                            "sent_at": "2026-01-01T10:00:01+09:00"}]}),))
        self.assertEqual(len(conversation(self.db, "me@example.com", "boss@example.com")["timeline"]), 2)
        delete_message(self.db, self.deletion())
        remaining = conversation(self.db, "me@example.com", "boss@example.com")["timeline"]
        self.assertEqual(len(remaining), 1)
        self.assertNotIn(first_id, remaining[0]["source_ids"])

    def test_new_conversation_persists_without_creating_mail_and_reuses_existing(self):
        self.populate()
        payload = {"owner": "ME@example.com", "recipient": "New@example.com",
                   "name": "새 수신자", "label": "교수"}
        result = create_conversation(self.db, payload)
        self.assertTrue(result["created"])
        contact = result["contact"]
        self.assertEqual((contact["owner"], contact["recipient"], contact["name"], contact["label"]),
                         ("me@example.com", "new@example.com", "새 수신자", "교수"))
        self.assertEqual(contact["count"], 0)
        self.assertEqual(conversation(self.db, contact["owner"], contact["recipient"])["timeline"], [])
        self.assertEqual(self.db.execute("SELECT count(*) FROM sent_mail").fetchone()[0], 2)
        with closing(connect(self.path)) as reopened:
            self.assertEqual(conversations(reopened)["total_conversations"], 3)
            matches = conversations(reopened, label_filter="교수")["items"]
            self.assertEqual(matches[0]["recipient"], "new@example.com")
        repeated = create_conversation(self.db, dict(payload, name="덮어쓰기", label="팀장"))
        self.assertFalse(repeated["created"])
        self.assertEqual(repeated["contact"]["label"], "교수")
        existing = create_conversation(self.db, dict(payload, recipient="boss@example.com"))
        self.assertFalse(existing["created"])
        self.assertEqual(self.db.execute("SELECT count(*) FROM manual_conversations").fetchone()[0], 1)
        update_contact_label(self.db, {"owner": "me@example.com", "recipient": "new@example.com",
                                      "label": "연구원", "revision": 1})
        self.assertEqual(conversations(self.db, label_filter="연구원")["items"][0]["name"], "새 수신자")

    def test_new_conversation_validation_and_empty_database(self):
        payload = {"owner": "me@example.com", "recipient": "new@example.com", "name": "", "label": ""}
        for field, value in (("owner", ""), ("recipient", "not-an-email"), ("name", "x" * 101), ("label", [])):
            with self.subTest(field=field), self.assertRaises(ValueError):
                create_conversation(self.db, dict(payload, **{field: value}))
        self.assertEqual(self.db.execute("SELECT count(*) FROM manual_conversations").fetchone()[0], 0)
        result = create_conversation(self.db, payload)
        self.assertEqual(result["contact"]["name"], "new")
        self.assertEqual(conversations(self.db)["owners"], ["me@example.com"])

    def test_db_context_separates_original_facts_styles_and_deleted_messages(self):
        mail_id = self.populate()
        original = get_mail(self.db, mail_id)
        update_mail(self.db, mail_id, self.payload(mail_id))
        data = draft_context(self.db, "me@example.com", "boss@example.com", "reply")
        self.assertEqual(data["current_facts"]["conversation"][-1]["body"], original["display_original_authored"])
        self.assertEqual(data["style_references"][0]["authored_body"], "수정한 내 문체입니다.")
        self.assertNotIn("중요한 답변", json.dumps(data["style_references"], ensure_ascii=False))
        delete_message(self.db, self.deletion(kind="quote"))
        self.assertEqual(len(draft_context(self.db, "me@example.com", "boss@example.com")["current_facts"]["conversation"]), 1)
        delete_message(self.db, self.deletion())
        deleted = draft_context(self.db, "me@example.com", "boss@example.com")
        self.assertEqual(deleted["current_facts"]["conversation"], [])
        self.assertNotIn("수정한 내 문체입니다.", json.dumps(deleted["style_references"], ensure_ascii=False))

    def test_new_recipient_context_retrieves_by_rank_without_copying_other_facts(self):
        self.populate()
        update_contact_label(self.db, {"owner": "me@example.com", "recipient": "boss@example.com",
                                      "label": "교수", "revision": 0})
        create_conversation(self.db, {"owner": "me@example.com", "recipient": "new@example.com", "label": "교수"})
        data = draft_context(self.db, "me@example.com", "new@example.com")
        self.assertEqual(data["current_facts"]["conversation"], [])
        self.assertEqual(data["recipient_context"]["role"], "교수")
        self.assertEqual(data["style_references"][0]["recipient"], "boss@example.com")
        self.assertEqual(draft_context(self.db, "other@example.com", "new@example.com")["style_references"], [])

    def test_prompt_reference_command_reads_db_without_ai_or_writes(self):
        self.populate()
        reference = prompt_reference(self.path, "me@example.com", "boss@example.com")
        self.assertEqual(reference["database_path"], str(self.path.resolve()))
        args = shlex.split(reference["context_command"])
        self.assertEqual(args[1:3], ["-m", "mimicmail"])
        self.assertNotIn("authored_body", json.dumps(reference))
        before = self.path.read_bytes()
        output = io.StringIO()
        with patch("mimicmail.__main__.Codex", side_effect=AssertionError("No model call")), redirect_stdout(output):
            self.assertEqual(main(args[3:]), 0)
        data = json.loads(output.getvalue())
        self.assertEqual(data["current_facts"]["recipient"], "boss@example.com")
        self.assertEqual(len(data["current_facts"]["conversation"]), 2)
        self.assertEqual(self.path.read_bytes(), before)
        missing = self.root / "missing.sqlite3"
        with redirect_stderr(io.StringIO()):
            self.assertEqual(main(["--db", str(missing), "--user", "me@example.com", "context", "--to", "new@example.com"]), 1)
        self.assertFalse(missing.exists())

    def test_http_read_save_and_local_origin_protection(self):
        mail_id = self.populate()
        server = create_server(self.path, 0)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(thread.join, 2)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        url = f"http://127.0.0.1:{server.server_port}"
        with urlopen(url) as response:
            self.assertEqual(response.status, 200)
            self.assertIn("frame-ancestors 'none'", response.headers["Content-Security-Policy"])
        with urlopen(url + "/api/conversations") as response:
            token = json.load(response)["token"]
        payload = json.dumps(self.payload(mail_id)).encode()
        for headers in ({"Content-Type": "application/json"},
                        {"Content-Type": "application/json", "X-Mimicmail-Token": token,
                         "Origin": "https://untrusted.example"}):
            with self.assertRaises(HTTPError) as error:
                urlopen(Request(url + f"/api/mail/{mail_id}", data=payload, method="PUT", headers=headers))
            self.assertEqual(error.exception.code, 403)
        with urlopen(Request(url + f"/api/mail/{mail_id}", data=payload, method="PUT",
                             headers={"Content-Type": "application/json", "X-Mimicmail-Token": token,
                                      "Origin": url})) as response:
            self.assertEqual(json.load(response)["revision"], 1)
        with self.assertRaises(HTTPError) as error:
            urlopen(Request(url + "/api/conversations", headers={"Host": "untrusted.example"}))
        self.assertEqual(error.exception.code, 403)
        deletion = json.dumps(self.deletion()).encode()
        for headers in ({"Content-Type": "application/json"},
                        {"Content-Type": "application/json", "X-Mimicmail-Token": token,
                         "Origin": "https://untrusted.example"}):
            with self.assertRaises(HTTPError) as error:
                urlopen(Request(url + "/api/message", data=deletion, method="DELETE", headers=headers))
            self.assertEqual(error.exception.code, 403)
        self.assertFalse(get_mail(self.db, mail_id)["deleted"])
        with urlopen(Request(url + "/api/message", data=deletion, method="DELETE",
                             headers={"Content-Type": "application/json", "X-Mimicmail-Token": token,
                                      "Origin": url})) as response:
            self.assertEqual(json.load(response), {"deleted": True})
        self.assertTrue(get_mail(self.db, mail_id)["deleted"])
        create_body = json.dumps({"owner": "me@example.com", "recipient": "new@example.com", "label": "교수"}).encode()
        with self.assertRaises(HTTPError) as error:
            urlopen(Request(url + "/api/conversation", data=create_body, method="POST",
                            headers={"Content-Type": "application/json"}))
        self.assertEqual(error.exception.code, 403)
        with urlopen(Request(url + "/api/conversation", data=create_body, method="POST",
                             headers={"Content-Type": "application/json", "X-Mimicmail-Token": token,
                                      "Origin": url})) as response:
            self.assertTrue(json.load(response)["created"])
        with urlopen(url + "/api/conversation?owner=me%40example.com&recipient=new%40example.com") as response:
            result = json.load(response)
            self.assertEqual(result["timeline"], [])
            self.assertEqual(result["prompt_reference"]["database_path"], str(self.path.resolve()))


if __name__ == "__main__":
    unittest.main()
