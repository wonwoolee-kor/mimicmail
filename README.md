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

## 설정

설정 파일은 선택 사항입니다. 프로젝트 폴더에서 `.env.example`을 `.env`로 복사한 뒤 필요한 값을 입력합니다.

```sh
cp .env.example .env
```

우선순위는 **CLI 옵션 → 실행 환경변수 → 현재 작업 폴더의 `.env` → 기본값**입니다. 상위 폴더의 `.env`는 읽지 않습니다. 상대 경로도 현재 작업 폴더를 기준으로 해석합니다. `Mimicmail.command`는 프로젝트 폴더에서 실행합니다.

| 환경변수 | CLI 옵션 | 기본값·용도 |
| --- | --- | --- |
| `MIMICMAIL_DB` | `--db` | `data/mimicmail.sqlite3` |
| `MIMICMAIL_USER` | `--user` | 색인·초안·문맥 조회의 본인 주소 또는 소유자 ID |
| `MIMICMAIL_BACKEND` | `--backend` | `codex`; API를 사용하려면 `openai` |
| `MIMICMAIL_PORT` | `gui --port` | `8765`; `0`이면 빈 포트 사용 |
| `CODEX_MODEL` | `--model` | 생략하면 Codex CLI의 기본 모델 사용 |
| `OPENAI_MODEL` | `--model` | OpenAI API 사용 시 필수 |
| `OPENAI_API_KEY` | 없음 | OpenAI API 사용 시 필수 |

Gmail 자동 수집을 끈 GUI와 `context` 명령은 모델 로그인이나 API 키 없이 동작합니다. `.env`의 값은 프로그램 설정으로만 읽고 프로세스 환경변수에 복사하지 않습니다. `.env`와 로컬 데이터는 Git에서 제외하며, 비밀값이 없는 `.env.example`만 버전 관리합니다. OpenAI API를 선택하면 API 사용 요금을 별도로 부담합니다.

## Gmail 수집

`wonwoolee.fwd@gmail.com` 하나만 연결합니다. Dooray·다른 Gmail 계정의 자동 전달은 각 서비스에서 설정합니다. 앱은 전용 계정의 전체 메일함을 읽으며, Gmail의 읽음 표시·라벨·삭제·발송을 변경하지 않습니다. 받은편지함이나 `To` 헤더로 수집 범위를 좁히지 않으므로 보관 처리한 메일과 원래 수신자를 유지하는 자동 전달 메일도 가져옵니다.

### 최초 OAuth 설정

