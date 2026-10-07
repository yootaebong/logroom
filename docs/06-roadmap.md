# 06 · Roadmap

원칙: **기록 최우선 · 수직 슬라이스로 빨리 end-to-end.** 각 마일스톤은 완료기준(AC)을 만족해야 다음으로.

## M0 · Foundation
스캐폴딩. "빈 앱이 뜨고 DB가 선다."

**M0a ✅ 완료** (커밋 `4a31c88`)
- 모노레포(pnpm workspaces + Turborepo + Biome + tsconfig base) ✅
- `apps/desktop` = Tauri v2 + React19/Vite/Tailwind v4/shadcn ✅ (productName "LogRoom", id `app.logroom.desktop`)
- `packages/core` = Event/Stream/Ingest zod 스키마 + 타입(SSOT) ✅
- 검증: `turbo build`(core tsc / desktop tsc+vite) 통과, `biome check` 이슈 0

**M0b ✅ 완료** (Rust 백엔드 · 로컬 검증 완료, 커밋 대기)
- Rust: SQLite(rusqlite **0.39** bundled + FTS5 trigram) + refinery `V1__init.sql`(02의 DDL) + PRAGMA(WAL/busy_timeout/foreign_keys/synchronous) ✅
  - ⚠️ rusqlite는 0.40이 최신이나 `refinery-core 0.9`가 `rusqlite <=0.39`만 지원 → **0.39 고정**.
- Rust: 인제스트 서버(axum, `/v1/health` liveness + `/v1/ingest` 토큰 상수시간검증·body limit, 토큰/포트 → `~/.logroom/ingest.json` 0600, 포트 재사용, single-instance 플러그인) ✅
- Tauri 커맨드 골격(`list_events_by_day`/`search_events`/`get_stream` 스텁 — 실쿼리는 M1) + `event-ingested` emit + 앱 실제 실행 ✅
- 스키마 SSOT(zod↔serde) 계약 테스트: 공유 fixture `packages/core/test/fixtures/ingest-sample.json`을 vitest(zod) + Rust `#[test]`(serde) 양방향 파싱 ✅
- **AC 전부 통과**: 앱 창 표시 · `logroom.db`(journal_mode=wal, perm 0600) 생성 · `GET /v1/health` 200 · `cargo build`/`cargo test` 통과 · 계약 테스트 통과.
  - 보너스 검증: 토큰 없는 POST→401 · ingest inserted:2 · 재전송 idempotent inserted:0 · stream started/ended 재계산 · FTS5 trigram 한글검색('백엔드') hit.
- 파일: `apps/desktop/src-tauri/{Cargo.toml, migrations/V1__init.sql, src/{model,db,ingest,lib}.rs}` + core 계약 테스트.

## ⤷ 사이드 · 웨이트리스트 랜딩 (M1 전, 반나절 · 비블로킹)
M0 이후 ~ M1 착수 전, 반나절 타임박스. **M1을 막지 않는 사이드 트랙.**
- `apps/web` = Astro + Tailwind, **1페이지**: 가치제안 + 프라이버시 약속 + 이메일 수집.
- 배포: Vercel/Cloudflare Pages (모노레포에서 `apps/web`만).
- 애널리틱스/이메일은 **사이트에서만** — 앱 텔레메트리 0 유지 (ADR-0011).
- 도메인: **logroom.app 확정 (구매 완료)** — DNS 연결은 웨이트리스트 배포 시점.
- **AC**: 공개 URL 접속 + 이메일 수집 동작. 반나절 후 손 뗀다(풀 랜딩은 M5).

## M1 · First Recording (수직 슬라이스) ★
Claude Code를 실제로 기록해서 타임라인에 보이게.
- Rust 파일감시: `~/.claude/projects/**/*.jsonl` tail + backfill(cursor)
- 정규화(03의 Claude Code 매핑) → `db::ingest()` 직접 호출(감시자=앱 내부 프로세스라 HTTP 우회; hook만 POST)
- hook 스크립트(`packages/captures/claude-code`) 실시간 POST
- Tauri commands: `list_events_by_day`, `search_events`, `get_stream`
- UI: ① Today 타임라인(병렬 레인) + ② FTS 검색
- **AC**: 실제 Claude Code 세션이 레인으로 뜨고, 프롬프트가 검색된다. 앱 재실행 시 backfill 동작.

## M2 · Kiro

**CLI ✅ 완료** — `~/.kiro/sessions/cli/*.jsonl` 감시+backfill, 라인 kind 4종(Prompt/AssistantMessage/ToolResults/Compaction) 정규화, 라인 ts 부재에 대한 단조증가 보간 적용(03의 소스2).
- `conversations_v2` 백필은 **후속으로 격하**(스키마는 확인됐으나 이번 M2 범위 밖).
- **AC(CLI)**: Kiro CLI 세션이 스트림으로 뜨고, 프롬프트/응답/툴사용이 검색된다. 앱 재실행 시 backfill 동작(claude_code와 동일 cursor idempotent 재개).

