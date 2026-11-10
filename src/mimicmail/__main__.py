import argparse
from contextlib import closing
from datetime import date
import json
import os
from pathlib import Path
import sqlite3
import sys

from .llm import Codex, OpenAI
from .pipeline import PURPOSES, RELATIONSHIPS, connect, draft, ingest


def main(argv=None):
    parser = argparse.ArgumentParser(description="과거 발신 메일의 문체를 참고하는 이메일 초안 생성기")
    parser.add_argument("--db", default="data/mimicmail.sqlite3", help="SQLite 파일 경로")
    parser.add_argument("--user", help="사용자 기본 이메일 주소 (ingest/draft/context 필수)")
    parser.add_argument("--backend", choices=("codex", "openai"), default="codex",
                        help="분류·생성 엔진 (기본: codex)")
    parser.add_argument("--model", help="모델 ID (Codex는 생략 시 CLI 기본 설정 사용)")
    commands = parser.add_subparsers(dest="command", required=True)
    index = commands.add_parser("ingest", help="본인 발신 .eml을 분류하고 저장")
    index.add_argument("paths", nargs="+", type=Path, help=".eml 파일 또는 폴더")
    index.add_argument("--alias", action="append", default=[], help="추가 발신 주소 (반복 가능)")
    index.add_argument("--since", type=date.fromisoformat,
                       help="이 날짜의 한국 시간 00:00부터 포함 (YYYY-MM-DD)")
    generate = commands.add_parser("draft", help="현재 대화를 바탕으로 초안 생성")
    generate.add_argument("eml", type=Path, help=".eml 파일 경로 또는 표준 입력을 뜻하는 -")
    generate.add_argument("--to", required=True, help="초안의 수신자 이메일")
    generate.add_argument("--request", required=True, help="메일에 담을 내용과 작성 목적")
    generate.add_argument("--role", help="확인된 수신자 직책 (예: 팀장)")
    generate.add_argument("--relationship", choices=RELATIONSHIPS)
    generate.add_argument("--limit", type=int, default=5)
    context = commands.add_parser("context", help="DB에서 대화·직급·문체 예시를 읽기 전용으로 조회")
    context.add_argument("--to", required=True, help="수신자 이메일")
    context.add_argument("--purpose", choices=PURPOSES, default="other", help="작성할 메일의 목적")
    gui = commands.add_parser("gui", help="로컬 메일 보관함·편집기 열기")
    gui.add_argument("--port", type=int, default=8765)
    gui.add_argument("--no-open", action="store_true", help="브라우저를 자동으로 열지 않기")
    args = parser.parse_args(argv)
    try:
        if args.command == "gui":
            from .gui import serve
            serve(args.db, args.port, open_browser=not args.no_open)
            return 0
        if not args.user:
            raise ValueError("--user로 본인 이메일 주소를 지정하세요.")
        if args.command == "context":
            from .gui import draft_context
            with closing(sqlite3.connect(Path(args.db).resolve().as_uri() + "?mode=ro", uri=True)) as db:
                db.row_factory = sqlite3.Row
                result = draft_context(db, args.user, args.to, args.purpose)
            print(json.dumps(result, ensure_ascii=False, indent=2))
            return 0
        if args.command == "draft" and args.limit < 1:
            raise ValueError("--limit은 1 이상이어야 합니다.")
        if args.backend == "codex":
            llm = Codex(args.model or os.environ.get("CODEX_MODEL"))
        else:
            llm = OpenAI(os.environ.get("OPENAI_API_KEY"),
                         args.model or os.environ.get("OPENAI_MODEL"))
        if args.command == "ingest":
            paths = []
            for path in args.paths:
                if not path.exists():
                    raise ValueError(f"경로가 없습니다: {path}")
                paths.extend(sorted(item for item in path.rglob("*")
                                    if item.is_file() and item.suffix.lower() == ".eml")
                             if path.is_dir() else [path])
            if not paths:
                raise ValueError("색인할 .eml 파일이 없습니다.")
        with closing(connect(args.db)) as db:
            if args.command == "ingest":
                result = ingest(db, llm, paths, args.user, args.alias, since=args.since)
            else:
                source = sys.stdin.buffer.read() if args.eml == Path("-") else args.eml
                result = draft(db, llm, source, args.user, args.to, args.request,
                               args.role, args.relationship, args.limit)
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0
    except (OSError, ValueError, RuntimeError, sqlite3.Error) as error:
        print(f"오류: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
