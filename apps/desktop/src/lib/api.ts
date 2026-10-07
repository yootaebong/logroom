import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import {
  disable as disableAutostartPlugin,
  enable as enableAutostartPlugin,
  isEnabled as isAutostartEnabledPlugin,
} from "@tauri-apps/plugin-autostart";
import type {
  CaptureConfig,
  CaptureConfigInfo,
  CaptureHealthResult,
  DailySummary,
  DbStats,
  DeleteEventsBeforeResult,
  DigestResult,
  ExportDataResult,
  GetStreamResult,
  ListEventsByDayResult,
  ListMyMessagesResult,
  PerformanceReview,
  PeriodStats,
  PeriodSummary,
  PeriodType,
  ResumeBriefing,
  ReviewPeriodType,
  ReviewProfile,
  ReviewProject,
  SearchHit,
  SummaryConfig,
  SummaryDetail,
  SummaryDetailScope,
  SummaryEngineStatus,
  UpdateInfo,
  VacuumDbResult,
} from "./types";

/** TanStack Query 쿼리키 접두어(실시간 invalidate에서도 재사용). */
export const QUERY_KEY_EVENTS_BY_DAY = "events-by-day";
export const QUERY_KEY_STREAM = "stream";
export const QUERY_KEY_SEARCH = "search";
export const QUERY_KEY_CAPTURE_CONFIG = "capture-config";
export const QUERY_KEY_DIGEST = "digest";
export const QUERY_KEY_CAPTURE_HEALTH = "capture-health";
export const QUERY_KEY_CAPTURE_PAUSED = "capture-paused";
export const QUERY_KEY_DB_STATS = "db-stats";
export const QUERY_KEY_AUTOSTART = "autostart";
export const QUERY_KEY_APP_VERSION = "app-version";
export const QUERY_KEY_CHECK_UPDATES_ENABLED = "check-updates-enabled";
export const QUERY_KEY_SUMMARY_CONFIG = "summary-config";
export const QUERY_KEY_SUMMARY_ENGINE_STATUS = "summary-engine-status";
export const QUERY_KEY_DAILY_SUMMARY = "daily-summary";
export const QUERY_KEY_SUMMARY_PREVIEW = "summary-preview";
export const QUERY_KEY_PERIOD_SUMMARY = "period-summary";
export const QUERY_KEY_PERIOD_STATS = "period-stats";
export const QUERY_KEY_PERIOD_SUMMARY_PREVIEW = "period-summary-preview";
export const QUERY_KEY_RESUME_BRIEFING_PREVIEW = "resume-briefing-preview";
export const QUERY_KEY_PROJECTS = "projects";
export const QUERY_KEY_MY_MESSAGES = "my-messages";
export const QUERY_KEY_SUMMARY_DETAIL = "summary-detail";
export const QUERY_KEY_PERFORMANCE_REVIEW = "performance-review";
export const QUERY_KEY_PERFORMANCE_REVIEW_PREVIEW = "performance-review-preview";
export const QUERY_KEY_REVIEW_PROJECTS = "review-projects";
export const QUERY_KEY_REVIEW_PROFILE = "review-profile";

