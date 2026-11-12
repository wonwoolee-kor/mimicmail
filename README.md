# Mimicmail

과거 발신 메일의 문체로 초안을 작성합니다. Rust·SQLite 기반이며 메일 발송 기능은 없습니다.

## 실행

```sh
cargo build --release --locked
./target/release/mimicmail gui
```

macOS에서는 `Mimicmail.command`로 실행합니다. 주소는 `http://127.0.0.1:8765`, 종료는 Ctrl+C입니다. 색인·생성에는 로그인된 Codex CLI가 필요합니다.

```sh
./target/release/mimicmail --user me@example.com ingest ./history
./target/release/mimicmail --user me@example.com draft current.eml \
  --to recipient@example.com --request '작성할 내용'
```

## 코드 구조

| 경로 | 역할 |
| --- | --- |
| `src/main.rs`, `src/config.rs` | 명령 실행, CLI·환경변수·`.env` 설정 |
| `src/mail.rs`, `src/context.rs` | MIME 해석, 인용문 추출, 대화 순서 구성 |
| `src/archive.rs`, `src/db.rs` | 메일·대화 조회와 수정, DB 연결·마이그레이션 |
| `src/mailbox.rs`, `src/search.rs` | 조회 캐시·페이지 처리, Unicode 검색 |
| `src/settings.rs` | DB에 저장하는 General context |
| `src/drafting.rs`, `src/pipeline.rs`, `src/llm.rs` | 사실·문체 후보 선택, 색인·초안 생성, 모델 실행 |
| `src/privacy.rs`, `src/work.rs` | 개인정보 치환, 요청 취소·시간 제한 |
| `src/server.rs`, `assets/` | 로컬 HTTP 서버와 브라우저 화면 |
| `sql/`, `prompts/` | SQL 스키마, 모델 지시문 |
| `tests/`, `scripts/` | 회귀 테스트, 자원 사용량 비교 |

CLI와 HTTP 서버는 같은 보관함·문맥 코드를 호출합니다. 보관함 원본 테이블을 기준으로 조회 캐시를 재구성하며, 문체 편집·삭제·충돌 검사에는 트랜잭션을 사용합니다.

## 화면

- **설정 → General context**: 모든 대화의 공통 정보.
- **+ 새 대화**: 이름으로 시작. 직급은 문체 선택에 사용.
- **프롬프트 복사**: Codex에 붙여 넣어 초안 작성.
- **보낸 메일 / 상대 답장**: 본문 저장. 저장 전 입력은 페이지를 떠나면 사라짐.
- **연필**: 문체 참조 편집. **휴지통**: 메시지를 대화·참조에서 제외.
- **우클릭 / ⋯ → 대화 삭제**: 대화·답장·문체 자료·DB 보관 원문·검색 캐시를 삭제합니다. 다른 대화도 참조하는 원문은 마지막 연결을 삭제할 때 지웁니다. 예전에 삭제한 대화의 잔여 데이터도 다음 실행 때 정리합니다. 재색인을 막기 위한 식별자만 남기며, 가져온 원본 `.eml` 파일은 건드리지 않습니다.

## Codex 조회

프로젝트 폴더에서 프롬프트의 `owner`, `recipient`를 사용합니다. 기본 DB는 `data/mimicmail.sqlite3`; 별도 DB는 하위 명령 앞에 `--db PATH`를 지정합니다.

```sh
./target/release/mimicmail --user OWNER context --to RECIPIENT
```

결과의 `selection_instructions`, `instructions`를 따릅니다. 필요한 본문만 `--history-key KEY`, `--style-id ID`로 추가 조회합니다. 옵션은 반복할 수 있습니다. 과거 사실은 동일 인물의 관련 요청에만, 다른 사람의 메일은 문체에만 사용합니다. 현재 요청이 공통 정보보다 우선합니다.

질문·입력 대기 없이 작성합니다. 불필요한 미확인 내용은 생략하고 필수 누락만 `[확인 필요: 항목]`으로 표시합니다.

메일 본문을 로컬 DB에 저장하고 색인·생성 시 모델에 전달합니다. 이메일 주소는 치환하지만 원본 `.eml`은 그대로 둡니다. 로컬 자료·비밀값·빌드 결과는 Git에서 제외합니다.

## 검증

```sh
cargo test --locked
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
node --test tests/*.mjs
```
