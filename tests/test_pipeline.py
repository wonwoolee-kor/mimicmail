from contextlib import redirect_stderr, redirect_stdout
from copy import deepcopy
from email.message import EmailMessage
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError, URLError

from mimicmail.__main__ import main
from mimicmail.llm import OpenAI, object_schema
from mimicmail.mail import authored_text, parse_mail
from mimicmail.pipeline import classify, connect, draft, ingest, retrieve


UNKNOWN = {"purpose": "request", "role": "unknown", "role_evidence": "",
           "relationship": "unknown", "relationship_evidence": ""}


class FakeLLM:
    def __init__(self, *results):
        self.results = list(results)
        self.calls = []

    def complete(self, instructions, data, schema):
        self.calls.append((instructions, deepcopy(data), schema))
        return deepcopy(self.results.pop(0))


class MailTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.db = connect(":memory:")
        self.addCleanup(self.db.close)

    def mail(self, name="mail", sender="Me <ME@example.com>", to="boss@example.com",
             body="검토 부탁드립니다.\n감사합니다.", html=None, cc=None):
        message = EmailMessage()
        message["From"] = sender
        message["To"] = to
        message["Subject"] = "검토 요청"
        message["Message-ID"] = f"<{name}@example.com>"
        if cc:
            message["Cc"] = cc
        if body is not None:
            message.set_content(body)
        if html:
            if body is None:
                message.set_content(html, subtype="html")
            else:
                message.add_alternative(html, subtype="html")
        path = self.root / f"{name}.eml"
        path.write_bytes(message.as_bytes())
        return path

    def row(self, fingerprint, recipient="other@example.com", role="unknown",
            relationship="unknown", purpose="request", owner="me@example.com"):
        with self.db:
            self.db.execute("INSERT INTO sent_mail VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                            (owner, fingerprint, fingerprint, owner, recipient, role,
                             relationship, purpose, f"style {fingerprint}"))

    def test_mime_headers_and_plain_preference(self):
        path = self.mail(to="팀장 <BOSS@example.com>", cc="peer@example.com",
                         html="<p>HTML alternative</p>")
        mail = parse_mail(path)
        self.assertEqual(mail.sender, "me@example.com")
        self.assertEqual(mail.to, ["boss@example.com"])
        self.assertEqual(mail.cc, ["peer@example.com"])
        self.assertEqual(mail.subject, "검토 요청")
        self.assertIn("검토 부탁드립니다", mail.authored_body)
        self.assertNotIn("HTML alternative", mail.body)

    def test_text_attachment_is_not_used_as_body(self):
        path = self.mail()
        from email import policy
        from email.parser import BytesParser
        message = BytesParser(policy=policy.default).parsebytes(path.read_bytes())
        message.add_attachment("첨부파일 비밀", filename="note.txt")
        path.write_bytes(message.as_bytes())
        self.assertNotIn("첨부파일 비밀", parse_mail(path).body)

    def test_quoted_text_is_preserved_only_in_conversation(self):
        path = self.mail(body="제 문체입니다.\n\nOn Monday, Boss wrote:\n> 상대방 문체")
        mail = parse_mail(path)
        self.assertEqual(mail.authored_body, "제 문체입니다.")
        self.assertIn("상대방 문체", mail.body)

    def test_reply_markers(self):
        for marker in ("On Monday,\nBoss wrote:", "2026년 11월 9일 팀장님이 작성:",
                       "-----Original Message-----", "-------- 원본 메시지 --------",
                       "Begin forwarded message:",
                       "보낸 사람: 팀장\n보낸 날짜: 월요일\n받는 사람: 나\n제목: 요청"):
            with self.subTest(marker=marker):
                self.assertEqual(authored_text("작성한 문장.\n감사합니다.\n\n" + marker +
                                               "\n상대방 문장"), "작성한 문장.\n감사합니다.")
        self.assertEqual(authored_text("> 상대방 질문\n내 답변\n> 또 다른 질문\n내 답변2"),
                         "내 답변\n내 답변2")

    def test_html_quotes_and_scripts_are_excluded(self):
        path = self.mail(body=None, html=("<html><head><style>hidden</style></head><body>"
            "<p>안녕하세요.</p><p>검토 부탁드립니다.</p>"
            '<div class="gmail_quote"><div>상대방 글</div></div>'
            "<blockquote>다른 인용문</blockquote><script>bad()</script></body></html>"))
        mail = parse_mail(path)
        self.assertIn("검토 부탁드립니다.", mail.authored_body)
        for quote in ("상대방 글", "다른 인용문", "hidden", "bad()"):
            self.assertNotIn(quote, mail.authored_body)
        self.assertIn("상대방 글", mail.body)

    def test_outlook_html_reply_boundary(self):
        mail = parse_mail(self.mail(body=None, html=(
            '<div>내 문장</div><div id="divRplyFwdMsg">From: Boss</div>'
            '<p>옛 메일 내용</p>')))
        self.assertEqual(mail.authored_body, "내 문장")
        self.assertIn("옛 메일 내용", mail.body)

    def test_incoming_mail_never_reaches_classifier_or_index(self):
        llm = FakeLLM()
        path = self.mail(sender="boss@example.com", to="me@example.com")
        self.assertEqual(ingest(self.db, llm, [path], "me@example.com"),
                         {"indexed": 0, "skipped": 1})
        self.assertEqual(llm.calls, [])
        self.assertEqual(self.db.execute("SELECT count(*) FROM sent_mail").fetchone()[0], 0)

    def test_alias_sent_mail_and_repeat_ingestion(self):
        path = self.mail(sender="alias@example.com", body="내 문체\n> 상대방 글")
        llm = FakeLLM(UNKNOWN)
        self.assertEqual(ingest(self.db, llm, [path], "ME@example.com", ["ALIAS@example.com"]),
                         {"indexed": 1, "skipped": 0})
        self.assertEqual(ingest(self.db, llm, [path], "me@example.com", ["alias@example.com"]),
                         {"indexed": 0, "skipped": 1})
        row = self.db.execute("SELECT * FROM sent_mail").fetchone()
        self.assertEqual(row["authored_body"], "내 문체")
        self.assertEqual(row["owner"], "me@example.com")

    def test_quote_only_and_self_mail_are_skipped(self):
        paths = [self.mail("quotes", body="> other author's text"),
                 self.mail("self", to="me@example.com")]
        self.assertEqual(ingest(self.db, FakeLLM(), paths, "me@example.com"),
                         {"indexed": 0, "skipped": 2})

    def test_multi_recipient_deduplication(self):
        path = self.mail(to="boss@example.com, peer@example.com, me@example.com",
                         cc="boss@example.com")
        llm = FakeLLM(UNKNOWN, UNKNOWN)
        ingest(self.db, llm, [path], "me@example.com")
        self.assertEqual(len(llm.calls), 2)
        self.assertEqual(self.db.execute("SELECT count(*) FROM sent_mail").fetchone()[0], 2)
        self.assertEqual(len(retrieve(self.db, "me@example.com", "boss@example.com", UNKNOWN)), 1)

    def test_failed_multi_recipient_classification_is_atomic(self):
        path = self.mail(to="boss@example.com, peer@example.com")
        llm = FakeLLM(UNKNOWN, {**UNKNOWN, "purpose": "invalid"})
        with self.assertRaises(ValueError):
            ingest(self.db, llm, [path], "me@example.com")
        self.assertEqual(self.db.execute("SELECT count(*) FROM sent_mail").fetchone()[0], 0)

    def test_role_and_relationship_require_verbatim_evidence(self):
        mail = parse_mail(self.mail(body="김 팀장님, 검토 부탁드립니다."))
        llm = FakeLLM({**UNKNOWN, "role": "팀장", "role_evidence": "김 팀장님",
                       "relationship": "superior", "relationship_evidence": "제 상사입니다"})
        result = classify(llm, mail, "me@example.com", "boss@example.com")
        self.assertEqual(result["role"], "팀장")
        self.assertEqual(result["relationship"], "unknown")

    def test_search_priority_and_owner_isolation(self):
        self.row("purpose", purpose="request")
        self.row("role", role="팀장", purpose="thanks")
        self.row("relation", relationship="superior", purpose="thanks")
        self.row("recipient", recipient="boss@example.com", purpose="thanks")
        self.row("another-owner", recipient="boss@example.com", owner="stranger@example.com")
        profile = {**UNKNOWN, "role": "팀장", "relationship": "superior"}
        result = retrieve(self.db, "me@example.com", "boss@example.com", profile)
        self.assertEqual([row["fingerprint"] for row in result],
                         ["recipient", "relation", "role", "purpose"])

    def test_unknown_values_do_not_match_each_other(self):
        self.row("unrelated", purpose="thanks")
        self.assertEqual(retrieve(self.db, "me@example.com", "new@example.com", UNKNOWN), [])
        with self.assertRaises(ValueError):
            retrieve(self.db, "me@example.com", "new@example.com", UNKNOWN, limit=0)

    def test_draft_separates_current_facts_from_historical_style(self):
        self.row("old", recipient="boss@example.com")
        with self.db:
            self.db.execute("UPDATE sent_mail SET authored_body=?", ("과거 계약금 999만 원",))
        incoming = self.mail("current", sender="boss@example.com", to="me@example.com",
                             body="이번 회의는 화요일입니다.")
        llm = FakeLLM(UNKNOWN, {"subject": "회의 확인", "body": "화요일에 뵙겠습니다.",
                                "missing_facts": []})
        result = draft(self.db, llm, incoming, "me@example.com", "boss@example.com",
                       "참석한다고 답장", role="팀장", relationship="superior")
        instructions, data, _ = llm.calls[-1]
        self.assertNotIn("999", json.dumps(data["current_facts"], ensure_ascii=False))
        self.assertNotIn("화요일", json.dumps(data["style_references"], ensure_ascii=False))
        self.assertEqual(data["recipient_context"]["relationship"], "superior")
        self.assertEqual(result["classification"]["role_evidence"], "사용자 지정 (--role)")
        self.assertEqual(result["classification"]["relationship_evidence"],
                         "사용자 지정 (--relationship)")
        self.assertIn("사실을 새 메일에 가져오지 않는다", instructions)
        self.assertEqual(result["style_sources"][0]["fingerprint"], "old")
        self.assertEqual(result["missing_facts"], [])

    def test_empty_index_still_allows_draft_with_explicit_missing_facts(self):
        llm = FakeLLM(UNKNOWN, {"subject": "일정 확인", "body": "[확인 필요: 시간]에 뵙겠습니다.",
                                "missing_facts": ["시간"]})
        result = draft(self.db, llm, self.mail(), "me@example.com", "boss@example.com", "답장")
        self.assertEqual(result["style_sources"], [])
        self.assertEqual(result["missing_facts"], ["시간"])

    def test_cli_ingest_and_draft(self):
        sent = self.mail("sent")
        incoming = self.mail("incoming", sender="boss@example.com", to="me@example.com")
        database = self.root / "mail.sqlite3"
        common = ["--backend", "openai", "--db", str(database), "--user", "me@example.com"]
        llm = FakeLLM(UNKNOWN, UNKNOWN, {"subject": "Re: 요청", "body": "확인했습니다.",
                                         "missing_facts": []})
        with patch("mimicmail.__main__.OpenAI", return_value=llm):
            output = io.StringIO()
            with redirect_stdout(output):
                self.assertEqual(main(common + ["ingest", str(sent), str(incoming)]), 0)
            self.assertEqual(json.loads(output.getvalue()), {"indexed": 1, "skipped": 1})
            output = io.StringIO()
            with redirect_stdout(output):
                self.assertEqual(main(common + ["draft", str(incoming), "--to", "boss@example.com",
                                                "--request", "확인했다고 답장"]), 0)
            self.assertEqual(len(json.loads(output.getvalue())["style_sources"]), 1)

    def test_cli_missing_credentials_has_no_database_side_effect(self):
        database = self.root / "missing.sqlite3"
        with patch.dict("os.environ", {}, clear=True), redirect_stderr(io.StringIO()) as error:
            result = main(["--backend", "openai", "--db", str(database), "--user", "me@example.com",
                           "ingest", str(self.root)])
        self.assertEqual(result, 1)
        self.assertIn("OPENAI_API_KEY", error.getvalue())
        self.assertFalse(database.exists())

    def test_cli_reads_session_mail_from_stdin_without_saving_it(self):
        path = self.mail(sender="boss@example.com", to="me@example.com", body="검토 일정 알려 주세요.")
        raw = path.read_bytes()
        path.unlink()
        database = self.root / "mail.sqlite3"
        llm = FakeLLM(UNKNOWN, {"subject": "검토 일정", "body": "금요일까지 전달드리겠습니다.",
                                "missing_facts": []})
        with io.TextIOWrapper(io.BytesIO(raw), encoding="utf-8") as stdin, \
                patch("sys.stdin", stdin), patch("mimicmail.__main__.Codex", return_value=llm), \
                redirect_stdout(io.StringIO()) as output:
            code = main(["--db", str(database), "--user", "me@example.com", "draft", "-",
                         "--to", "boss@example.com", "--request", "금요일까지 전달하겠다고 답장"])
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(output.getvalue())["body"], "금요일까지 전달드리겠습니다.")
        self.assertEqual(llm.calls[0][1]["conversation"]["body"], "검토 일정 알려 주세요.")
        self.assertEqual(list(self.root.iterdir()), [database])
        db = connect(database)
        self.addCleanup(db.close)
        self.assertEqual(db.execute("SELECT count(*) FROM sent_mail").fetchone()[0], 0)