export const queryKeys = {
  eventsByDay: (localDate: string, tz: string) => [QUERY_KEY_EVENTS_BY_DAY, localDate, tz] as const,
  myMessages: (localDate: string, tz: string) => [QUERY_KEY_MY_MESSAGES, localDate, tz] as const,
  // range 미지정 시 2-요소 키(prefix)를 유지해, rename 등에서 `queryKeys.stream(id)`로
  // invalidate하면 range가 붙은 변형(메시지형 스트림의 날짜별 캐시 항목)도 함께 무효화된다.
  stream: (id: string, range?: GetStreamRange) =>
    range ? ([QUERY_KEY_STREAM, id, range] as const) : ([QUERY_KEY_STREAM, id] as const),
  search: (query: string, params: SearchEventsParams = {}) =>
    [QUERY_KEY_SEARCH, query, params] as const,
  captureConfig: () => [QUERY_KEY_CAPTURE_CONFIG] as const,
  projects: () => [QUERY_KEY_PROJECTS] as const,
  digest: (localDate: string, tz: string) => [QUERY_KEY_DIGEST, localDate, tz] as const,
  captureHealth: () => [QUERY_KEY_CAPTURE_HEALTH] as const,
  capturePaused: () => [QUERY_KEY_CAPTURE_PAUSED] as const,
  dbStats: () => [QUERY_KEY_DB_STATS] as const,
  autostart: () => [QUERY_KEY_AUTOSTART] as const,
  appVersion: () => [QUERY_KEY_APP_VERSION] as const,
  checkUpdatesEnabled: () => [QUERY_KEY_CHECK_UPDATES_ENABLED] as const,
  summaryConfig: () => [QUERY_KEY_SUMMARY_CONFIG] as const,
  summaryEngineStatus: () => [QUERY_KEY_SUMMARY_ENGINE_STATUS] as const,
  dailySummary: (localDate: string, locale: string) =>
    [QUERY_KEY_DAILY_SUMMARY, localDate, locale] as const,
  summaryPreview: (localDate: string, locale: string) =>
    [QUERY_KEY_SUMMARY_PREVIEW, localDate, locale] as const,
  periodSummary: (periodType: PeriodType, periodKey: string, locale: string) =>
    [QUERY_KEY_PERIOD_SUMMARY, periodType, periodKey, locale] as const,
  periodStats: (periodType: PeriodType, periodKey: string, tz: string) =>
    [QUERY_KEY_PERIOD_STATS, periodType, periodKey, tz] as const,
  periodSummaryPreview: (periodType: PeriodType, periodKey: string, locale: string) =>
    [QUERY_KEY_PERIOD_SUMMARY_PREVIEW, periodType, periodKey, locale] as const,
  // 재개 브리핑 본문은 useMutation으로만 호출하므로 useQuery 캐시 키가 없다(미리보기만 useQuery).
  resumeBriefingPreview: (project: string, locale: string) =>
    [QUERY_KEY_RESUME_BRIEFING_PREVIEW, project, locale] as const,
  summaryDetail: (scope: SummaryDetailScope, scopeKey: string, locale: string) =>
    [QUERY_KEY_SUMMARY_DETAIL, scope, scopeKey, locale] as const,
  performanceReview: (periodType: ReviewPeriodType, periodKey: string, locale: string) =>
    [QUERY_KEY_PERFORMANCE_REVIEW, periodType, periodKey, locale] as const,
  // 미리보기는 입력(프로젝트·목표)이 바뀌면 발췌가 달라지므로 키에 함께 넣는다.
  performanceReviewPreview: (
    periodType: ReviewPeriodType,
    periodKey: string,
    locale: string,
    projects: string[] | null,
    goals: string | null,
  ) =>
    [QUERY_KEY_PERFORMANCE_REVIEW_PREVIEW, periodType, periodKey, locale, projects, goals] as const,
  reviewProjects: (periodType: ReviewPeriodType, periodKey: string, tz: string) =>
    [QUERY_KEY_REVIEW_PROJECTS, periodType, periodKey, tz] as const,
  reviewProfile: () => [QUERY_KEY_REVIEW_PROFILE] as const,
};

/** 특정 로컬 날짜(YYYY-MM-DD)의 스트림/이벤트 마커 조회 (docs/02 "하루" 정의 참고). */
export function listEventsByDay(localDate: string, tz: string): Promise<ListEventsByDayResult> {
  return invoke<ListEventsByDayResult>("list_events_by_day", { localDate, tz });
}

/** 특정 로컬 날짜(YYYY-MM-DD)에 내가 쓴 메시지(Slack/GitHub/Linear 원문) 조회 — 요약 화면 우측
 * 패널(DayMessagesPanel.tsx) 전용. `listEventsByDay`와 동일한 "하루" 정의(day_range_ms)를 쓴다. */
export function listMyMessagesByDay(localDate: string, tz: string): Promise<ListMyMessagesResult> {
  return invoke<ListMyMessagesResult>("list_my_messages_by_day", { localDate, tz });
}

/** search_events 필터(docs/05-ui-ux.md ② "필터: 소스, 기간, 타입"). 모두 선택이며 미지정/빈
 * 배열은 전체를 의미한다. `fromTs`/`toTs`는 `[fromTs, toTs)` 반열림 epoch ms 구간. */
export interface SearchEventsParams {
  sources?: string[];
  types?: string[];
  fromTs?: number;
  toTs?: number;
}

/** FTS5 전문검색. */
export function searchEvents(query: string, params: SearchEventsParams = {}): Promise<SearchHit[]> {
  return invoke<SearchHit[]>("search_events", {
    query,
    sources: params.sources,
    types: params.types,
    fromTs: params.fromTs,
    toTs: params.toTs,
  });
}

