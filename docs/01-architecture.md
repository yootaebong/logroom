# 01 · Architecture

## 기술 스택 (확정) + 근거

| 레이어 | 선택 | 근거 |
|--------|------|------|
| 데스크톱 셸 | **Tauri v2** | 작고 서명 가능한 설치파일, 내장 auto-updater, secure-by-default → 유료 프라이버시 제품에 최적 (vs Electron: 용량/메모리 불리) |
| 프론트 | **React 18 + TypeScript + Vite** | 익숙함, 생태계 |
| 스타일 | **Tailwind v4 + shadcn/ui** | 코드 소유형(오픈코어 아니어도 커스터마이즈 자유), a11y 기본 |
| 상태 | **Zustand** (UI) + **TanStack Query** (DB 읽기 캐싱) | 가벼움 |
| 타임라인 | **커스텀 SVG + d3-scale + TanStack Virtual** | 병렬 레인 타임라인은 기성품 없음. 대량 이벤트 → 가상화 필수 |
| 로컬 DB | **SQLite (rusqlite 번들 + FTS5)** in Rust | FTS5/트리거/암호화(SQLCipher) 제어 위해 Rust측. TS는 얇은 커맨드 레이어 |
| 마이그레이션 | **refinery** (Rust) | day-1부터 버전 관리 |
| 캡처 수신 | **axum** on 127.0.0.1 + 토큰 | hook/확장/CLI가 POST |
| 파일 감시 | **notify** (Rust) | 세션 로그 파일 tail |
| 모노레포 | **pnpm workspaces + Turborepo** | 솔로에 가벼움 |
| 린트/포맷 | **Biome** | 한 도구로 빠르게 |
| 테스트 | **Vitest** + Rust test (+ 후에 Playwright/tauri-driver) | |

> 핵심 원칙: **로직 대부분은 TS**, Rust는 3곳만 — ①SQLite/FTS ②인제스트 서버 ③파일 감시(+후에 라이선스 검증).

## 모노레포 레이아웃

```
logroom/
├── apps/
│   ├── desktop/            # Tauri 앱
│   │   ├── src/            # React 프론트 (UI)
│   │   └── src-tauri/      # Rust: DB, 인제스트 서버, 파일감시, 커맨드
│   ├── extension/          # (M3) 브라우저 확장 MV3 — Gemini 웹 캡처
│   └── web/                # 랜딩(Astro + Tailwind): 웨이트리스트(now) → 풀랜딩(M5)
├── packages/
│   ├── core/               # 공유 TS: Event/Stream zod 스키마, 정규화, 타입
│   └── captures/
│       ├── claude-code/    # Claude Code hook 스크립트 + JSONL 파서
│       ├── kiro/           # Kiro CLI/IDE 파서
│       └── shared/         # 캡처 공통(인제스트 클라이언트, 토큰 로드)
├── crates/                 # (필요시) 공유 Rust 크레이트
├── docs/                   # 본 문서
└── package.json / pnpm-workspace.yaml / turbo.json / biome.json
```

## 스키마 SSOT (TS zod ↔ Rust)

Event/Stream 스키마는 **한 곳(`packages/core`)** 이 진실소스다. TS(zod)와 Rust 구조체가 어긋나면 인제스트가 깨지므로:
- `packages/core`의 **zod 스키마를 SSOT**로, TS 타입은 `z.infer`로 파생.
- Rust 측은 **zod → JSON Schema → Rust 타입 생성**(예: typify) 또는 계약 테스트로 동기화.
- 로컬 pre-commit/CI에서 **인제스트 계약 테스트**(샘플 페이로드가 양측 파싱 통과)로 드리프트 감지.
- 변경 순서 고정: `packages/core` 수정 → 파생물 재생성 → 마이그레이션 추가.

## 프로세스 모델

앱은 **3가지 역할**을 한 바이너리(Tauri) 안에서 수행한다:

```
┌─────────────────────────── LogRoom.app (Tauri) ───────────────────────────┐
│                                                                          │
│  [Rust core (src-tauri)]                     [React UI (webview)]        │
│   ├─ SQLite + FTS5 (logroom.db)   ◀── Tauri commands ──▶  타임라인/검색/digest │
│   ├─ 인제스트 서버 (axum, 127.0.0.1:PORT, 토큰)                            │
│   ├─ 파일 감시자 (notify) ─┐                                              │
│   └─ 이벤트 emit ──▶ webview (live update)                                │
│                          │                                               │
│  [트레이/메뉴바 아이콘] ── 상시 실행, 빠른 접근                              │
└──────────────────────────┼───────────────────────────────────────────────┘
                           │ 로컬(127.0.0.1)로만
        ┌──────────────────┼───────────────────────────────┐
        │                  │                                │
 [Claude Code hook]  [파일 감시 대상]                 [브라우저 확장]
  POST /v1/ingest     ~/.claude/projects/**/*.jsonl    (M3) gemini.google.com
                      ~/.kiro/sessions/cli/*.jsonl      POST /v1/ingest
                      Kiro IDE workspace-sessions
```

## 데이터 흐름 (캡처 → 열람)

1. **감지**: 파일 감시자가 세션 로그 변경 감지 / hook·확장이 POST.
2. **정규화**: 소스 raw → `packages/core`의 Event/Stream 스키마로 매핑.
3. **인제스트**: `POST /v1/ingest` → 서버가 stream upsert + event 삽입(중복 dedup).
4. **저장**: SQLite, FTS 트리거가 검색 인덱스 자동 갱신.
5. **push**: Rust가 `event-ingested` 이벤트 emit → UI가 실시간 갱신.
6. **열람**: UI는 Tauri command(`list_events_by_day`, `search_events`, `get_stream`)로 조회.

## 캡처 실행 모델 (중요)

- **파일 감시형**(Claude Code JSONL, Kiro): Rust 코어가 앱 실행 중 상시 감시 → **외부 프로세스 불필요**. 앱 꺼져 있던 동안의 로그는 **재실행 시 backfill**(파일 mtime/오프셋 기준).
- **push형**(Claude Code hook, 브라우저 확장): 외부에서 발생 즉시 POST. 앱이 꺼져 있으면 유실 → 그래서 Claude Code는 **hook(실시간) + JSONL(진실소스/backfill) 이중화**.
- 각 소스는 **idempotent**: `(source, external_id)` UNIQUE로 재수집해도 중복 없음.

## 배포 (M5+)

- Tauri bundler → macOS `.dmg`, Apple 공증(notarization).
- **Mac App Store 배포 안 함**: 샌드박스가 타 앱 세션파일 읽기를 차단 → 캡처 불가. 직접 배포(.dmg) + hardened runtime 필수. (ADR-0009)
- auto-update: Tauri updater 플러그인, 서명된 매니페스트를 정적 호스팅.
- Windows(.msi/nsis)는 판매 준비 단계에서 추가.

## 테스트 전략

"나중에 실행만" 하려면 캡처/DB 회귀를 잡는 골격이 필수:
- **파서 픽스처**: 각 소스의 **실제 세션 스냅샷(익명화)** 을 `packages/captures/*/fixtures/`에 저장 → 정규화 결과 스냅샷 테스트(Vitest). 소스 포맷 변경 즉시 감지.
- **마이그레이션 테스트**(Rust): 빈 DB→최신 적용 성공 + 각 버전 스키마 검증(다운그레이드 없음).
- **인제스트 계약 테스트**: `packages/core` zod ↔ Rust 파싱을 같은 샘플로 양방향 검증.
- **단위**: 레인 배치·자정 클리핑·TZ 변환 엣지 케이스.
- **E2E(후속)**: `tauri-driver`/Playwright로 캡처→표시→검색 스모크.
- 우선순위: 파서 픽스처 > 마이그레이션 > 계약 > 나머지 (기록 정확성 최우선).
