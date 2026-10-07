# 03 · Capture

기록이 최우선. 이 문서는 **인제스트 API 계약 + 소스별 어댑터**를 정의한다.
소스 포맷은 실제 파일을 검사해 **검증 완료**(2026-07, macOS).

## 인제스트 API 계약

Rust axum 서버, `127.0.0.1:<port>`에만 바인딩(외부 접근 불가).

**디스커버리 & 인증**
- 앱 첫 실행 시 랜덤 포트 + 32바이트 토큰 생성.
- `~/.logroom/ingest.json` (perm 0600)에 기록: `{ "port": 47xxx, "token": "…" }` — 캡처러가 읽음.
- 토큰은 OS 키체인에도 보관(진실소스). 모든 요청 헤더: `X-LogRoom-Token: <token>`.
- **싱글 인스턴스**: Tauri single-instance 플러그인으로 앱 1개만 실행(포트/DB 라이터 충돌 방지). 두 번째 실행은 기존 창 포커스.
- **포트 재사용**: 재시작 시 이전 포트 우선 바인딩, 사용 중이면 새 포트 발급 후 `ingest.json` 갱신. 캡처러는 매 요청 전 파일 재확인.

**엔드포인트**
| 메서드 | 경로 | 설명 |
|--------|------|------|
| GET | `/v1/health` | `{ ok: true, version }` |
| POST | `/v1/ingest` | 이벤트 1건 또는 배치 수신 |

**POST /v1/ingest 바디** (zod 스키마는 `packages/core`)
```jsonc
{
  "stream": {                       // 선택: 있으면 upsert
    "id": "claude_code:<sessionId>",
    "source": "claude_code",
    "title": "…", "project": "/Users/…/logroom", "gitBranch": "main"
  },
  "events": [{
    "externalId": "<native uuid>",  // dedup 키 (필수)
    "streamId": "claude_code:<sessionId>",
    "ts": 1730000000000,            // epoch ms
    "source": "claude_code",
    "type": "prompt",               // prompt|response|tool_use|tool_result|message|file_edit|note
    "title": "…", "body": "…",
    "model": "claude-sonnet-4-5", "tokensIn": 1200, "tokensOut": 340,
    "url": null, "parentId": null, "metadata": {}
  }]
}
```
- 서버: stream upsert(없으면 생성) → events는 `(source, externalId)` UNIQUE로 idempotent 삽입 → `ended_at` 갱신 → `event-ingested` emit.

## 캡처 실행 모델

- **파일 감시형**(Claude Code JSONL, Kiro): Rust `notify`가 앱 실행 중 상시 감시. 앱 꺼진 동안 로그는 재실행 시 `capture_cursors`(offset/mtime)로 **backfill**.
- **push형**(Claude Code hook, 브라우저 확장): 발생 즉시 POST.
- Claude Code는 **hook=라이브 신호(비저장) + JSONL=저장 진실소스**로 역할 분리 → 중복 방지(아래 "중복 처리" 규칙, ADR-0010).

**감시 root 해석 (`src/capture/config.rs`)** — Claude Code는 `CLAUDE_CONFIG_DIR` 환경변수로 `~/.claude-a`, `~/.claude-b` 같은 커스텀 설정 디렉토리를 쓸 수 있다(실측: `~/.claude` 하나만 감시하면 실사용 데이터 대부분 누락).
- **자동 탐지**: `$HOME` 바로 아래에서 `.claude*` 패턴 디렉토리(`.claude`, `.claude-a`, `.claude-b` 등)를 찾아 그중 `projects/` 하위 디렉토리가 있는 것만 root로 추가한다. `.claude.json` 같은 **파일**은 `is_dir()` 필터로 제외된다.
- **설정 파일**(선택): `~/.logroom/config.json`. 없으면 자동 탐지 + 기존 기본 경로(Claude 자동탐지 + `~/.kiro/sessions/cli`)만으로 동작(회귀 없음). 파싱 실패 시 `eprintln!` 경고 후 기본 동작으로 폴백(크래시 금지).
  ```jsonc
  { "capture": {
      "claudeCode": { "extraRoots": ["<projects 디렉토리 절대경로>"], "useDefaults": true },
      "kiroCli":    { "extraRoots": [], "useDefaults": true },
      "bodyPolicy": "essential",
      "scrubSecrets": true } }
  ```
  - `useDefaults: false`로 자동 탐지/기본 경로를 끄고 `extraRoots`만 쓸 수 있다.
  - `extraRoots`는 존재하는 경로만 추가(없으면 경고 후 스킵), 자동 탐지 root와 중복되면(`canonicalize` 기준) 제거된다.
  - `bodyPolicy`(`"essential"`(기본) | `"full"`): tool 계열 body/metadata 절단 여부(ADR-0012). FE Settings UI
    노출은 후속 — 현재는 이 파일을 직접 편집해야 한다.
  - `scrubSecrets`(기본 **true**): 시크릿 스크럽(M4) 사용 여부. `false`면 ingest 직전 마스킹을 건너뛴다
    (`src/capture/scrub.rs`, 04-privacy-security.md 참고).