1. [Google Cloud Console](https://console.cloud.google.com/apis/library/gmail.googleapis.com)에서 프로젝트를 만들고 **Gmail API**를 활성화합니다.
2. **Google Auth Platform**에서 앱 이름·지원 이메일을 설정합니다. 개인 Gmail 계정은 **External**로 설정하고, Testing 상태라면 테스트 사용자에 `wonwoolee.fwd@gmail.com`을 추가합니다.
3. **Data Access**에는 `https://www.googleapis.com/auth/gmail.readonly`만 추가합니다. **Clients → Create client → Desktop app**에서 클라이언트를 만듭니다. 앱은 임시 포트의 `127.0.0.1` 콜백과 PKCE를 사용합니다. [Google 데스크톱 OAuth 안내](https://developers.google.com/identity/protocols/oauth2/native-app)
4. `.env`의 `GMAIL_CLIENT_ID`, `GMAIL_CLIENT_SECRET`에 발급한 값을 입력합니다. 기존 자료와 통합하려면 `MIMICMAIL_USER`에 기존 색인에 사용한 본인 주소를 넣고, 추가 본인 주소는 `MIMICMAIL_ALIASES`에 쉼표로 구분합니다. 새 DB에서 소유자를 생략하면 전용 계정 주소를 사용합니다. 처음 동기화한 DB의 소유자를 바꾸려면 별도 DB를 지정합니다.
5. 아래 명령을 실행하고 브라우저에서 **전용 계정**으로 승인합니다. 다른 계정이나 읽기 전용 범위를 벗어난 권한은 거부합니다.

```sh
cargo build --release --locked
./target/release/mimicmail gmail connect
./target/release/mimicmail gmail sync
./target/release/mimicmail gmail status
```

`gmail connect --no-open`은 승인 URL만 출력합니다. URL은 앱을 실행한 같은 컴퓨터의 브라우저에서 열며, 3분 안에 승인합니다. `connect`와 `status`는 모델을 호출하지 않습니다. `sync`는 저장 후 기존 Codex/OpenAI 분류기를 자동으로 호출하므로 선택한 모델의 로그인을 준비합니다.

External/Testing 앱의 Gmail refresh token은 7일 후 만료합니다. 계속 운영하려면 게시 상태와 적용할 검증 요건을 확인하고, 토큰을 해제하거나 만료하면 `gmail connect`로 다시 승인합니다. `gmail.readonly`는 제한된 범위(restricted scope)입니다. [토큰 만료 조건](https://developers.google.com/identity/protocols/oauth2#expiration), [Gmail 권한·검증 안내](https://developers.google.com/workspace/gmail/api/auth/scopes)

### 자동 수집과 설정

| 환경변수 | 기본값·용도 |
| --- | --- |
| `GMAIL_ENABLED` | `false`; `true`이면 GUI 실행 중 수집·분석 |
| `GMAIL_POLL_SECONDS` | `120`; 수집 완료 후 다음 실행까지 대기, 30~86400초 |
| `GMAIL_CLIENT_ID` | Desktop app OAuth 클라이언트 ID |
| `GMAIL_CLIENT_SECRET` | 해당 클라이언트 secret |
| `MIMICMAIL_ALIASES` | 추가 본인 발신 주소, 쉼표로 구분 |

연결을 마친 뒤 `.env`에서 `GMAIL_ENABLED=true`로 바꾸고 평소처럼 `gui`나 `Mimicmail.command`를 실행합니다. GUI와 별도로 계속 수집하려면 다음 명령을 실행합니다. 두 방식 모두 실행 중에만 수집하며 별도의 시작 프로그램을 설치하지 않습니다.

```sh
./target/release/mimicmail gmail watch
```

Ctrl+C로 종료합니다. GUI 종료 시 함께 시작한 수집 작업도 정리합니다. 진행 중인 HTTP/모델 호출의 제한 시간까지 기다릴 수 있습니다(HTTP 30초, Codex 기본 180초, OpenAI 120초). 동일 DB에서 수집 작업이 겹치면 파일 잠금으로 중복 실행을 막습니다. 수집 이후 화면을 새로고침하면 새 메일을 확인할 수 있습니다.

### 데이터 처리와 복구

- 처음에는 전체 페이지를 수집하고, 이후 `historyId`로 추가 메일을 찾습니다. 기록이 만료해 404를 받으면 전체 조회로 복구합니다. 커서는 메일을 로컬 대기열에 저장한 뒤 이동합니다. [Gmail 동기화 안내](https://developers.google.com/workspace/gmail/api/guides/sync)
- Gmail 메시지 ID로 재수집을 방지합니다. 전달 경로가 달라도 Message-ID와 발신자·수신자·제목·날짜·본문이 모두 같으면 중복 분석을 막습니다. Message-ID만 같은 서로 다른 내용은 각각 보관합니다.
- 수신 메일은 분류·검색·대화 사실에 사용하고 문체 예시에서 제외합니다. `MIMICMAIL_USER`와 별칭의 발신 메일만 기존 발신 파이프라인으로 보냅니다. 자동 전달로 도착하지 않은 과거 메일·보낸메일은 별도 `.eml` 색인이 필요합니다. 원래 서비스·계정을 추측하지 않습니다.
- SPAM·TRASH·DRAFT는 식별자만 기록하고 분석에서 제외합니다. 해당 라벨을 제거하면 증분 수집에서 다시 확인합니다. Gmail에서 나중에 삭제하거나 라벨을 바꿔도 이미 보관한 메일은 자동 삭제하지 않습니다. 앱에서 대화를 삭제하면 관련 원문·대기열 본문·검색 자료를 지우고 재수집을 막습니다.
- 텍스트/HTML 본문과 기존 인용문 파서를 사용합니다. 첨부파일과 첨부 `.eml` 분석은 지원하지 않습니다. 해석할 수 없는 메일은 `failed/mime`으로 기록하고 다음 메일을 처리합니다. 분석 오류는 본문을 대기열에 남겨 1분~1시간 간격으로 재시도합니다. 한 번에 최대 100건을 분석하고 `watch`가 남은 건을 이어 처리합니다.
- `gmail status`는 수집 위치, 상태별 건수, 오류가 있는 메시지 ID를 로컬 DB에서 조회합니다. 오류 원인을 해결한 뒤 `gmail sync --retry-failed`로 MIME 오류 메일을 다시 가져오고 분석 대기 시간을 초기화할 수 있습니다. 모델 응답의 오류 원문은 저장하지 않습니다.

OAuth 토큰은 macOS 키체인의 `mimicmail.gmail` / `wonwoolee.fwd@gmail.com` 항목에 저장합니다. 다른 OS에서는 DB와 같은 경로의 `*.gmail-token.json`을 사용하며 Unix에서는 권한 600으로 생성하고 검사합니다. 토큰·client secret을 DB·로그·모델 입력에 넣지 않습니다. `.env`의 권한도 `chmod 600 .env`로 제한하세요. Windows에서 파일 저장 방식을 사용한다면 사용자 계정만 읽도록 별도 ACL을 설정해야 합니다.

Gmail 원본 MIME은 메모리에서 해석하고 파일로 보관하지 않습니다. 대기열·보관함·Gmail 분석 입력에는 기존 주소 치환 규칙을 적용하며, 분석을 마치면 대기열의 본문 사본을 지웁니다. 주소 이외의 본문 내용은 남고 설정한 모델에 전달합니다. 수집을 중지하려면 `GMAIL_ENABLED=false`로 바꾸고 실행 중인 `watch`를 종료합니다. 접근 권한까지 해제하려면 [Google 계정의 앱 연결](https://myaccount.google.com/connections)에서 해당 OAuth 앱을 삭제합니다.

## 코드 구조

| 경로 | 역할 |
| --- | --- |
| `src/main.rs`, `src/config.rs` | 명령 실행, CLI·환경변수·`.env` 설정 |
| `src/gmail/` | 전용 Gmail OAuth, 증분 수집, 분석 대기열·재시도 |
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
