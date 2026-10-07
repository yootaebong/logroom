# 02 · Data Model

SQLite 단일 파일. 모든 소스를 **통합 Event/Stream 모델**로 정규화한다.
DB 위치: `~/Library/Application Support/LogRoom/logroom.db` (perm 0600).

## 개념

- **Stream** = 하나의 세션/대화/작업 흐름 = 타임라인 **레인 1개**. 소스의 native session이 기본 단위.
- **Event** = 스트림 안의 개별 활동(프롬프트·응답·툴사용·메시지 등).
- 관계: `Event.stream_id → Stream.id` (N:1).

## DDL

```sql
-- 스트림: 세션/대화 = 병렬 레인 1개
CREATE TABLE streams (
  id          TEXT PRIMARY KEY,               -- "<source>:<native session id>"
  source      TEXT NOT NULL,                  -- claude_code | kiro_cli | kiro_ide | gemini_web | manual
  kind        TEXT NOT NULL DEFAULT 'session',-- session | task | thread | agent
  title       TEXT,                           -- 소스 제공 제목(예: Claude ai-title) 또는 자동 생성
  project     TEXT,                           -- cwd/repo 경로
  git_branch  TEXT,
  started_at  INTEGER NOT NULL,               -- epoch ms (첫 이벤트)
  ended_at    INTEGER,                        -- epoch ms (마지막 이벤트; 진행중이면 갱신)
  status      TEXT NOT NULL DEFAULT 'active', -- active | done
  metadata    TEXT NOT NULL DEFAULT '{}',     -- JSON (소스별 부가정보)
  created_at  INTEGER NOT NULL
);
CREATE INDEX idx_streams_started ON streams(started_at DESC);
CREATE INDEX idx_streams_source  ON streams(source, started_at DESC);
CREATE INDEX idx_streams_project ON streams(project);

-- 이벤트: 스트림 내 개별 활동
CREATE TABLE events (
  id          TEXT PRIMARY KEY,               -- uuid v7 (시간정렬 가능)
  stream_id   TEXT REFERENCES streams(id) ON DELETE CASCADE,
  ts          INTEGER NOT NULL,               -- epoch ms
  source      TEXT NOT NULL,
  type        TEXT NOT NULL,                  -- prompt | response | tool_use | tool_result | message | file_edit | note
  title       TEXT,                           -- 리스트 표시용 짧은 요약
  body        TEXT,                           -- 원문(프롬프트/응답/내용)
  model       TEXT,                           -- 예: claude-sonnet-4-5
  tokens_in   INTEGER,
  tokens_out  INTEGER,
  url         TEXT,
  parent_id   TEXT,                           -- 스레드 트리(부모 이벤트 id)
  external_id TEXT,                            -- 소스 native uuid (dedup 키)
  metadata    TEXT NOT NULL DEFAULT '{}',
  created_at  INTEGER NOT NULL,
  UNIQUE(source, external_id)                 -- 재수집 idempotent
);
CREATE INDEX idx_events_ts        ON events(ts DESC);
CREATE INDEX idx_events_stream    ON events(stream_id, ts);
CREATE INDEX idx_events_source_ts ON events(source, ts DESC);
CREATE INDEX idx_events_type      ON events(type);

-- 전문검색: FTS5 external content. CJK 부분검색 위해 trigram 토크나이저.
-- V2는 prompt/response 타입만 인덱싱하도록 좁혔고, V7이 사용자 콘텐츠인 message/note를 다시 더했다
-- (아래 "저장 규모" 절 참고) — 트리거 WHEN 조건으로 강제, tool_use/tool_result는 계속 제외.
CREATE VIRTUAL TABLE events_fts USING fts5(
  title, body,
  content='events', content_rowid='rowid',
  tokenize='trigram'
);
CREATE TRIGGER events_ai AFTER INSERT ON events WHEN new.type IN ('prompt', 'response', 'message', 'note') BEGIN
  INSERT INTO events_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;
CREATE TRIGGER events_ad AFTER DELETE ON events WHEN old.type IN ('prompt', 'response', 'message', 'note') BEGIN
  INSERT INTO events_fts(events_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
END;
-- UPDATE는 트리거 하나의 WHEN 절에서 old/new 타입을 동시에 분기할 수 없어 delete/insert 트리거로 분리(V2).
CREATE TRIGGER events_au_del AFTER UPDATE ON events WHEN old.type IN ('prompt', 'response', 'message', 'note') BEGIN
  INSERT INTO events_fts(events_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
END;
CREATE TRIGGER events_au_ins AFTER UPDATE ON events WHEN new.type IN ('prompt', 'response', 'message', 'note') BEGIN
  INSERT INTO events_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;

-- 캡처 진행상태(파일 감시 오프셋 등, backfill용)
CREATE TABLE capture_cursors (
  source     TEXT NOT NULL,
  resource   TEXT NOT NULL,      -- 파일 경로 등
  offset     INTEGER NOT NULL DEFAULT 0,
  mtime      INTEGER,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (source, resource)
);
```