**IDE 🚧 보류** — `workspace-sessions/*/sessions.json` 실측 시 보유 워크스페이스 전부 빈 배열이라 세션 객체 스키마 확정 불가(03의 소스3). 비어있지 않은 샘플 확보 시 별도 착수.

## M3 · Gemini (웹 확장)
- `apps/extension` MV3, content script on gemini.google.com
- 앱↔확장 토큰 핸드셰이크(네이티브 메시징)
- **AC**: Gemini 웹 프롬프트/응답이 캡처돼 스트림으로 뜬다.

## M4 · Digest & Polish
- ④ Digest(집계 기반 "오늘 한 일"), ③ Stream Detail 대화뷰
- 필터/키보드 네비, 시크릿 스크럽 룰, **캡처 일시정지 UI ✅ 완료**(`CaptureHealthBadge` 토글,
  `get_capture_paused`/`set_capture_paused`, 의미론은 04-privacy-security.md), **소스별 캡처 헬스 표시**
- **AC**: 매일 아침 전날 요약이 쓸만하다(도그푸드 습관화).

## M5 · Product Hardening
- ⑤ Settings 전체 + **첫 실행 온보딩**, (옵션)SQLCipher, export/삭제 + DB 유지보수(VACUUM/checkpoint)
- 패키징: `.dmg` + Apple 공증 (**App Store 아님 — ADR-0009**), auto-update(서명 매니페스트)
- 프라이버시 릴리스 체크리스트(04) 통과
- **풀 랜딩**(`apps/web`): 타임라인 GIF/스크린샷 · 기능 · 프라이버시 · 다운로드 (가격/결제는 M6)
- **AC**: 서명·공증된 설치파일로 깨끗이 설치/업데이트 된다. 아웃바운드 0 검증.

## M7 · Pay-worthy Output ★ 다음 (ADR-0016 — M6보다 **선행**)
- **① 일일 AI 요약**: 다이제스트 상단 3~5줄. 엔진 이중화 — **Claude CLI headless**(`claude -p`,
  구독 쿼터·0클릭) + **BYO Anthropic API 키**(커넥터 토큰과 동일 마스킹·0600). 설정 "자동/CLI/키".
  **프롬프트 en/ko 이중화 — 요약 출력 언어 = 앱 언어(필수 요구)**. opt-in + 전송 발췌 미리보기 +
  일시정지 연동. CLI는 `CLAUDE_CONFIG_DIR` 격리(자기 캡처 루프 방지)
- **② 재개 브리핑**: 프로젝트별 "어디까지 했지" 패킷 생성 → 클립보드(에이전트 재투입용)
- ③ 주간 리포트(md 내보내기) — ①② 도그푸드 검증 후
- **④ 업무평가서**(ADR-0017): 분기 평가(본체)·월간 점검 — 잘한 것 / 못한 것(월간: 밀린 것·막힌 것) /
  채우면 좋은 것. 월간·주간 요약 + DB 사실 신호 + 선택 입력(프로젝트·목표·평가 프로필). 점수·등급 없음
- (후속 옵션) 로컬 모델 — Ollama 설치 감지 연동만, 동봉 없음(ADR-0016 기각 근거 참고)
- **AC**: ①②를 본인이 2주 도그푸드 → "돈 낼 만하다" 판정이 M6 착수 조건.

## M6 · Monetization (판매 시작) — **M7 이후로 순연 (ADR-0016)**
- 무료/Pro 경계 구현 — **게이트 위치(커넥터 vs 요약·리포트)는 M7 검증 결과로 재논의**(ADR-0007 부분 재검토)
- 라이선스: **Ed25519 오프라인 서명키** 로컬 검증, Lemon Squeezy 체크아웃(MoR)
- **구독**(월/연)
- 랜딩페이지 + 네트워크 투명성 페이지, Apple 공증
- **AC**: 결제→라이선스 키→Pro 기능 활성이 오프라인 검증으로 동작.

## M8+ · Expand
- 커넥터: Notion · Jira · Figma
- Windows 빌드/서명
- 티켓 크로스링크(Linear↔GitHub↔세션 — TICKET-N 커버리지 18% 실측, 컨벤션 정착 또는 AI 매칭 필요)
- (옵션) E2E 멀티기기 동기화(Pro)

---

## 수익화 상세 (deferred, M6에서 구현)

- **모델**: MIT 오픈소스(ADR-0018), 수익화는 별도 재결정. (이전 계획: 무료=로컬 AI 캡처+타임라인+검색, Pro=커넥터+AI요약+동기화.)
- **구독 근거**: 커넥터는 외부 API 변경으로 지속 유지보수 필요 → 반복 매출 정당화. 동기화는 실서버비 발생.
- **결제**: Lemon Squeezy/Paddle(Merchant of Record) → 글로벌 세금 위임.
- **라이선스**: 서명된 키를 앱이 내장 공개키로 로컬 검증(phone-home 불필요). 온라인 활성화는 옵션(좌석 제한 soft).
- 가격/티어 확정은 도그푸드 후 별도 결정.

## 현재 위치