/** `getStream`의 이벤트 범위 필터 — `[fromTs, toTs)` 반열림 epoch ms 구간(메시지형 스트림의
 * "그날 메시지만" 조회, StreamDetailPanel.tsx 참고). 지정하지 않으면 전체 이벤트를 반환한다. */
export interface GetStreamRange {
  fromTs: number;
  toTs: number;
}

/** 스트림 상세 조회. 존재하지 않으면 null. `range`를 지정하면 이벤트를 해당 구간으로 좁힌다
 * (스트림 메타는 range와 무관하게 항상 전체 기준 — query.rs::get_stream 참고). */
export function getStream(id: string, range?: GetStreamRange): Promise<GetStreamResult | null> {
  return invoke<GetStreamResult | null>("get_stream", {
    id,
    fromTs: range?.fromTs,
    toTs: range?.toTs,
  });
}

/** 스트림 제목 수동 편집. trim 후 빈 문자열이면 BE가 에러를 던진다(db.rs::rename_stream).
 * 이후 재캡처가 흘러들어와도 이 title은 보호된다(metadata.manualTitle). */
export function renameStream(id: string, title: string): Promise<void> {
  return invoke<void>("rename_stream", { id, title });
}

/** 특정 로컬 날짜(YYYY-MM-DD)의 "오늘 한 일" 집계(요약/프로젝트별 스트림) 조회. */
export function getDigest(localDate: string, tz: string): Promise<DigestResult> {
  return invoke<DigestResult>("get_digest", { localDate, tz });
}

/** 전체 프로젝트 표시명 목록(요약 뷰 "재개" 스코프의 프로젝트 드롭다운, M7-② 발견성 개선) — 최근
 * 활동 내림차순으로 정렬돼 있다. 날짜 범위와 무관하게 DB 전체가 대상이다(getDigest의 그날 프로젝트
 * 목록과 다른 점). */
export function listProjects(): Promise<string[]> {
  return invoke<string[]>("list_projects");
}

/** 캡처 소스 root 설정 + 해석된 경로 목록 조회. */
export function getCaptureConfig(): Promise<CaptureConfigInfo> {
  return invoke<CaptureConfigInfo>("get_capture_config");
}

/** 캡처 소스 root 설정 저장(적용은 앱 재시작 후). */
export function setCaptureConfig(config: CaptureConfig): Promise<void> {
  return invoke<void>("set_capture_config", { config });
}

/** 앱 재시작. */
export function restartApp(): Promise<void> {
  return invoke<void>("restart_app");
}

/** 소스별(Claude Code/Kiro CLI) 캡처 헬스 조회(docs/03-capture.md "캡처 헬스"). */
export function getCaptureHealth(): Promise<CaptureHealthResult> {
  return invoke<CaptureHealthResult>("get_capture_health");
}

/** 캡처 일시정지(M4) 여부 조회. true면 파일감시/HTTP 인제스트 모두 저장 없이 진행된다
 * (docs/04-privacy-security.md "일시정지 의미론"). */
export function getCapturePaused(): Promise<boolean> {
  return invoke<boolean>("get_capture_paused");
}

/** 캡처 일시정지 토글. 재시작 없이 즉시 적용되며 `~/.logroom/config.json`에도 영구 반영된다. */
export function setCapturePaused(paused: boolean): Promise<void> {
  return invoke<void>("set_capture_paused", { paused });
}

/** DB 통계 조회(설정 다이얼로그 "데이터" 섹션, 읽기 전용). 다이얼로그를 열 때만 호출한다. */
export function getDbStats(): Promise<DbStats> {
  return invoke<DbStats>("get_db_stats");
}

/** 전체 데이터 내보내기(NDJSON + logroom.db 사본)를 `~/Downloads/logroom-export-<timestamp>/`에 생성한다. */
export function exportData(): Promise<ExportDataResult> {
  return invoke<ExportDataResult>("export_data");
}

/** 기간 삭제 미리보기: `ts`(epoch ms) 이전 이벤트 수. */
export function countEventsBefore(ts: number): Promise<number> {
  return invoke<number>("count_events_before", { ts });
}

/** 기간 삭제 실행: `ts`(epoch ms) 이전 이벤트를 영구 삭제한다(수동 실행 전용). */
export function deleteEventsBefore(ts: number): Promise<DeleteEventsBeforeResult> {
  return invoke<DeleteEventsBeforeResult>("delete_events_before", { ts });
}

