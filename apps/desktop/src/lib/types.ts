import type { EventType, Source, StreamKind, StreamStatus } from "@logroom/core";

/**
 * Tauri 커맨드 응답 타입 (camelCase). docs/02-data-model.md의 streams/events 테이블에
 * 대응하되, 조회용으로 가공된 필드만 담는다. Rust 쪽 계약과 합의된 shape.
 */

/** list_events_by_day / get_stream 이 반환하는 스트림 행 */
export interface StreamRow {
  id: string;
  source: Source;
  kind: StreamKind;
  title: string | null;
  project: string | null;
  gitBranch: string | null;
  startedAt: number; // epoch ms
  endedAt: number | null; // epoch ms (진행중이면 null 가능)
  status: StreamStatus;
  metadata: Record<string, unknown>;
}

/** list_events_by_day 가 반환하는 타임라인 마커(이벤트 요약) */
export interface EventMarker {
  id: string;
  streamId: string;
  ts: number; // epoch ms
  type: EventType;
  title: string | null;
}

/** search_events 결과 1건 */
export interface SearchHit {
  eventId: string;
  streamId: string;
  ts: number; // epoch ms
  type: EventType;
  source: Source;
  title: string | null;
  snippet: string; // FTS5 snippet, `[]`가 하이라이트 마커
  streamTitle: string | null;
}

/** get_stream 이 반환하는 이벤트 전체 필드 */
export interface EventFull {
  id: string;
  streamId: string;
  ts: number; // epoch ms
  source: Source;
  type: EventType;
  title: string | null;
  body: string | null;
  model: string | null;
  tokensIn: number | null;
  tokensOut: number | null;
  url: string | null;
  parentId: string | null;
  externalId: string | null;
  metadata: Record<string, unknown>;
}

/** invoke("list_events_by_day", ...) 응답 */
export interface ListEventsByDayResult {
  streams: StreamRow[];
  events: EventMarker[];
}

/** list_my_messages_by_day 가 반환하는 "내가 쓴 메시지" 1건(Slack/GitHub/Linear 원문,
 * DayMessagesPanel.tsx). */
export interface DayMessage {
  id: string;
  streamId: string;
  ts: number; // epoch ms
  source: Source;
  title: string | null;
  body: string | null;
  url: string | null;
  streamTitle: string | null;
}

/** invoke("list_my_messages_by_day", ...) 응답 */
export interface ListMyMessagesResult {
  messages: DayMessage[];
}

/** invoke("get_stream", ...) 응답 */
export interface GetStreamResult {
  stream: StreamRow;
  events: EventFull[];
}

/** get_digest 가 반환하는 요약 카드 수치. */
export interface DigestTotals {
  streams: number;
  agentStreams: number;
  prompts: number;
  responses: number;
  toolEvents: number;
  tokensIn: number;
  tokensOut: number;
  firstTs: number | null; // epoch ms
  lastTs: number | null; // epoch ms
}

/** get_digest 프로젝트 섹션 본문에 나열되는 스트림 1건. */
export interface DigestStreamItem {
  streamId: string;
  title: string | null;
  source: Source;
  prompts: number;
  startedAt: number; // epoch ms
  endedAt: number; // epoch ms
}

/** get_digest 가 반환하는 프로젝트 단위 섹션 1건. */
export interface DigestProject {
  project: string | null;
  streams: number;
  prompts: number;
  tokensIn: number;
  tokensOut: number;
  firstTs: number; // epoch ms
  lastTs: number; // epoch ms
  sources: Source[];
  items: DigestStreamItem[];
}

/** invoke("get_digest", ...) 응답 */
export interface DigestResult {
  totals: DigestTotals;
  projects: DigestProject[];
}

/** get_capture_config / set_capture_config 이 다루는 캡처 소스 식별자(Source 서브셋). 파일감시
 * root 설정(claudeCode/kiroCli)에 한정된다 — Slack은 root 개념이 없어 별도 `SlackConfig`로 다룬다. */