- Claude root가 여러 개여도 `stream_id`는 `sessionId` 기반, cursor는 파일 절대경로 기반이라 소스 판별·중복 방지에 영향 없다.

---

## 소스 1 · Claude Code ✅ 검증됨 (2026-07, 실샘플 94줄 분석)

**경로**: `~/.claude*/projects/<encoded-cwd>/<sessionId>.jsonl` (한 줄 = 1 JSON, JSONL). `~/.claude*`는 기본 `~/.claude` + `CLAUDE_CONFIG_DIR` 커스텀 경로(`~/.claude-a`, `~/.claude-b` 등) — root 자동 탐지 규칙은 위 "캡처 실행 모델" 참고.
**서브에이전트(Task)**: `~/.claude*/projects/<encoded-cwd>/<sessionId>/subagents/agent-<id>.jsonl` — **별도 파일**, 각 라인 `isSidechain:true`, `sessionId`는 부모와 동일.
**hook**: `~/.claude*/settings.json`에 `UserPromptSubmit / PreToolUse / PostToolUse / Stop / SessionStart` 활성.

**공통 라인 키(실측)**: `type, message, uuid, parentUuid, sessionId, cwd, gitBranch, timestamp, isSidechain, promptId(user), requestId(assistant), userType, version, entrypoint, permissionMode`.

> **핵심: 1 JSONL 라인 ≠ 1 이벤트.** `message.content[]`가 여러 블록이며 **블록 단위로 이벤트를 생성**한다(실측: 한 assistant 라인에 text/thinking/tool_use 혼재).

**JSONL 라인 타입(실측):**
| type | content 블록 / 키 | → LogRoom 이벤트 |
|------|---------|-------------|
| `user` | `content[]`: `text`, `tool_result{tool_use_id,content}` | `text`→**prompt**(단, 메타/중단 마커면 **skip** — 아래 참고) · `tool_result`→**tool_result** |
| `assistant` | `content[]`: `text`, `thinking`, `tool_use{id,name,input}`; `message.{model,usage,stop_reason}` | `text`→**response** · `tool_use`→**tool_use**(title=name, input→metadata) · `thinking`→**skip**(기본) |
| `ai-title` | `aiTitle`, `sessionId` | → **stream.title** (이벤트 아님) |
| `last-prompt` | `lastPrompt`, `sessionId` | skip (prompt 보정용) |
| `queue-operation` / `attachment` / `file-history-snapshot` / `mode` | — | **skip** (필요 시 metadata) |
| `summary` (일부 세션) | 세션 요약 | stream.metadata (후속) |

**스트림 그룹핑**
- 메인: `stream.id = "claude_code:" + sessionId`, `kind = session`.
- **서브에이전트**: `stream.id = "claude_code:" + sessionId + ":agent:" + agentId`, `kind = agent`, `metadata.parentStream = "claude_code:" + sessionId`. → **병렬 레인으로 분리 표시**(제품 정체성; sessionId로 합치면 병렬성 소실).
- `project = cwd`(레포 루트 정규화, 02), `gitBranch`, `title = aiTitle`(없으면 첫 prompt 60자).
- **hub 세션**(cwd 가 레포 밖 — 중간 에이전트·`~/git` 같은 상위 폴더에서 띄운 세션, `capture/hub.rs`): `metadata.hub = true`(+`entrypoint`, 예 `sdk-ts`)를 붙이고 화면에 "에이전트 경유"로 표시한다. 인제스트 뒤 base 스트림 이벤트를 **턴**(prompt 하나부터 다음 prompt 전까지)으로 나눠, tool_use 의 경로 신호(Bash `cd <dir>`·`git -C <dir>`, 절대 `file_path`, Grep/Glob `path`)를 레포 루트로 정규화해 다수결(동점은 먼저 나온 레포)한 뒤 자식 스트림 `<base id>@<레포 루트>` 로 옮긴다. 신호 없는 턴은 base 에 남는다. 진행 중 턴의 뒷조각이 오면 이미 옮긴 같은 턴 이벤트와 합쳐 다시 투표한다(라이브와 일괄 결과가 같다). **제외 프로젝트 표가 하나라도 있는 턴은 통째로 지운다**(뒤늦게 오는 응답도 `capture_cursors(source='hub_excluded_turn')` 로 지움). 서브에이전트 스트림은 턴 분할 없이 스트림 전체 투표로 `project` 만 바꾼다. 기존 데이터는 앱 시작 때 한 번 소급한다(`capture_cursors('hub_reattribute','v1')` 마커, 실패한 스트림이 있으면 다음 기동에 재시도).

