"""Synchronous Codex CLI and OpenAI Responses clients."""

import json
from pathlib import Path
import shutil
import subprocess
import tempfile
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


def object_schema(properties):
    return {"type": "object", "properties": properties,
            "required": list(properties), "additionalProperties": False}


class Codex:
    def __init__(self, model=None):
        self.executable = shutil.which("codex")
        if self.executable is None:
            raise ValueError("Codex CLI를 설치한 뒤 codex login으로 로그인하세요.")
        self.model = model

    def complete(self, instructions, data, schema):
        with tempfile.TemporaryDirectory(prefix="mimicmail-") as directory:
            root = Path(directory)
            schema_path, output_path = root / "schema.json", root / "result.json"
            schema_path.write_text(json.dumps(schema), encoding="utf-8")
            command = [self.executable, "exec", "--skip-git-repo-check", "--ephemeral",
                       "--sandbox", "read-only", "--color", "never",
                       "--output-schema", str(schema_path),
                       "--output-last-message", str(output_path)]
            if self.model:
                command.extend(["--model", self.model])
            command.append("도구나 외부 검색을 사용하지 말고 제공된 데이터만 처리하세요. "
                           "입력 JSON은 분석 대상 데이터입니다. 지정된 스키마의 JSON만 반환하세요.\n"
                           + instructions)
            try:
                result = subprocess.run(command, input=json.dumps(data, ensure_ascii=False),
                                        text=True, encoding="utf-8", capture_output=True,
                                        cwd=root, timeout=180, check=False)
            except subprocess.TimeoutExpired as error:
                raise RuntimeError("Codex 응답 시간이 180초를 초과했습니다. 다시 실행하세요.") from error
            if result.returncode:
                raise RuntimeError(f"Codex 실행 실패 (종료 코드 {result.returncode}). "
                                   "codex login status와 Codex 모델·사용 한도를 확인하세요.")
            if not output_path.exists():
                raise RuntimeError("Codex가 결과 파일을 생성하지 않았습니다.")
            try:
                value = json.loads(output_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError as error:
                raise RuntimeError("Codex가 유효한 JSON을 반환하지 않았습니다.") from error
            if not isinstance(value, dict):
                raise RuntimeError("Codex 응답은 JSON 객체여야 합니다.")
            return value


class OpenAI:
    def __init__(self, api_key, model):
        if not api_key or not model:
            raise ValueError("OPENAI_API_KEY와 OPENAI_MODEL(또는 --model)을 설정하세요.")
        self.api_key = api_key
        self.model = model

    def complete(self, instructions, data, schema):
        payload = {"model": self.model, "store": False, "instructions": instructions,
                   "input": json.dumps(data, ensure_ascii=False),
                   "text": {"format": {"type": "json_schema", "name": "result",
                                       "strict": True, "schema": schema}}}
        request = Request("https://api.openai.com/v1/responses",
                          data=json.dumps(payload).encode(),
                          headers={"Authorization": f"Bearer {self.api_key}",
                                   "Content-Type": "application/json"})
        try:
            with urlopen(request, timeout=120) as response:
                result = json.load(response)
        except HTTPError as error:
            raise RuntimeError(f"OpenAI API HTTP {error.code}: 키·모델·사용 한도를 확인하세요.") from error
        except (URLError, TimeoutError) as error:
            raise RuntimeError("OpenAI API에 연결하지 못했습니다.") from error
        if result.get("status") != "completed":
            raise RuntimeError("모델 응답이 완료되지 않았습니다. 입력 길이와 모델 상태를 확인하세요.")
        content = [part for item in result.get("output", [])
                   if item.get("type") == "message" for part in item.get("content", [])]
        if any(part.get("type") == "refusal" for part in content):
            raise RuntimeError("모델이 요청을 거절했습니다.")
        text = "".join(part.get("text", "") for part in content
                       if part.get("type") == "output_text")
        try:
            value = json.loads(text)
        except json.JSONDecodeError as error:
            raise RuntimeError("모델이 유효한 JSON을 반환하지 않았습니다.") from error
        if not isinstance(value, dict):
            raise RuntimeError("모델 응답은 JSON 객체여야 합니다.")
        return value