> **토크나이저 주의**: `trigram`은 한국어/CJK 부분일치에 강하지만 인덱스가 커진다.
> 영문 위주면 `unicode61 remove_diacritics 2`가 가볍다. 도그푸드는 한글 검색이 중요 → **trigram 채택**.
> (SQLite 3.51 확인됨, trigram 지원 OK.)

## 시간 · 타임존 · 일 경계 (모든 시간 처리의 기준)

- **저장**: 모든 `ts`/`started_at`/`ended_at`은 **epoch milliseconds(UTC 절대시각)**. 로컬시각 저장 금지.
- **정규화**: 소스 타임스탬프를 인제스트 전 epoch ms로 변환.
  - Claude Code: `timestamp`(ISO8601) → ms.
  - Kiro: `created_at`/`updated_at` 이미 ms.
- **"하루" 정의**: **사용자 로컬 타임존 기준** 일 경계(로컬 00:00 ~ 다음날 00:00).
  - `list_events_by_day(localDate, tz)`는 로컬 자정 2개를 epoch ms 범위 `[startMs, endMs)`로 변환해 `ts` 범위 쿼리.
  - 타임존 = OS 로컬 TZ(설정에서 override, 기본 시스템). DST/이동 시에도 **저장은 절대시각, 표시는 조회 시점 TZ**로 일관.
- **경계 스트림(자정 넘김)**: 스트림이 자정을 넘기면 각 날짜 뷰에서 **해당 날짜 구간으로 클리핑**해 렌더(레인 배치 입력도 클리핑된 구간 사용).

## 마이그레이션 전략

- **refinery**(Rust) — `migrations/V1__init.sql`, `V2__essential_body.sql`(ADR-0012) 형태 버전드 SQL.
- 앱 시작 시 최신 버전까지 자동 적용. **다운그레이드 없음**(로컬 데이터 안전 우선).
- 스키마 변경 시: 새 마이그레이션 파일 추가만. 기존 파일 수정 금지.
- 매 마이그레이션 전(pending 존재 시) `logroom.db`를 `logroom.db.bak-pre-v<version>`으로 백업(자동, perm 0600). 백업 실패 시 마이그레이션 중단.
- **마이그레이션 후 정리**: `open_and_migrate`가 refinery의 실행 리포트로 **새로 적용된 마이그레이션이 있을 때만**
  `VACUUM` + `PRAGMA wal_checkpoint(TRUNCATE)`를 실행한다(트랜잭션 밖에서, 매 기동 비용 방지). `V2__essential_body.sql`처럼
  삭제/축소가 큰 마이그레이션 직후 디스크 공간을 즉시 회수하기 위함.

## SQLite 런타임 설정 (PRAGMA)

연결 초기화 시 항상 적용(Rust 커넥션):
- `journal_mode = WAL` — 인제스트(쓰기)와 UI(읽기) 동시성. 리더가 라이터를 막지 않음.
- `busy_timeout = 5000` — 순간 잠금 충돌 시 대기(즉시 실패 방지).
- `foreign_keys = ON` — `ON DELETE CASCADE` 동작 보장.
- `synchronous = NORMAL` — WAL과 함께 성능/안전 균형.
- 쓰기는 **단일 라이터(인제스트 경로)** 로 직렬화, 읽기는 다중 허용.

## 스트림 그룹핑 (핵심 로직)

**stream_id 도출** = `"<source>:<native session id>"`
- Claude Code: `claude_code:<sessionId>` — 서브에이전트(Task)는 `claude_code:<sessionId>:agent:<agentId>` (`kind=agent`, 별도 레인, `metadata.parentStream`으로 부모 연결)
- Kiro CLI: `kiro_cli:<session_id>`
- Kiro IDE: `kiro_ide:<sessionId>` (workspace-sessions 항목)
- Gemini: `gemini_web:<conversationId>`
- manual: `manual:<uuid>`