export type CaptureSource = "claude_code" | "kiro_cli";

/** get_capture_health 가 다루는 소스 식별자. 파일감시(CaptureSource) + 폴러형(Slack/GitHub/Linear/Notion)을
 * 모두 포함한다(docs/08-connectors.md "헬스 판정", Rust `capture/health.rs::HealthSource`와 1:1 대응). */
export type HealthSource = CaptureSource | "slack" | "github" | "linear" | "notion";

/** 소스 1개(Claude Code 또는 Kiro CLI)에 대한 캡처 root 설정. */
export interface CaptureSourceConfig {
  extraRoots: string[];
  useDefaults: boolean;
  /** 이 소스 캡처 사용 여부. false면 재시작 후 이 소스의 root(기본+커스텀)가 감시 대상에서 전부 빠진다. */
  enabled: boolean;
}

/** 캡처 시 이벤트 body/metadata 저장 범위(ADR-0012 회고 중심 저장). "essential"(기본): tool_result/tool_use는
 * 요약만 저장. "full": 원문 그대로 저장. */
export type BodyPolicy = "essential" | "full";

/** Slack 커넥터(v1) 설정(docs/08-connectors.md, ADR-0015) — 로컬 폴링 + 수동 user token. `token`이
 * `null`이면 아직 발급받은 토큰을 붙여넣지 않은 상태. */
export interface SlackConfig {
  token: string | null;
  enabled: boolean;
  pollMinutes: number;
}

/** GitHub 커넥터(v1) 계정 1개(docs/08-connectors.md) — 다중 계정 지원이 Slack과의 핵심 차이.
 * `username`은 서버가 `GET /user`로 확인한 로그인명 — 아직 검증 전(또는 마지막 확인 이후 토큰이
 * 무효화됨)이면 `null`. `token`은 기존 토큰이 있으면 서버가 마스킹된 값(`"••••" + 마지막 4자`)으로
 * 내려준다(Slack과 동일한 왕복 보호 원칙, `get_capture_config`/`set_capture_config` 참고). */
export interface GithubAccount {
  token: string;
  username: string | null;
}

/** GitHub 커넥터(v1) 설정(docs/08-connectors.md) — 계정 여러 개를 등록할 수 있다. */
export interface GithubConfig {
  accounts: GithubAccount[];
  enabled: boolean;
  pollMinutes: number;
}

/** Linear 커넥터(v1) 계정(워크스페이스) 1개(docs/08-connectors.md) — GitHub와 동일한 다중 계정
 * 지원 패턴. `viewerId`/`viewerName`은 서버가 `viewer { id name email }`로 확인한 값 — 아직 검증
 * 전(또는 마지막 확인 이후 키가 무효화됨)이면 `null`. `token`은 기존 토큰이 있으면 서버가 마스킹된
 * 값(`"••••" + 마지막 4자`)으로 내려준다(GitHub와 동일한 왕복 보호 원칙). */
export interface LinearAccount {
  token: string;
  viewerId: string | null;
  viewerName: string | null;
}

/** Linear 커넥터(v1) 설정(docs/08-connectors.md) — 계정(워크스페이스) 여러 개를 등록할 수 있다. */
export interface LinearConfig {
  accounts: LinearAccount[];
  enabled: boolean;
  pollMinutes: number;
}

/** Notion 토큰 종류 — 서버가 `GET /v1/users/me`로 판별한다(사용자가 고르지 않는다).
 * `personal`: Personal Access Token — 내가 마지막으로 편집한 페이지만 기록한다.
 * `integration`: Internal Integration Secret — 연결한 페이지의 모든 편집을 기록한다. */
export type NotionTokenKind = "personal" | "integration";