**이벤트 매핑 규칙**
- `ts = timestamp(ISO8601 → epoch ms)`, `parentId = parentUuid`(라인 레벨 스레드 체인).
- **externalId(블록 단위, dedup 안정)**: `tool_use`는 `tool_use.id`(예: `toolu_…`), `tool_result`는 **`<tool_use_id>#result`**(tool_use와 동일 id라 suffix 없으면 `(source, external_id)` UNIQUE 충돌 → tool_result 전량 유실. 2026-07-02 실측으로 발견·수정), `text`/`thinking`은 `"<라인 uuid>#<블록 index>"`.
- 토큰: `usage.input_tokens → tokensIn`, `output_tokens → tokensOut`, `cache_creation_input_tokens`/`cache_read_input_tokens` → metadata.
- `thinking`은 기본 저장 안 함(검색 노이즈·프라이버시). 포함 여부는 후속 설정 옵션.

**시스템 노이즈 prompt 필터(2026-07 도입)** — 슬래시 커맨드 메타/중단 마커가 그대로 prompt로 저장되며
검색 노이즈가 되던 문제(실측: prompt 4,460건 중 ~640건)를 캡처 단계에서 원천 차단한다.
- **라인 레벨**: `isMeta == true`(Claude Code 원본 공식 마커)면 라인 전체 skip.
- **텍스트 블록 레벨**: trim한 텍스트가 아래 프리픽스로 **시작**하면 해당 블록만 skip
  (`src/capture/policy.rs::is_meta_prompt_text`):
  - `<command-name>` / `<command-message>` / `<command-args>`(슬래시 커맨드 실행 echo)
  - `<local-command-stdout>` / `<local-command-caveat>`(로컬 커맨드 결과·안내 문구)
  - `[Request interrupted by user` (`]`, ` for tool use]` 계열 모두 포함)
  - **(V4 추가, 2026-07)** 시스템 주입 prompt 변종: `<task-notification>`(백그라운드 에이전트 완료
    알림이 prompt로 저장되던 것) / `<teammate-message`(속성이 이어져 `>` 없이 prefix) /
    `<fork-boilerplate>` / `[structured-output-enforce]` / `[SYSTEM NOTIFICATION`
    (` - NOT USER INPUT]` 계열) / `<ide_opened_file>`
  - `[Image:`로 시작하는 텍스트는 **사용자가 첨부한 이미지 프롬프트**이므로 필터 대상이 **아니다**(유지).
- `tool_result` 블록은 이 필터의 영향을 받지 않는다.
- **기존 데이터 소급 정리**: `migrations/V3__prune_meta_prompts.sql`(슬래시 커맨드 메타/중단 마커)과
  `migrations/V4__prune_injected_prompts.sql`(시스템 주입 prompt 변종, 실측 `<task-notification>`
  724건 포함 — 그중 3건은 title까지 오염)이 각각 같은 패턴 집합으로
  ①노이즈 prompt 이벤트 삭제 → ②노이즈 title을 남은 첫 prompt 앞 60자로 갱신(없으면 NULL) →
  ③이벤트 0개가 된 스트림 삭제 → ④남은 스트림 시간범위 재계산 순으로 처리한다.
  `<ide_opened_file>`은 LIKE 와일드카드 `_`를 포함해 `ESCAPE '\'` 처리가 필요하다(V4).