**Stream 필드 채우기**
- `title`: 소스 제목(Claude `ai-title`, Kiro `.json.title`) → 없으면 prompt 휴리스틱(`capture/policy.rs::derive_stream_title`): 순서대로 스캔해 trim 후 15자(코드포인트) 이상인 **첫 "의미 있는" prompt**를 채택(그런 prompt가 없으면 첫 prompt로 폴백) → 첫 줄만(첫 개행 경계) 추출 → 선두 마크다운 프리픽스(`#`, `>`, `-`, `*`, 공백 반복) strip → trim → 60자(코드포인트) 초과 시 절단(`…`). "ㄱㄱ" 같은 응답성 첫 prompt가 그대로 title이 되거나 60자 하드컷으로 주제가 안 드러나던 문제(실측 2026-07: session 111개 중 55개가 하드컷 title)를 완화한다. 기존 하드컷 title 데이터는 `V6__derive_titles.sql`로 소급 재계산했다.
  - **수동 편집**: `rename_stream` 커맨드(FE `StreamDetailPanel`의 연필 아이콘)로 title을 직접 바꿀 수 있다. 이때 `metadata.manualTitle = true`를 세팅해, 이후 같은 스트림에 재캡처가 들어와도 `db.rs::upsert_stream`이 title을 덮지 않고 보호한다(그 외 `project`/`git_branch` 컬럼은 기존처럼 갱신).
- `project`: `cwd`(Claude/Kiro CLI) / base64 디코드 경로(Kiro IDE) / null(Gemini). cwd가 레포 하위면 **`.git` 상위 = 레포 루트로 정규화**해 같은 레포 스트림을 묶는다(하위 디렉토리 파편화 방지). `.git`이 파일(git worktree)이면 `gitdir: <path>` 내용을 파싱해 `/.git/worktrees/` 세그먼트 앞부분(본 레포 루트)으로 한 번 더 정규화한다(기존 오염 데이터는 `V5__worktree_project_remap.sql`로 소급).
  cwd가 레포 밖(**hub 세션** — 예: 중간 에이전트(agent hub)가 `~/.agent-hub` 에서 띄운 세션)이면 턴마다 실제로 만진 레포로 이벤트를 자식 스트림 `<base id>@<레포 루트>`(project = 레포 루트, `metadata.hub`·`hubStream`)으로 옮긴다 — 규칙은 03 "hub 세션" 참고.
- `started_at`/`ended_at`: 스트림 이벤트 ts의 min/max. 이벤트 삽입 때마다 `ended_at = max(ended_at, ts)`.
- `status`: 마지막 이벤트가 N분(기본 15) 이내면 `active`, 아니면 `done`(조회 시 계산 or 배치).

## 레인 배치 (타임라인 렌더 알고리즘)

특정 날짜의 스트림들을 겹치지 않게 세로 레인에 배치 = **구간 분할(greedy interval partitioning)**:

```
1. 그 날 [started_at, ended_at]가 걸치는 stream들을 started_at 오름차순 정렬
2. lanes = []
3. 각 stream s:
     - lanes 중 마지막 stream의 ended_at <= s.started_at 인 레인 찾기
     - 있으면 그 레인에 배치, 없으면 새 레인 생성
4. lane index = 세로 위치, 시간 = 가로 위치(d3 time scale)
```

- "지금 활동 중" = `ended_at`가 최근 15분 이내인 스트림 → 상단/강조 표시.
- 프로젝트별 색상, 소스별 아이콘.

## 저장 규모 · 보존 · 내보내기

- **실측(2026-07, 도그푸드 DB)**: 전체 DB 1.1GB 중 **FTS 인덱스 734MB(67%)** + `tool_result.body` 201.8MB +
  `tool_use.metadata`(input 전문) 28.3MB — tool 계열이 대부분을 차지하는 반면, 제품의 핵심 가치인
  **prompt+response는 16MB(전체의 1.5%)뿐**이었다. 원문(코드 diff, 커맨드 출력 등)은 어차피 로컬 소스
  파일/터미널 히스토리에 남아있어 "회고"(무엇을 언제 물어봤고 어떻게 답했는지) 용도로는 tool 계열 전문 보존의
  한계효용이 낮다.