/** Notion 커넥터(v1) 계정 1개(docs/08-connectors.md) — GitHub/Linear와 동일한 다중 계정 지원 패턴.
 * `id`는 서버가 저장할 때 부여하는 로컬 식별자 — 새로 추가한 계정은 `null`로 보내고, 기존 계정은
 * 받은 값을 그대로 돌려보내야 한다(토큰이 마스킹돼 있어 이 값으로 원본을 찾는다). `label`은 사용자가
 * 붙인 이름("회사", "개인") — PAT는 워크스페이스 이름을 알려 주지 않아 이 이름으로 계정을 구분한다.
 * `kind`/`userName`/`workspaceId`/`workspaceName`은 서버가 토큰을 검증한 뒤 채운다 — 아직 검증
 * 전이면 `null`. `token`은 기존 토큰이 있으면 서버가 마스킹된 값(`"••••" + 마지막 4자`)으로
 * 내려준다(GitHub/Linear와 동일한 왕복 보호 원칙). */
export interface NotionAccount {
  token: string;
  id: string | null;
  label: string | null;
  kind: NotionTokenKind | null;
  userName: string | null;
  workspaceId: string | null;
  workspaceName: string | null;
}

/** Notion 커넥터(v1) 설정(docs/08-connectors.md) — 개인·회사 워크스페이스 계정을 여러 개 등록할 수
 * 있다. 주의: Internal Integration Secret은 페이지마다 연결을 붙여야 접근이 생기고, 연결이 없으면
 * 검색이 빈 배열을 돌려주며 오류도 나지 않는다(설정 화면 가이드 참고). */
export interface NotionConfig {
  accounts: NotionAccount[];
  enabled: boolean;
  pollMinutes: number;
}

/** get_capture_config / set_capture_config 이 주고받는 전체 설정. */
export interface CaptureConfig {
  claudeCode: CaptureSourceConfig;
  kiroCli: CaptureSourceConfig;
  bodyPolicy: BodyPolicy;
  /** 캡처에서 제외할 프로젝트 절대경로 prefix 목록. 매치되는 프로젝트의 활동은 재시작 후부터
   * 기록되지 않는다(소급 삭제 아님). */
  excludeProjects: string[];
  slack: SlackConfig;
  github: GithubConfig;
  linear: LinearConfig;
  notion: NotionConfig;
}

/** config를 실제 경로로 해석한 결과 1건(기본/커스텀 root 구분 + 존재 여부). */
export interface ResolvedCaptureRoot {
  source: CaptureSource;
  path: string;
  origin: "default" | "custom";
  exists: boolean;
}

/** invoke("get_capture_config") 응답 */
export interface CaptureConfigInfo {
  config: CaptureConfig;
  resolvedRoots: ResolvedCaptureRoot[];
}

/** get_capture_health 가 매기는 소스별 캡처 상태(docs/03-capture.md "캡처 헬스").
 * "ok": 정상. "stale": 파일은 갱신되는데 캡처가 안 따라감(감시 실패 의심). "inactive": 감시 root 없음(도구 미설치). */
export type CaptureHealthStatus = "ok" | "stale" | "inactive";

/** get_capture_health 가 반환하는 소스 1건의 헬스 정보. `roots`는 파일감시 소스(Claude/Kiro)에서는
 * 감시 중인 root 개수, Slack(폴러형)에서는 "설정됨"(1)/"미설정"(0)을 의미한다(root 개념이 없어
 * 같은 필드를 재사용 — docs/08-connectors.md "헬스 판정"). */
export interface SourceHealth {
  source: HealthSource;
  roots: number;
  lastCapturedAt: number | null; // epoch ms
  latestFileMtime: number | null; // epoch ms
  status: CaptureHealthStatus;
}

/** invoke("get_capture_health") 응답 */
export interface CaptureHealthResult {
  sources: SourceHealth[];
}

/** invoke("get_db_stats") 응답 — 설정 다이얼로그 "데이터" 섹션(읽기 전용)이 표시하는 DB 통계. */
export interface DbStats {
  dbSizeBytes: number;
  walSizeBytes: number;
  events: number;
  streams: number;
  oldestTs: number | null; // epoch ms
  newestTs: number | null; // epoch ms
}