/** DB 최적화(VACUUM + checkpoint). 수십 초 걸릴 수 있다. */
export function vacuumDb(): Promise<VacuumDbResult> {
  return invoke<VacuumDbResult>("vacuum_db");
}

/** 로그인 시 자동 실행(macOS LaunchAgent) 등록 여부 조회. `config.json`이 아니라 OS에 직접
 * 질의하며, 개발 모드(`pnpm dev`)에서는 등록 대상 실행 파일이 없어 항상 비활성으로 보인다. */
export function getAutostartEnabled(): Promise<boolean> {
  return isAutostartEnabledPlugin();
}

/** 로그인 시 자동 실행 활성화. 저장 버튼과 무관하게 즉시 OS에 등록된다. */
export function enableAutostart(): Promise<void> {
  return enableAutostartPlugin();
}

/** 로그인 시 자동 실행 비활성화. 즉시 OS 등록이 해제된다. */
export function disableAutostart(): Promise<void> {
  return disableAutostartPlugin();
}

/** 현재 앱 버전(설정 다이얼로그 "일반" 섹션 표시용). Tauri 코어 API — 커맨드 왕복 없이 직접 조회. */
export function getAppVersion(): Promise<string> {
  return getVersion();
}

/** 수동 업데이트 확인(M5, ADR-0013). 이미 최신 버전이면 `null`. 체크는 항상 버전 요청만(식별자 없음)이다. */
export function checkUpdate(): Promise<UpdateInfo | null> {
  return invoke<UpdateInfo | null>("check_update");
}

/** 업데이트 다운로드+설치. 진행률은 `update-progress` 이벤트로 emit되고, 완료 후 재시작은
 * 자동으로 하지 않는다 — 이 함수 성공 후 사용자가 확인하면 `restartApp()`을 별도 호출해야 한다. */
export function installUpdate(): Promise<void> {
  return invoke<void>("install_update");
}

/** 자동 업데이트 백그라운드 체크 사용 여부 조회(설정 다이얼로그 "일반" 섹션 체크박스). */
export function getCheckUpdatesEnabled(): Promise<boolean> {
  return invoke<boolean>("get_check_updates");
}

/** 자동 업데이트 백그라운드 체크 사용 여부 저장. 재시작 없이 다음 체크 주기부터 반영된다. */
export function setCheckUpdatesEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_check_updates", { enabled });
}

/** 트레이 메뉴 로케일 저장(i18n/index.ts가 해석한 "en"/"ko"만 전달 — "system"은 FE가 미리 해석해
 * 넘긴다). `~/.logroom/config.json`에 영구 저장되며, 트레이 메뉴 라벨은 다음 재시작부터 반영된다
 * (Settings "언어" 섹션 안내 문구 참고). */
export function setAppLocale(locale: "en" | "ko"): Promise<void> {
  return invoke<void>("set_app_locale", { locale });
}

/** 일일 AI 요약(M7-①, ADR-0016) 설정 조회. `apiKey`는 커넥터 토큰과 동일하게 마스킹된 값으로
 * 내려온다(변경 없이 그대로 저장해도 원본 키가 보존됨). */
export function getSummaryConfig(): Promise<SummaryConfig> {
  return invoke<SummaryConfig>("get_summary_config");
}

/** 일일 AI 요약 설정 저장. 재시작 없이 다음 생성 요청부터 반영된다. */
export function setSummaryConfig(config: SummaryConfig): Promise<void> {
  return invoke<void>("set_summary_config", { config });
}

/** 요약 엔진 가용성 진단. CLI 감지는 로컬 `--version` 실행뿐이라 네트워크 요청이 없다.
 * 후보 경로를 순회하므로 CLI가 하나도 없는 환경에서는 수백 ms 걸릴 수 있다. */
export function detectSummaryEngine(): Promise<SummaryEngineStatus> {
  return invoke<SummaryEngineStatus>("detect_summary_engine");
}

/** 캐시된 일일 요약 조회. 없으면 `null`(다이제스트 카드가 "생성" 상태를 표시). */
export function getDailySummary(localDate: string, locale: string): Promise<DailySummary | null> {
  return invoke<DailySummary | null>("get_daily_summary", { localDate, locale });
}

/** 일일 요약 생성(또는 캐시 반환). `force=true`면 캐시를 무시하고 재생성한다. 비활성 상태이거나
 * 캡처가 일시정지된 동안에는 BE가 에러를 던진다(프라이버시 계약 — 아웃바운드 중단 규칙). */
