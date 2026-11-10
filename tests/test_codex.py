from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from mimicmail.__main__ import main
from mimicmail.llm import Codex, object_schema


class CodexTests(unittest.TestCase):
    def setUp(self):
        executable = patch("mimicmail.llm.shutil.which", return_value="/test/codex")
        executable.start()
        self.addCleanup(executable.stop)

    def test_structured_output_stdin_and_temporary_files(self):
        schema = object_schema({"value": {"type": "string"}})
        observed = {}

        def run(command, **kwargs):
            root = kwargs["cwd"]
            observed["root"] = root
            self.assertEqual(json.loads((root / "schema.json").read_text()), schema)
            self.assertEqual(json.loads(kwargs["input"]), {"메일": "본문"})
            self.assertNotIn("본문", " ".join(command))
            self.assertEqual(command[command.index("--sandbox") + 1], "read-only")
            self.assertEqual(command[command.index("--model") + 1], "chosen-model")
            self.assertIn("--ephemeral", command)
            (root / "result.json").write_text('{"value":"완료"}', encoding="utf-8")
            return subprocess.CompletedProcess(command, 0)

        with patch("mimicmail.llm.subprocess.run", side_effect=run):
            result = Codex("chosen-model").complete("분류", {"메일": "본문"}, schema)
        self.assertEqual(result, {"value": "완료"})
        self.assertFalse(observed["root"].exists())

    def test_default_model_uses_cli_configuration(self):
        def run(command, **kwargs):
            self.assertNotIn("--model", command)
            (kwargs["cwd"] / "result.json").write_text("{}")
            return subprocess.CompletedProcess(command, 0)

        with patch("mimicmail.llm.subprocess.run", side_effect=run):
            self.assertEqual(Codex().complete("분류", {}, {}), {})

    def test_missing_cli(self):
        with patch("mimicmail.llm.shutil.which", return_value=None), self.assertRaises(ValueError):
            Codex()

    def test_process_failure_and_timeout(self):
        with patch("mimicmail.llm.subprocess.run", return_value=subprocess.CompletedProcess([], 1)):
            with self.assertRaisesRegex(RuntimeError, "Codex 실행 실패"):
                Codex().complete("test", {}, {})
        with patch("mimicmail.llm.subprocess.run", side_effect=subprocess.TimeoutExpired("codex", 180)):
            with self.assertRaisesRegex(RuntimeError, "180초"):
                Codex().complete("test", {}, {})

    def test_missing_invalid_and_non_object_output(self):
        for output in (None, "not JSON", "[]"):
            def run(command, **kwargs):
                if output is not None:
                    (kwargs["cwd"] / "result.json").write_text(output)
                return subprocess.CompletedProcess(command, 0)

            with self.subTest(output=output), patch("mimicmail.llm.subprocess.run", side_effect=run):
                with self.assertRaises(RuntimeError):
                    Codex().complete("test", {}, {})

    def test_cli_defaults_to_codex_without_api_key(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sample.eml"
            path.write_text("From: me@example.com\nTo: boss@example.com\nSubject: Hello\n\nHello")
            with patch.dict("os.environ", {}, clear=True), \
                    patch("mimicmail.__main__.Codex") as client, \
                    patch("mimicmail.__main__.OpenAI") as api, \
                    patch("mimicmail.__main__.ingest", return_value={"indexed": 1, "skipped": 0}), \
                    redirect_stdout(io.StringIO()):
                code = main(["--db", str(Path(directory) / "mail.sqlite3"), "--user", "me@example.com",
                             "ingest", str(path)])
            self.assertEqual(code, 0)
            client.assert_called_once_with(None)
            api.assert_not_called()


if __name__ == "__main__":
    unittest.main()