**중복 처리 (hook ↔ JSONL) — 핵심 규칙 (ADR-0010)**
- **저장 진실소스 = JSONL 파서(Rust 감시자)만.** 모든 prompt/response/tool 이벤트는 여기서 생성(`external_id = uuid`).
- **hook = 라이브 신호 전용.** UserPromptSubmit/Stop에서 `POST /v1/ingest`로 **스트림 활성 표시(ended_at 갱신 + "지금 활동중")만** 보내고 **영구 이벤트로 저장하지 않는다** → `promptId`↔`uuid` 키 불일치로 인한 이중 기록 없음.
- 근거: hook은 저지연 UX(실시간 반영), JSONL은 완전·정규화(토큰/모델/툴). 저장 경로가 하나뿐이라 dedup 문제 원천 제거.
- (대안) hook도 저장하려면 `promptId`로 키를 통일하고 JSONL 파서가 upsert-merge로 완전정보 덮어쓰기. 기본은 "JSONL=진실소스, hook=신호" 채택.

---

## 소스 2 · Kiro CLI ✅ 검증됨 (2026-07-02, 직접 실측 재검증 — M2 CLI 완료)

**경로**: `~/.kiro/sessions/cli/<sessionId>.{json,jsonl,history,lock}` (flat 디렉토리) + `~/Library/Application Support/kiro-cli/data.sqlite3`

- `<sessionId>.json` = **세션 메타**(jsonl 처리 시마다 재로드, 감시 대상 아님): `{session_id, cwd, created_at, updated_at, title, parent_session_id, session_created_reason, session_state}` → **stream** 필드.
  - **`created_at`/`updated_at`은 ISO8601 문자열**(예 `2026-06-30T02:28:30.686993Z`) — 이전 기록의 "이미 ms"는 오기이며 **정정**.
- `<sessionId>.jsonl` = **이벤트 스트림**(감시 대상): 한 줄 = `{kind, data, version}`. **라인 레벨 timestamp 없음** → 아래 "ts 보간" 참고.
- `.history`/`.lock`은 감시 제외(확장자 `.jsonl`만 처리).
- `data.sqlite3`의 `conversations_v2` 대화 백필은 스키마는 검증됐으나(95건) **M2 범위 밖 후속 작업으로 격하**. 이번 M2는 `.jsonl` 감시+backfill만 구현.

**JSONL 라인 kind(실측, 4종) + 중첩 content 블록 구조:**
| kind | `data` 구조 | → LogRoom 이벤트 |
|------|-----------|-------------|
| `Prompt` | `{message_id, content:[{kind:"text", data:<string>}], meta:{timestamp:<epoch 초\|null>}}` (`meta` 자체가 null일 수 있음) | `text` 블록 → **prompt** |
| `AssistantMessage` | `{message_id, content:[{kind:"text"\|"thinking"\|"toolUse", data}]}` — text/thinking의 `data`=문자열, toolUse의 `data`=`{toolUseId, name, input:object}` | `text`→**response** · `toolUse`→**tool_use**(title=name, input→metadata) · `thinking`→**skip**(기본, Claude와 동일 정책) |
| `ToolResults` | `{message_id, content:[{kind:"toolResult", data:{toolUseId, content:[…], status}}], results:{…}}` | `toolResult` 블록 → **tool_result** |
| `Compaction` | — | **skip** |

**externalId 규칙(블록 단위, dedup 안정)**
| 라인.블록 | externalId |
|---|---|
| Prompt text | **`prompt:<message_id>#<블록 index>`** |
| AssistantMessage text | **`response:<message_id>#<블록 index>`** |
| AssistantMessage toolUse | `toolUseId` |
| ToolResults toolResult | **`<toolUseId>#result`**(tool_use와 동일 id라 suffix 없으면 `(source, external_id)` UNIQUE 충돌 → tool_result 유실. Claude 소스1과 동일 규칙) |

`message_id`/`toolUseId`는 파일 내 고유(실측 검증됨)하지만, Prompt와 AssistantMessage의 text 블록이 같은 `<message_id>#<idx>` 포맷을 공유해 `message_id` 재사용 시 충돌 가능성이 있었다(2026-07-02 리뷰 지적으로 발견·수정) → `kind` prefix로 원천 차단.