/** invoke("export_data") 응답 — 설정 다이얼로그 "데이터" 섹션의 내보내기 버튼. */
export interface ExportDataResult {
  dir: string;
  streams: number;
  events: number;
}

/** invoke("delete_events_before") 응답 — 기간 삭제 실행 결과. */
export interface DeleteEventsBeforeResult {
  deletedEvents: number;
  deletedStreams: number;
}

/** invoke("vacuum_db") 응답 — DB 최적화(VACUUM) 전/후 파일 크기(본체 + WAL, bytes). */
export interface VacuumDbResult {
  beforeBytes: number;
  afterBytes: number;
}

/** 일일 AI 요약(M7-①, ADR-0016) 엔진 선택 — "자동(감지 시 우선)/CLI/API 키" 3택. 어떤 CLI/API를
 * 쓸지는 이 값과 별개인 {@link CliProvider}/{@link ApiProvider}가 정한다(모드 × 프로바이더 조합). */
export type SummaryEngine = "auto" | "cli" | "api";

/** `engine`이 cli(또는 auto)일 때 실제로 실행할 CLI 프로바이더. */
export type CliProvider = "claude" | "gemini" | "codex";

/** `engine`이 api(또는 auto)일 때 실제로 호출할 API 프로바이더. 각각 대응하는 키 필드
 * (`anthropicApiKey`/`openaiApiKey`/`geminiApiKey`)를 쓴다. */
export type ApiProvider = "anthropic" | "openai" | "gemini";

/** get_summary_config / set_summary_config 이 주고받는 요약 설정. 3개 API 키 각각 커넥터 토큰과
 * 동일한 마스킹 왕복 원칙(`"••••" + 마지막 4자`)을 독립적으로 따른다 — 기존 키가 있으면 서버가
 * 마스킹된 값으로 내려주고, 그 값을 변경 없이 그대로 저장해도 원본 키가 보존된다. */
export interface SummaryConfig {
  enabled: boolean;
  engine: SummaryEngine;
  cliProvider: CliProvider;
  apiProvider: ApiProvider;
  anthropicApiKey: string | null;
  openaiApiKey: string | null;
  geminiApiKey: string | null;
  cliModel: string;
  apiModel: string;
  /** CLI 백엔드에 주입할 CLAUDE_CONFIG_DIR(선택) — **Claude CLI 전용**(Gemini/Codex CLI는 별도 설정
   * 디렉토리 개념이 없다). 셸 alias 로 CLAUDE_CONFIG_DIR 을 나눠 쓰면 앱이 실행하는 claude가 기본
   * ~/.claude로 붙어 미로그인 실패하므로 로그인된 디렉토리를 지정한다. */
  cliConfigDir: string | null;
  /** 매일 자동 생성(캐치업형) — 어제 날짜 요약이 없으면 주기적으로 생성(useAutoDailySummary). */
  autoGenerate: boolean;
}

/** detect_summary_engine 이 반환하는 요약 엔진 가용성 진단.
 *
 * 온보딩 opt-in 스텝이 "켜면 실제로 동작하는가"를 미리 보여주는 데 쓴다. **키 원문은 담지 않고
 * 보유 여부만** 내려온다. `resolved`는 BE의 decide_engine 결과(`"cli:claude"` 등)이며 쓸 수 있는
 * 엔진이 없으면 `null` — 판정 규칙을 FE에 복제하지 않으려고 BE가 계산해 내려준다. */
export interface SummaryEngineStatus {
  cli: { claude: boolean; gemini: boolean; codex: boolean };
  apiKeys: { anthropic: boolean; openai: boolean; gemini: boolean };
  resolved: string | null;
}

/** get_daily_summary / generate_daily_summary 가 반환하는 캐시된 요약 1건. */
export interface DailySummary {
  localDate: string;
  locale: string;
  tz: string;
  /** 실제로 생성에 쓰인 엔진("auto"가 해석된 뒤의 값) — "cli" | "api". */
  engine: string;
  model: string;
  content: string;
  createdAt: number; // epoch ms
  /** 캐시를 만든 프롬프트 버전이 현재 버전(summary/prompts.rs PROMPT_VERSION)과 다르면 true —
   * SummaryView 가 열 때 한 번 자동 재생성한다. */
  stale: boolean;
}