**v0.5.0 배포 (2026-07-15, PR #1~#47 머지).** M0~M5 완료 + 커넥터 3종(Slack·GitHub·Linear) 가동.
실배포 앱이 auto-update로 갱신되는 상태(5회차). **다음 = M7-① 일일 AI 요약**(ADR-0016 — M6 순연).

| 마일스톤 | 상태 | 핵심 산출물 (PR) |
|---|---|---|
| M0 Foundation | ✅ | 모노레포 + Rust 백엔드(SQLite/FTS5·axum·계약테스트) |
| M1 First Recording | ✅ | 읽기 커맨드+타임라인/⌘K 검색(#1) · Claude Code 파일감시/정규화(#2) · FTS 2글자 LIKE 폴백(#3) · tool_result externalId fix(#4) |
| M2 Kiro | ✅ CLI만 | 감시+backfill+ts 단조증가 보간(#5). **IDE는 보류**(sessions.json 실측 전부 빈 배열 — 샘플 확보 시 착수) |
| M3 Gemini | ⏸ 보류 | MV3 확장+네이티브 메시징 — 재평가 대기 |
| M4 Digest & Polish | ✅ | Digest(#10) · 요청 중심 턴 트리(#11) · 타임라인 개편(#12) · 캡처 헬스(#16) · 키보드 네비+시크릿 스크럽(#17) · 캡처 일시정지(#18) |
| M5 Product Hardening | ✅ 공증 제외 | 트레이(#20) · Settings/데이터 관리(#23~24) · 온보딩(#39) · dmg+auto-update(#28, ADR-0013) · 랜딩 logroom.app(#27~35) · 슬랙식 셸+테마(#29) · i18n en/ko(#40) |
| 커넥터 트랙 | ✅ 3종 | ADR-0015. Slack(#37) · GitHub 다중계정(#43) · Linear 이슈=스트림(#45~46) · 릴리스 v0.1.0~**v0.5.0**(#47) |
| M7 Pay-worthy Output | ▶ ① 완료 | ADR-0016. **① 일일 AI 요약 완료**(CLI+BYO 이중 엔진 · 프롬프트 v3 en/ko "한눈에+자세히" · 전용 뷰+설정 AI 탭 · 자동 생성(캐치업형) · 미리보기 에디터 · V8 캐시). 도그푸드 중 → ② 재개 브리핑 → 검증 후 M6 |

**품질/데이터 트랙** (도그푸드 피드백 기반, 마이그레이션 V2~V5):
- 캡처 root 자동탐지(`~/.claude*`)+커스텀 설정+Settings UI(#6) — CLAUDE_CONFIG_DIR 분리 사용자 대응
- 회고 중심 저장 ADR-0012(#7): tool 요약화+FTS는 prompt/response만 — **DB 1.1GB→204MB**
- 노이즈 소급 정리: 커맨드 메타(#8, V3) · 에이전트 알림(#14, V4) · worktree project 정규화(#15, V5)
- UI 방향 확정: 다이제스트 기본 뷰(#13), "내 요청이 1급 정보" 원칙

### 다른 세션에서 재개하는 법
1. 저장소 루트에서 시작
2. `pnpm install` → `source ~/.cargo/env` → `pnpm --filter @logroom/core build`(desktop tsc가 core dist 필요)
3. 상태 확인: `cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml` · `pnpm --filter @logroom/desktop build` · `pnpm exec biome check apps/desktop/src`
4. 앱: `pnpm --filter @logroom/desktop tauri dev` — 백그라운드는 `nohup … & disown`(셸 종료에 앱이 딸려 죽는 것 방지). **마이그레이션 파일 작업 전엔 dev 반드시 종료**(워처가 미완성 마이그레이션을 실 DB에 자동 적용).
5. **다음 작업 = M7-① 일일 AI 요약**(ADR-0016: SummaryEngine 이중화 CLI+BYO · 프롬프트 en/ko · opt-in+발췌 미리보기 · `CLAUDE_CONFIG_DIR` 격리) 또는 도그푸드 피드백 대응. 세션 간 미세 컨텍스트는 `CLAUDE-activeContext.md`.

> **구현 메모**: rusqlite **0.39 고정**(refinery-core 0.9 제약). DB `~/Library/Application Support/LogRoom/logroom.db`, 마이그레이션 V1~V5(pending 시 자동 백업 `bak-pre-v<N>` + 적용 시 VACUUM). 설정 `~/.logroom/config.json`(roots/bodyPolicy/scrubSecrets/capturePaused). externalId는 kind별 네임스페이스 필수(#4 교훈). 시크릿 스크럽·일시정지는 파일감시+HTTP **두 진입점 모두** 적용.

> **확정 요약**: 이름 **LogRoom** / 도메인 **logroom.app**(구매완료) / 스택 Tauri v2 + React + Vite + Tailwind v4 + shadcn / SQLite+FTS5(trigram) / 로컬온리·프라이버시 / MIT 오픈소스(ADR-0018), 수익화는 별도 재결정 / macOS 우선. 결정 상세는 `07-decisions`(ADR-0001~0018).