**라인 ts 부재 → 단조증가 보간 (핵심 설계)**
- 라인 레벨 timestamp가 없으므로 파일 처리 시작점 `start_ts`부터 **단조증가**하는 `ts`를 계산해 각 이벤트에 붙인다.
- `start_ts` 산출(우선순위): 그 스트림(`kiro_cli:<sessionId>`)의 기존 최신 `ts`(재개 시) → 메타 `created_at`(ISO8601→ms) → 파일 mtime(ms).
- `cur = start_ts`. 각 라인 처리 시: `kind=Prompt`이고 `meta.timestamp`(epoch 초)가 있으면 `cur = max(cur+1, timestamp*1000)`(실제 시각으로 앵커링), 아니면 `cur += 1`.
- 한 라인의 모든 블록 이벤트는 같은 `cur`를 공유(블록별 +1 불필요 — 블록 구분은 externalId로 충분).
- 재파싱 시 ts가 달라져도 `(source, external_id)` UNIQUE라 중복 삽입 없음(idempotent 유지).

**매핑**: `stream.id = "kiro_cli:" + session_id`(파일 stem), `kind = session`, `title = 메타 title`, `project = 메타 cwd`(레포 루트 정규화, 02·Claude 소스1과 동일 함수 공유), `gitBranch = null`, `metadata.parent_session_id`(메타에 있으면).

---

## 소스 3 · Kiro IDE ✅ 검증됨(경로/구조) — 캡처 보류

**경로**: `~/Library/Application Support/Kiro/User/globalStorage/kiro.kiroagent/workspace-sessions/<base64(workspace path)>/sessions.json`
- 폴더명 = **base64(워크스페이스 절대경로)** → 디코드하면 `project`. (예: `L1VzZXJz…` → `/Users/me/src/acme-web` 검증됨)
- `sessions.json` = 세션 객체 **배열**(빈 워크스페이스는 `[]`).
- 보조: 같은 트리의 `state.vscdb`(SQLite, VS Code 상태)에 채팅이 있을 수 있음.

**매핑**: 배열 각 항목 → stream(`kiro_ide:<id>`), `project = base64 디코드 경로`.
> 항목 스키마는 **비어있지 않은 sessions.json**으로 착수 시 확정 필요.
> **실측 결과(2026-07-02)**: 보유한 모든 워크스페이스의 `sessions.json`이 빈 배열 `[]` → 세션 객체 스키마를 확정할 수 없어 **캡처 보류**(M2 범위 제외). 비어있지 않은 샘플을 확보하는 대로 착수.

---

## 소스 4 · Gemini (웹) — 로컬 파일 없음 → 확장 필요 (M3)

- MV3 브라우저 확장, content script on `gemini.google.com`.
- 제출된 프롬프트 + 응답 텍스트 캡처 → `POST /v1/ingest`.
- `stream.id = "gemini_web:" + conversationId`(URL에서 추출), `url` 저장, `project = null`.
- 토큰/포트는 확장이 `~/.logroom/ingest.json`에 직접 접근 불가하므로, **네이티브 메시징** 또는 앱이 확장에 세션 토큰 전달하는 핸드셰이크 설계(M3에서 상세).

## 공통 규칙

- 모든 어댑터는 `packages/core`의 정규화 함수로 Event/Stream 산출 → 인제스트 클라이언트로 전송.
- **body 저장 정책(ADR-0012, 회고 중심 저장)**: 기본(`essential`) `tool_result.body`는 256자, `tool_use.metadata`의
  input은 512자로 절단(`inputPreview`)해 저장하고, `prompt`/`response`는 항상 전문 저장한다 — `~/.logroom/config.json`의
  `capture.bodyPolicy: "full"`로 절단 없는 원문 저장으로 전환 가능(02-data-model.md 참고).
- **민감정보 스크럽**(M4, `src/capture/scrub.rs`, 기본 **on**): 정규식으로 body/title에서 시크릿 패턴을
  마스킹 후 저장(프라이버시). 상세 규칙·한계는 04-privacy-security.md 참고.
- 실패는 조용히 재시도(로컬 큐) — 앱 꺼짐/네트워크 없음 상황에서도 유실 최소화.
- **캡처 헬스**: 소스별 `last_captured_at` / 상태(ok·지연·오류)를 추적(`capture_cursors` 활용)해 `capture-health` 이벤트로 UI에 노출. "기록 최우선" 제품이라 **캡처가 멈춘 걸 사용자가 즉시 알 수 있어야** 한다.