/** 주간/월간 AI 요약 롤업(M7-③) 기간 단위. `periodKey` 형식: week="그 주 월요일 YYYY-MM-DD" /
 * month="YYYY-MM"(store.ts::localDateToWeekKey/localDateToMonthKey 참고). */
export type PeriodType = "week" | "month";

/** get_period_summary / generate_period_summary 가 반환하는 캐시된 기간 요약 1건
 * (`DailySummary`와 동일한 shape에 periodType/periodKey만 추가). */
export interface PeriodSummary {
  periodType: PeriodType;
  periodKey: string;
  locale: string;
  tz: string;
  engine: string;
  model: string;
  content: string;
  createdAt: number; // epoch ms
  /** 캐시를 만든 프롬프트 버전이 현재 버전(summary/prompts.rs PROMPT_VERSION)과 다르면 true —
   * SummaryView 가 열 때 한 번 자동 재생성한다. */
  stale: boolean;
}

/** get_period_stats 가 반환하는 기간 팩트 집계 — 요약 유무와 무관하게 항상 조회 가능하다
 * (SummaryView.tsx 팩트 스트립, summary/period.rs::get_period_stats 참고). */
export interface PeriodStats {
  activeDays: number;
  sessions: number;
  prompts: number;
  githubEvents: number;
  linearIssues: number;
  slackMessages: number;
}

/** "상세 보기" 온디맨드 요약(get_summary_detail/generate_summary_detail) 대상 scope — 일간/주간/
 * 월간 3종. `SummaryView.tsx`의 `SummaryScope`(재개 포함 UI 전용 유니온)와는 별도 정의다 — 재개
 * 브리핑은 상세 보기 대상이 아니라 그쪽 유니온을 그대로 재사용할 수 없다. */
export type SummaryDetailScope = "day" | "week" | "month";

/** get_summary_detail / generate_summary_detail 이 반환하는 캐시된 상세 요약 1건. `DailySummary`/
 * `PeriodSummary`와 동일한 shape에 scope/scopeKey만 대응한다(scope_key: day는 로컬 날짜, week·
 * month는 periodKey). */
export interface SummaryDetail {
  scope: string;
  scopeKey: string;
  locale: string;
  tz: string;
  engine: string;
  model: string;
  content: string;
  createdAt: number; // epoch ms
  /** 캐시를 만든 프롬프트 버전이 현재 버전(summary/prompts.rs PROMPT_VERSION)과 다르면 true —
   * SummaryView 가 열 때 한 번 자동 재생성한다. */
  stale: boolean;
}

/** 업무평가서 기간 단위 — 분기 평가("quarter")와 월간 점검("month"). `periodKey` 형식: quarter=
 * "YYYY-Qn"(store.ts::localDateToQuarterKey) / month="YYYY-MM"(주간·월간 요약의 월 키와 동일). */
export type ReviewPeriodType = "quarter" | "month";

/** 평가 프로필의 연차 구간. 빈 문자열 = 미지정(공통 기준으로 평가). */
export type ReviewLevel = "" | "junior" | "mid" | "senior" | "lead";

/** 평가 프로필(설정 → AI 요약 → 평가 프로필, `~/.logroom/config.json`의 `reviewProfile`). 전부
 * 선택 항목이다. 평가서 발췌에 그대로 들어가 AI로 전송되므로(미리보기로 확인 가능) **연봉 같은 민감
 * 정보는 받지 않는다**(ADR-0017). */
export interface ReviewProfile {
  /** 직무 자유 입력(예: "프론트엔드 개발자"). 빈 문자열 = 미지정. */
  role: string;
  level: ReviewLevel;
  managesPeople: boolean;
}