export function generateDailySummary(
  localDate: string,
  tz: string,
  locale: string,
  force: boolean,
  /** 미리보기 에디터에서 사용자가 수정한 발췌 — 있으면 자동 발췌 대신 이 내용으로 생성(1회성,
   * 저장되지 않음). BE에서 발송 직전 스크럽·길이 안전컷이 동일하게 적용된다. */
  excerptOverride: string | null = null,
): Promise<DailySummary> {
  return invoke<DailySummary>("generate_daily_summary", {
    localDate,
    tz,
    locale,
    force,
    excerptOverride,
  });
}

/** 요약 엔진에 실제로 전송될 발췌 텍스트 미리보기(생성/저장 없이 조회만). "전송 내용 미리보기"
 * 다이얼로그가 사용한다. */
export function previewSummaryInput(
  localDate: string,
  tz: string,
  locale: string,
): Promise<string> {
  return invoke<string>("preview_summary_input", { localDate, tz, locale });
}

/** 주간/월간 AI 요약 롤업(M7-③) 캐시 조회. 없으면 `null`(daily와 동일 계약). */
export function getPeriodSummary(
  periodType: PeriodType,
  periodKey: string,
  locale: string,
): Promise<PeriodSummary | null> {
  return invoke<PeriodSummary | null>("get_period_summary", { periodType, periodKey, locale });
}

/** 기간 팩트 집계(활동일·세션·프롬프트·GitHub·Linear·Slack 건수) 조회 — 요약이 없어도 조회 가능
 * (SummaryView.tsx 팩트 스트립). */
export function getPeriodStats(
  periodType: PeriodType,
  periodKey: string,
  tz: string,
): Promise<PeriodStats> {
  return invoke<PeriodStats>("get_period_stats", { periodType, periodKey, tz });
}

/** 주간/월간 요약 생성(또는 캐시 반환). `force=true`면 캐시를 무시하고 재생성한다.
 * `cascade=true`(수동 경로 기본)면 빠진 하위 요약을 **한 단계만** 생성해 합성한다(주간→일간 ≤7회,
 * 월간→주간 ≤6회 — 그 주간은 일간을 새로 만들지 않음). `cascade=false`(자동 캐치업의 월간)는
 * 캐시된 하위 요약만 롤업 — 콜드 스타트에서 수십 회 엔진 호출 팬아웃 방지(리뷰 Critical). */
export function generatePeriodSummary(
  periodType: PeriodType,
  periodKey: string,
  tz: string,
  locale: string,
  force: boolean,
  /** 미리보기 에디터에서 수정한 발췌 — 있으면 캐스케이드 합성 발췌 대신 이 내용으로 생성(1회성,
   * 저장되지 않음). */
  excerptOverride: string | null = null,
  cascade = true,
): Promise<PeriodSummary> {
  return invoke<PeriodSummary>("generate_period_summary", {
    periodType,
    periodKey,
    tz,
    locale,
    force,
    excerptOverride,
    cascade,
  });
}

/** 기간 요약 엔진에 실제로 전송될 합성 발췌 미리보기(생성/저장 없이 조회만). 하위 계층 캐스케이드
 * 생성은 그대로 수행된다 — "미리보기 = 실제 전송본" 계약(previewSummaryInput과 동일 원칙). */
export function previewPeriodSummaryInput(
  periodType: PeriodType,
  periodKey: string,
  tz: string,
  locale: string,
): Promise<string> {
  return invoke<string>("preview_period_summary_input", { periodType, periodKey, tz, locale });
}

/** 재개 브리핑(M7-②) 발췌 텍스트만 미리 본다(생성/저장 없이 조회만) — "전송 내용 미리보기"용. */
export function previewResumeInput(project: string, tz: string, locale: string): Promise<string> {
  return invoke<string>("preview_resume_input", { project, tz, locale });
}

/** 재개 브리핑 생성. daily/period와 달리 **캐시가 없다** — 호출할 때마다 최신 데이터로 새로
 * 생성한다(프로젝트 상태는 계속 바뀌므로). 비활성 상태이거나 캡처가 일시정지된 동안에는 BE가
 * 에러를 던진다(다른 요약 커맨드와 동일한 프라이버시 계약). */