- **회고 중심 저장 정책(essential body policy, ADR-0012)**: `capture.bodyPolicy`(기본 `essential`, camelCase
  JSON 값은 `"essential"` | `"full"`, config.rs 참고)로 캡처 시점에 다이어트한다.
  - `prompt`/`response`: 정책과 무관하게 항상 **전문** 저장(핵심 가치).
  - `tool_result.body`: essential이면 **256자(코드포인트) 절단**(초과 시 말미 `…`), full이면 원문 그대로.
  - `tool_use.metadata`: essential이면 `input` 전문 대신 문자열화 후 **512자 절단**한 `inputPreview`만 보관,
    full이면 원문을 `input` 키에 그대로 보관. `body`(한줄 요약, command/file_path/pattern)는 정책과 무관하게 유지.
  - `full`은 설정 파일(`~/.logroom/config.json`) 수동 편집으로만 전환 가능(FE Settings UI 노출은 후속).
- **FTS 인덱싱 범위**: `events_fts`는 **`prompt`/`response`/`message`/`note`** 타입만 인덱싱한다(트리거 WHEN 조건,
  위 DDL 참고). V2는 tool 계열(body가 요약/미리보기뿐이라 검색 노이즈이기도 했다)을 제외하며 `prompt`/`response`만
  남겼는데, 이후 Slack 커넥터가 `message`(사용자가 직접 쓴 메시지) 타입을 도입하면서 이 조건이 사용자 콘텐츠까지
  함께 배제하는 부작용이 드러났다. `message`/`note`(수동 기록, 미래)는 tool 계열이 아니라 prompt/response와
  동급의 사용자 콘텐츠이므로 `V7__fts_user_content.sql`로 인덱싱 범위에 재포함했다(ADR-0012 보강). `tool_use`/
  `tool_result`는 계속 제외.
- **소급 마이그레이션**: `V2__essential_body.sql`이 기존 데이터의 `tool_result.body` 256자 절단 + `tool_use.metadata.input`
  제거 + FTS 재구축(`delete-all` 후 prompt/response만 재인덱싱)을 한 번에 적용했다. `V7__fts_user_content.sql`은
  트리거를 `message`/`note` 포함 조건으로 재정의하고, 기존 `message`/`note` 행을 FTS에 추가 인덱싱한다(전체
  재구축 없이 두 타입만 삽입). 두 마이그레이션 모두 직후 `VACUUM`으로 디스크 공간을 회수(위 "마이그레이션 전략" 참고).
- **대용량 body(향후)**: essential 정책으로도 prompt/response 자체가 매우 큰 경우(예: 대량 붙여넣기)는 이번 범위 밖 —
  임계값 초과 시 압축 blob 분리 저장은 필요성이 확인되면 별도 검토.
- **보존 정책(구현됨)**: 기본 무제한(로컬), **자동 삭제는 없다**(의도적 제외 — 되돌릴 수 없는 삭제를 백그라운드에서
  트리거하는 위험을 피한다). Settings "데이터" 섹션의 "기간 삭제"로 **수동**으로만 지정 일수(기본 90일) 이전
  기록을 지울 수 있다: `count_events_before(ts)`로 삭제 대상 건수를 먼저 보여주는 인라인 확인 단계를 거친 뒤
  `delete_events_before(ts)`가 실행된다(`src-tauri/src/data_admin.rs`). 삭제는 하나의 트랜잭션 안에서
  이벤트 삭제(FTS는 기존 트리거로 자동 반영) → 빈 스트림 제거 → 남은 스트림 경계(started_at/ended_at)
  재계산(`V3__prune_meta_prompts.sql`과 동일 패턴)까지 원자적으로 처리하고, 커밋 후 `wal_checkpoint(TRUNCATE)`한다.
- **DB 유지보수(구현됨)**: Settings "데이터" 섹션의 "DB 최적화" 버튼이 `VACUUM` + `wal_checkpoint(TRUNCATE)`를
  실행하고 전/후 파일 크기(본체+WAL, 예: "208MB → 195MB")를 보여준다(`data_admin::vacuum_db`). 마이그레이션
  직후 자동 VACUUM(위 "마이그레이션 전략")과는 별개로 사용자가 언제든 수동 실행할 수 있다.
- **Export(구현됨)**: Settings "데이터" 섹션의 "내보내기" 버튼이 `~/Downloads/logroom-export-<YYYYMMDD-HHMMSS>/`에
  `streams.jsonl` + `events.jsonl`(NDJSON, 테이블 컬럼 그대로 camelCase JSON) + `wal_checkpoint(TRUNCATE)` 후
  `logroom.db` 원본 사본을 생성한다(`data_admin::export_data`). 대량(15만+ 행) 테이블도 prepared statement로
  한 행씩 스트리밍 write해 메모리에 전체를 적재하지 않는다. 암호화 export 옵션은 이번 범위 밖(04 참고).