/** 평가서 1건을 만들 때 쓴 입력 — 캐시 행에 함께 저장돼 "무엇으로 만든 평가서인지"를 보여준다. */
export interface ReviewInputs {
  /** 포함한 프로젝트 표시명. `null` = 전체. */
  projects: string[] | null;
  /** 사용자가 적은 기간 목표. `null` = 입력 없음(기록만으로 평가). */
  goals: string | null;
  /** 생성 당시 프로필 스냅샷. */
  profile: ReviewProfile;
}

/** get_performance_review / generate_performance_review 가 반환하는 평가서 1건. */
export interface PerformanceReview {
  periodType: ReviewPeriodType;
  periodKey: string;
  locale: string;
  tz: string;
  engine: string;
  model: string;
  content: string;
  inputs: ReviewInputs;
  /** 생성 시점의 DB 집계 스냅샷(숫자 타일·차트). 스냅샷이 없으면 `null`. */
  stats: ReviewStats | null;
  promptVersion: string;
  createdAt: number; // epoch ms
}

/** 평가서 활동 추이 한 칸 — 분기는 주(`key` = 그 주 월요일), 월간 점검은 일(`key` = 그 날). */
export interface ReviewSeriesPoint {
  key: string;
  /** 내 요청(AI 세션). */
  requests: number;
  /** Slack·GitHub·Linear 활동. */
  toolActivity: number;
  /** Linear 이슈 완료. */
  completed: number;
}

/** 평가서 생성 시점의 DB 집계(summary/review.rs::snapshot_json). 화면의 숫자 타일·활동 추이·프로젝트
 * 비중 차트가 쓴다 — **평가 근거가 아니라 맥락**이다(AI는 활동량으로 판단하지 않는다, ADR-0017). */
export interface ReviewStats {
  activeDays: number;
  sessions: number;
  prompts: number;
  githubEvents: number;
  linearIssues: number;
  slackMessages: number;
  linear: {
    touched: number;
    completed: number;
    canceled: number;
    carryOver: number;
    stalled: number;
    /** 시작→완료 중앙값(0.1일 단위). 완료 이슈가 없으면 `null`. */
    medianCycleTenths: number | null;
  };
  github: {
    prsOpened: number;
    prsMerged: number;
    issuesOpened: number;
    issuesClosed: number;
    commentsReviews: number;
  };
  series: { unit: "week" | "day"; points: ReviewSeriesPoint[] };
  /** 프로젝트별 내 요청·메시지 수(내림차순). */
  projects: ReviewProject[];
  /** 요일(월=0)×시(0~23)별 내 요청·메시지 수(로컬 시각). "시간·요일 분포" 옵션(기본 꺼짐)이 쓴다. */
  rhythm?: number[][];
}

/** list_review_projects 가 반환하는 기간 내 활동 프로젝트 1건(프로젝트 선택 체크리스트용). */
export interface ReviewProject {
  /** 표시명(마지막 경로 세그먼트 — 요약의 `## {프로젝트}` 섹션 이름과 동일). */
  name: string;
  /** 그 기간 내 내 요청·메시지 수(정렬·표시용). */
  activity: number;
}

/** generate_resume_briefing 이 반환하는 재개 브리핑 1건(M7-②) — daily/period와 달리 `createdAt`이
 * "캐시된 시각"이 아니라 "이번 호출로 생성된 시각"이다(캐시 없음, 항상 온디맨드 최신 생성). */
export interface ResumeBriefing {
  project: string;
  content: string;
  engine: string;
  model: string;
  createdAt: number; // epoch ms
}

/** invoke("check_update") 응답 / "update-available" 이벤트 payload(M5, ADR-0013). 이미 최신
 * 버전이면 `check_update`는 `null`을 반환한다 — 이 타입 자체가 "업데이트 있음"을 의미한다. */
export interface UpdateInfo {
  available: boolean;
  version: string;
  notes: string | null;
  date: number | null; // epoch ms
}

/** "update-progress" 이벤트 payload — `install_update` 진행 중 누적 다운로드 바이트. */
export interface UpdateProgress {
  downloaded: number;
  total: number | null;
}