export function generateResumeBriefing(
  project: string,
  tz: string,
  locale: string,
): Promise<ResumeBriefing> {
  return invoke<ResumeBriefing>("generate_resume_briefing", { project, tz, locale });
}

/** "상세 보기" 온디맨드 요약 캐시 조회. 없으면 `null`(daily/period와 동일 계약) — SummaryView.tsx의
 * "상세 보기" 버튼이 펼칠 때 호출한다. */
export function getSummaryDetail(
  scope: SummaryDetailScope,
  scopeKey: string,
  locale: string,
): Promise<SummaryDetail | null> {
  return invoke<SummaryDetail | null>("get_summary_detail", { scope, scopeKey, locale });
}

/** "상세 보기" 온디맨드 요약 생성(또는 캐시 반환). `force=true`면 캐시를 무시하고 재생성한다.
 * 재캡처 없이 기본 요약과 같은 발췌를 상세 전용 프롬프트로 재호출한다(비용은 호출할 때만 발생) —
 * 비활성 상태이거나 캡처가 일시정지된 동안에는 BE가 에러를 던진다(다른 요약 커맨드와 동일한
 * 프라이버시 계약). */
export function generateSummaryDetail(
  scope: SummaryDetailScope,
  scopeKey: string,
  tz: string,
  locale: string,
  force: boolean,
): Promise<SummaryDetail> {
  return invoke<SummaryDetail>("generate_summary_detail", { scope, scopeKey, tz, locale, force });
}

// 업무평가서(분기 평가·월간 점검, ADR-0017) — summary/review.rs 참고. 요약과 같은 enabled/캡처
// 일시정지 게이트를 따르고(BE가 에러 코드 summary_disabled/summary_paused로 거부), 에러 코드 집합도
// 요약과 공유한다(SummaryView.tsx::SUMMARY_ERROR_KEYS).

/** 평가서 캐시 조회. 없으면 `null`. */
export function getPerformanceReview(
  periodType: ReviewPeriodType,
  periodKey: string,
  locale: string,
): Promise<PerformanceReview | null> {
  return invoke<PerformanceReview | null>("get_performance_review", {
    periodType,
    periodKey,
    locale,
  });
}

/** 그 기간에 활동이 있었던 프로젝트 목록(활동 수 내림차순) — 평가서 "포함할 프로젝트" 체크리스트. */
export function listReviewProjects(
  periodType: ReviewPeriodType,
  periodKey: string,
  tz: string,
): Promise<ReviewProject[]> {
  return invoke<ReviewProject[]>("list_review_projects", { periodType, periodKey, tz });
}

/** 평가서 엔진에 실제로 전송될 발췌 미리보기. 빠진 하위 요약(분기→월간, 월간 점검→주간)은 이
 * 호출에서 생성·캐시된다 — "미리보기 = 실제 전송본" 계약(previewPeriodSummaryInput과 동일). */
export function previewPerformanceReviewInput(
  periodType: ReviewPeriodType,
  periodKey: string,
  tz: string,
  locale: string,
  /** 포함할 프로젝트 표시명. `null` = 전체. */
  projects: string[] | null,
  /** 기간 목표. `null` 또는 빈 문자열 = 입력 없음. */
  goals: string | null,
): Promise<string> {
  return invoke<string>("preview_performance_review_input", {
    periodType,
    periodKey,
    tz,
    locale,
    projects,
    goals,
  });
}

/** 평가서 생성 — 호출할 때마다 새로 만들어 캐시를 덮어쓴다(재생성 = 같은 호출). */
export function generatePerformanceReview(
  periodType: ReviewPeriodType,
  periodKey: string,
  tz: string,
  locale: string,
  projects: string[] | null,
  goals: string | null,
  /** 미리보기 에디터에서 수정한 발췌 — 있으면 자동 발췌 대신 이 내용으로 생성(1회성). */
  excerptOverride: string | null = null,
): Promise<PerformanceReview> {
  return invoke<PerformanceReview>("generate_performance_review", {
    periodType,
    periodKey,
    tz,
    locale,
    projects,
    goals,
    excerptOverride,
  });
}

/** 평가 프로필 조회(설정 파일에 없으면 전부 빈 값). */
export function getReviewProfile(): Promise<ReviewProfile> {
  return invoke<ReviewProfile>("get_review_profile");
}

/** 평가 프로필 저장. */
export function setReviewProfile(profile: ReviewProfile): Promise<void> {
  return invoke<void>("set_review_profile", { profile });
}