class APITests(unittest.TestCase):
    def response(self, content, status="completed"):
        return {"status": status, "output": [{"type": "message", "content": content}]}

    def test_responses_http_contract_and_reasoning_output(self):
        response = self.response([{"type": "output_text", "text": '{"value":"ok"}'}])
        response["output"].insert(0, {"type": "reasoning", "summary": []})
        with patch("mimicmail.llm.urlopen", return_value=io.BytesIO(json.dumps(response).encode())) as call:
            result = OpenAI("test-key", "test-model").complete("instructions", {"facts": "text"},
                                                               object_schema({"value": {"type": "string"}}))
        request = call.call_args.args[0]
        payload = json.loads(request.data)
        self.assertEqual(request.full_url, "https://api.openai.com/v1/responses")
        self.assertEqual(request.get_header("Authorization"), "Bearer test-key")
        self.assertFalse(payload["store"])
        self.assertTrue(payload["text"]["format"]["strict"])
        self.assertEqual(result, {"value": "ok"})

    def test_refusal_incomplete_and_invalid_json_fail(self):
        cases = [self.response([{"type": "refusal", "refusal": "declined"}]),
                 self.response([], "incomplete"),
                 self.response([{"type": "output_text", "text": "not json"}])]
        for response in cases:
            with self.subTest(response=response), patch(
                "mimicmail.llm.urlopen", return_value=io.BytesIO(json.dumps(response).encode())
            ), self.assertRaises(RuntimeError):
                OpenAI("test-key", "test-model").complete("test", {}, {})

    def test_http_and_network_errors_are_actionable(self):
        errors = [HTTPError("https://api.openai.com", 401, "Unauthorized", {}, None),
                  URLError("offline"), TimeoutError()]
        for error in errors:
            with self.subTest(error=error), patch("mimicmail.llm.urlopen", side_effect=error), \
                    self.assertRaises(RuntimeError):
                OpenAI("test-key", "test-model").complete("test", {}, {})


if __name__ == "__main__":
    unittest.main()
