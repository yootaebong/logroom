import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import {
  Check,
  ChevronLeft,
  ChevronRight,
  Copy,
  PanelRight,
  RefreshCw,
  Sparkles,
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { DAY_MESSAGES_HEADING_ID, DayMessagesPanel } from "@/components/DayMessagesPanel";
import { ReviewPanel } from "@/components/ReviewPanel";
import {
  isNoDataError,
  PreviewDialog,
  type PreviewTarget,
  resumeErrorKey,
  SummaryContent,
  summaryErrorKey,
} from "@/components/SummaryShared";
import { Button } from "@/components/ui/button";
import type { ResolvedLocale, UiKey } from "@/i18n";
import { useResolvedLocale, useT } from "@/i18n";
import {
  generateDailySummary,
  generatePeriodSummary,
  generateResumeBriefing,
  generateSummaryDetail,
  getDailySummary,
  getPeriodStats,
  getPeriodSummary,
  getSummaryConfig,
  listProjects,
  queryKeys,
} from "@/lib/api";
import type { PeriodStats, PeriodType, SummaryDetailScope } from "@/lib/types";
import { cn } from "@/lib/utils";
import {
  LOCAL_TIMEZONE,
  localDateToMonthKey,
  localDateToWeekKey,
  shiftLocalDate,
  shiftLocalMonth,
  useAppStore,
} from "@/store";

/** "복사됨" 표시가 유지되는 시간(ms) — 재개 스코프 복사 버튼 피드백(구 ResumeBriefingDialog와 동일). */
const COPIED_NOTICE_MS = 2000;

/** localStorage 키 — "내가 쓴 메시지" 패널(DayMessagesPanel.tsx) 열림 상태 persist. **기본은 열림**. */
const MESSAGES_OPEN_STORAGE_KEY = "logroom-summary-messages-open";

/** Tailwind `lg` 브레이크포인트(px) — 이 폭 미만이면 패널이 오버레이 드로어로 뜨고 Esc로 닫힌다. */
const LG_BREAKPOINT_PX = 1024;

/** "상세 보기" 토글 버튼의 `aria-controls` 대상 id — 버튼과 펼쳐지는 영역을 연결해 스크린리더가
 * 둘의 관계를 인지하게 한다(a11y 감사 지적, WCAG 1.3.1). */
const SUMMARY_DETAIL_SECTION_ID = "summary-detail-section";

/** 기본은 열림 — 요약 옆에 그날 쓴 메시지 원문이 바로 보이는 게 기본 경험이다(사용자 결정).
 * 저장값이 `"false"`일 때만 닫는다: 즉 **사용자가 명시적으로 끈 경우에만** 닫힌 상태가 유지되고,
 * 저장값이 없는 최초 실행/신규 사용자는 열린 상태로 시작한다(`=== "true"` 비교였다면 최초 실행이
 * 닫힘이 되어 패널의 존재 자체를 모르고 지나칠 수 있다). */
function getInitialMessagesOpen(): boolean {
  return window.localStorage.getItem(MESSAGES_OPEN_STORAGE_KEY) !== "false";
}

/** 옛 프롬프트 버전 캐시 자동 재생성을 이미 시도한 대상 키(`summary:`/`detail:` 접두) — 모듈 스코프라
 * 뷰 언마운트·스코프 전환 뒤에도 앱 세션 동안 유지된다. 실패한 대상도 남겨 다시 시도하지 않는다(수동
 * "재생성" 버튼은 그대로 쓸 수 있다). */
const autoRegenAttempted = new Set<string>();

/** 옛 프롬프트 버전으로 만든 캐시(`stale`)를 열 때 자동 재생성할지 판정한다 — 엔진이 켜져 있고
 * 이 앱 세션에서 같은 대상(키)을 아직 시도하지 않았을 때만 true. 실패해도 같은 키는 다시 시도하지
 * 않는다(아웃바운드 반복 방지, useAutoDailySummary 의 attemptedRef 와 같은 원칙). */
function shouldAutoRegenerate(
  cached: { stale: boolean } | null | undefined,
  engineEnabled: boolean,
  attempted: ReadonlySet<string>,
  key: string,
): boolean {
  return cached?.stale === true && engineEnabled && !attempted.has(key);
}

function formatGeneratedAt(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** 일간/주간/월간/재개/평가 스코프 전환. "재개"(resume, M7-② 발견성 개선)는 날짜/기간이 아니라
 * 프로젝트 단위 브리핑이라 나머지 스코프와 데이터 모델이 다르다(아래 컴포넌트 곳곳의
 * `scope === "resume"` 분기 참고). "평가"(review, ADR-0017)는 날짜/기간 네비·통계 스트립·본문을
 * 전부 `ReviewPanel`이 자체적으로 그리므로(분기 평가/월간 점검 세그먼트가 따로 있음) 이 뷰에서는
 * 헤더 아래를 통째로 위임한다(`scope === "review"` 분기 참고). */
type SummaryScope = "day" | "week" | "month" | "resume" | "review";

const SCOPE_OPTIONS: Array<{ value: SummaryScope; labelKey: UiKey }> = [
  { value: "day", labelKey: "summary.view.scope.day" },
  { value: "week", labelKey: "summary.view.scope.week" },
  { value: "month", labelKey: "summary.view.scope.month" },
  { value: "resume", labelKey: "summary.view.scope.resume" },
  { value: "review", labelKey: "summary.view.scope.review" },
];

/** 스코프 세그먼트 토글(헤더 [일간|주간|월간]). */
function ScopeToggle({
  scope,
  onChange,
}: {
  scope: SummaryScope;
  onChange: (scope: SummaryScope) => void;
}) {
  const t = useT();
  return (
    <fieldset className="flex items-center gap-0.5 rounded-md border border-border p-0.5">
      <legend className="sr-only">{t("summary.view.scopeGroupAriaLabel")}</legend>
      {SCOPE_OPTIONS.map((option) => (
        <button
          key={option.value}
          type="button"
          aria-pressed={scope === option.value}
          onClick={() => onChange(option.value)}
          className={cn(
            "rounded px-2 py-1 text-xs font-medium transition-colors",
            scope === option.value
              ? "bg-accent text-accent-foreground"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          {t(option.labelKey)}
        </button>
      ))}
    </fieldset>
  );
}

/** "M/D" 표기(로케일 무관 — 요일 없이 숫자만이라 en/ko가 같은 표기로 충분하다). */
function formatMonthDay(localDate: string): string {
  const [, month, day] = localDate.split("-").map(Number);
  return `${month}/${day}`;
}

/** 주간 스코프 기간 라벨: "7/13 ~ 7/19"(periodKey = 그 주 월요일). */
function formatWeekRangeLabel(periodKey: string): string {
  return `${formatMonthDay(periodKey)} ~ ${formatMonthDay(shiftLocalDate(periodKey, 6))}`;
}

/** 월간 스코프 기간 라벨(Sidebar.tsx `formatSidebarDate`와 동일한 "locale 분기 + 하드코딩 템플릿"
 * 패턴 — 날짜 포맷은 discrete UI 문자열이 아니라 서식이라 i18n 테이블 대신 여기서 직접 분기한다).
 * ko: "2026년 7월", en: "July 2026". */
function formatMonthLabel(periodKey: string, locale: ResolvedLocale): string {
  const [yearStr, monthStr] = periodKey.split("-");
  const year = Number(yearStr);
  const monthIndex = Number(monthStr) - 1;
  if (locale === "ko") return `${year}년 ${monthIndex + 1}월`;
  const monthName = new Date(year, monthIndex, 1).toLocaleDateString("en-US", { month: "long" });
  return `${monthName} ${year}`;
}

/** 스코프+currentDate로부터 BE periodKey를 계산한다("day" 스코프는 기간 개념이 없어 `null`). */
function periodKeyFor(scope: SummaryScope, currentDate: string): string | null {
  if (scope === "week") return localDateToWeekKey(currentDate);
  if (scope === "month") return localDateToMonthKey(currentDate);
  return null;
}

const STATS_ITEMS: Array<{ key: keyof PeriodStats; labelKey: UiKey }> = [
  { key: "activeDays", labelKey: "summary.view.stats.activeDays" },
  { key: "sessions", labelKey: "summary.view.stats.sessions" },
  { key: "prompts", labelKey: "summary.view.stats.prompts" },
  { key: "githubEvents", labelKey: "summary.view.stats.githubEvents" },
  { key: "linearIssues", labelKey: "summary.view.stats.linearIssues" },
  { key: "slackMessages", labelKey: "summary.view.stats.slackMessages" },
];

/** 주간/월간 스코프 전용 팩트 스트립 — 요약 생성 여부와 무관하게 항상 표시된다(설계 원칙 "숫자는
 * DB, 서사는 LLM": get_period_stats는 SQL 직접 집계라 요약이 없어도 조회 가능). */
function PeriodStatsStrip({ stats }: { stats: PeriodStats }) {
  const t = useT();
  return (
    <div className="mt-4 grid grid-cols-3 gap-2 rounded-md border border-border p-3 sm:grid-cols-6">
      {STATS_ITEMS.map(({ key, labelKey }) => (
        <div key={key} className="text-center">
          <p className="text-base font-semibold tabular-nums">{stats[key]}</p>
          <p className="text-[11px] text-muted-foreground">{t(labelKey)}</p>
        </div>
      ))}
    </div>
  );
}

/** 재개 스코프 전용 프로젝트 드롭다운(M7-② 발견성 개선) — `list_projects`(최근 활동 내림차순, DB
 * 전체 대상)로 선택 가능한 프로젝트 목록을 채운다. 날짜 네비(◀ ▶)를 대체하는 위치에 표시된다. */
function ResumeProjectSelect({
  project,
  onChange,
}: {
  project: string | null;
  onChange: (project: string | null) => void;
}) {
  const t = useT();
  const { data: projects, isLoading } = useQuery({
    queryKey: queryKeys.projects(),
    queryFn: listProjects,
  });

  return (
    <select
      aria-label={t("summary.view.resumeProjectSelectAriaLabel")}
      value={project ?? ""}
      disabled={isLoading}
      onChange={(event) => onChange(event.target.value === "" ? null : event.target.value)}
      className="mt-1 w-full max-w-xs rounded-md border border-border bg-transparent px-2 py-1.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <option value="" disabled>
        {t("summary.view.resumeProjectSelectPlaceholder")}
      </option>
      {(projects ?? []).map((name) => (
        <option key={name} value={name}>
          {name}
        </option>
      ))}
    </select>
  );
}

/**
 * AI 요약 전용 뷰(M7-①/③, 재개 스코프는 M7-②) — 처음엔 다이제스트 상단 카드(DailySummaryCard)였지만
 * "읽는 콘텐츠"라 훑기용 다이제스트와 밀도가 충돌해(실사용 피드백) 사이드바 뷰로 분리했다. 다이제스트
 * 에는 이 뷰로 오는 바로가기 한 줄만 남는다.
 *
 * 스코프(일간/주간/월간/재개)는 로컬 state이며 기본값은 일간이다. 일간/주간/월간의 periodKey는 전역
 * currentDate에서 파생한다(`periodKeyFor`) — 일간은 currentDate 그대로, 주간은 그 주의 월요일, 월간은
 * "YYYY-MM". 헤더의 스코프 전용 ◀ ▶는 currentDate를 ±7일/±1개월 이동한다(store의 `shiftLocalDate`/
 * `shiftLocalMonth` 재사용). 전역 키보드 ←/→(useGlobalKeyboardNav)는 스코프와 무관하게 항상 하루
 * 단위로 currentDate를 옮긴다 — 일간 네비(App.tsx 헤더)와 동일한 단축키를 공유하기 위한 의도적
 * 설계이며, 주간/월간 스코프에서는 이 뷰의 ◀ ▶ 버튼이 별도의 기간 단위 이동을 담당한다.
 *
 * 재개(resume) 스코프는 날짜/기간이 아니라 **프로젝트** 단위라 데이터 모델이 다르다(구
 * ResumeBriefingDialog.tsx를 이 뷰로 통합 — 사이드바 아이콘 진입점은 발견성이 낮아 제거). 날짜 네비
 * 대신 프로젝트 드롭다운(`ResumeProjectSelect`)을 보여주고, 선택 시 `generateResumeBriefing`을
 * useMutation으로 호출해 결과를 인라인(SummaryContent 재사용) 표시한다. daily/period와 달리 캐시가
 * 없어 `useQuery` 대신 "선택 시 1회 트리거"가 명확한 `useMutation` 모델을 그대로 재사용했다.
 */
export function SummaryView() {
  const t = useT();
  const locale = useResolvedLocale();
  const currentDate = useAppStore((state) => state.currentDate);
  const setCurrentDate = useAppStore((state) => state.setCurrentDate);
  const setSettingsOpen = useAppStore((state) => state.setSettingsOpen);
  // App.tsx의 우측 드로어(StreamDetailPanel)와 동일한 store 상태 — 스트림 상세가 열려 있으면
  // 같은 우측 영역을 두고 겹치므로 표시를 억제한다(리뷰 지적, 레이아웃 충돌).
  const selectedStreamId = useAppStore((state) => state.selectedStreamId);
  const queryClient = useQueryClient();
  const [previewOpen, setPreviewOpen] = useState(false);
  const [scope, setScopeState] = useState<SummaryScope>("day");
  const [resumeProject, setResumeProject] = useState<string | null>(null);
  const [resumeCopied, setResumeCopied] = useState(false);
  const [messagesOpen, setMessagesOpen] = useState(getInitialMessagesOpen);
  // "상세 보기"(온디맨드) 펼침 상태 — 접었다 펴도 재호출하지 않도록 결과는 detailMutation.data에
  // 그대로 남겨두고 이 플래그만 토글한다(명세).
  const [detailOpen, setDetailOpen] = useState(false);

  const periodType: PeriodType = scope === "month" ? "month" : "week";
  const periodKey = periodKeyFor(scope, currentDate) ?? currentDate;
  const isResume = scope === "resume";
  const isPeriod = scope === "week" || scope === "month";
  // 재개(resume)·평가(review) 스코프는 "상세 보기" 대상이 아니다(명세) — 이 값이 null이면 버튼
  // 자체가 숨는 분기 안이라 실사용되지 않지만, 타입 차원에서도 안전하게 좁혀 둔다.
  const detailScope: SummaryDetailScope | null =
    scope === "day" || scope === "week" || scope === "month" ? scope : null;
  const isReview = scope === "review";
  // "내가 쓴 메시지" 패널은 일간 스코프 전용 — 주간/월간/재개/평가는 날짜 하루 단위 개념이 없어
  // 노출하지 않는다(명세).
  const isDay = scope === "day";
  // `messagesOpen` 자체는 건드리지 않고 표시만 억제한다 — 스트림 상세 드로어(App.tsx,
  // selectedStreamId !== null)가 열려 있으면 같은 우측 영역이 겹치므로(z-20 vs z-10) 여기서
  // 감추고, 상세를 닫으면 messagesOpen 값 그대로 패널이 자동 복귀한다(리뷰 지적, 레이아웃 충돌).
  const showMessages = isDay && messagesOpen && selectedStreamId === null;

  const closeMessages = useCallback(() => {
    setMessagesOpen(false);
    window.localStorage.setItem(MESSAGES_OPEN_STORAGE_KEY, "false");
  }, []);

  function toggleMessagesOpen() {
    setMessagesOpen((prev) => {
      const next = !prev;
      window.localStorage.setItem(MESSAGES_OPEN_STORAGE_KEY, String(next));
      return next;
    });
  }

  // 오버레이 모드(<lg)에서만 Esc로 패널을 닫는다 — ≥lg(나란히 배치)에서는 Esc가 다른 단축키와
  // 충돌할 수 있어 닫기 동작을 붙이지 않는다(명세).
  useEffect(() => {
    if (!showMessages) return;
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key !== "Escape") return;
      if (window.innerWidth >= LG_BREAKPOINT_PX) return;
      closeMessages();
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [showMessages, closeMessages]);

  const { data: config } = useQuery({
    queryKey: queryKeys.summaryConfig(),
    queryFn: getSummaryConfig,
  });

  // 세 쿼리/뮤테이션 세트를 항상 호출하고(hooks 규칙) `enabled`로 스코프에 맞는 쪽만 활성화한다 —
  // BE 커맨드가 daily/period/resume으로 분리돼 있어(lib.rs) 조건부 호출 없이 이 방식으로만 재사용이
  // 가능하다.
  const { data: dailySummary, isLoading: dailyLoading } = useQuery({
    queryKey: queryKeys.dailySummary(currentDate, locale),
    queryFn: () => getDailySummary(currentDate, locale),
    enabled: scope === "day",
  });
  const { data: periodSummary, isLoading: periodLoading } = useQuery({
    queryKey: queryKeys.periodSummary(periodType, periodKey, locale),
    queryFn: () => getPeriodSummary(periodType, periodKey, locale),
    enabled: isPeriod,
  });
  const { data: periodStats, isError: periodStatsError } = useQuery({
    queryKey: queryKeys.periodStats(periodType, periodKey, LOCAL_TIMEZONE),
    queryFn: () => getPeriodStats(periodType, periodKey, LOCAL_TIMEZONE),
    enabled: isPeriod,
  });

  const summary = isPeriod ? periodSummary : dailySummary;
  const isLoading = isPeriod ? periodLoading : dailyLoading;

  const generateDailyMutation = useMutation({
    mutationFn: ({
      force,
      excerptOverride = null,
    }: {
      force: boolean;
      excerptOverride?: string | null;
    }) => generateDailySummary(currentDate, LOCAL_TIMEZONE, locale, force, excerptOverride),
    onSuccess: (data) => {
      // 결과를 바로 캐시에 넣는다 — 무효화 후 재조회 전까지 옛(stale) 값이 남아 자동 재생성이 한 번 더
      // 걸리는 틈을 막는다.
      queryClient.setQueryData(queryKeys.dailySummary(data.localDate, data.locale), data);
      queryClient.invalidateQueries({ queryKey: queryKeys.dailySummary(currentDate, locale) });
    },
  });
  const generatePeriodMutation = useMutation({
    mutationFn: ({
      force,
      excerptOverride = null,
    }: {
      force: boolean;
      excerptOverride?: string | null;
    }) =>
      generatePeriodSummary(periodType, periodKey, LOCAL_TIMEZONE, locale, force, excerptOverride),
    onSuccess: (data) => {
      queryClient.setQueryData(
        queryKeys.periodSummary(data.periodType, data.periodKey, data.locale),
        data,
      );
      queryClient.invalidateQueries({
        queryKey: queryKeys.periodSummary(periodType, periodKey, locale),
      });
    },
  });
  const generateMutation = isPeriod ? generatePeriodMutation : generateDailyMutation;

  const engineEnabled = config?.enabled === true;
  // 일간·기간 생성 중 하나라도 진행 중이면 자동 재생성을 건너뛴다(동시 LLM 호출 방지).
  const anyGeneratePending = generateDailyMutation.isPending || generatePeriodMutation.isPending;

  // 일간만: 열린 캐시가 stale 이면 기존 "다시 생성"(force) 경로를 한 번 탄다. 실패하면 쿼리를
  // 무효화하지 않으므로 기존 캐시가 그대로 보인다. 주간·월간 force 는 낡은 하위 요약까지 다시 만드는
  // Refresh 캐스케이드(주간 ≤7회·월간 ≤6회)라 자동으로 돌리지 않고 안내만 띄운다(아래 staleNotice).
  const summaryTargetKey = `summary:day:${currentDate}:${locale}`;
  const summaryNeedsRegen =
    isDay &&
    !isLoading &&
    shouldAutoRegenerate(dailySummary, engineEnabled, autoRegenAttempted, summaryTargetKey);
  const { mutate: generateDailyMutate } = generateDailyMutation;
  useEffect(() => {
    // has 재확인: StrictMode 이중 실행·같은 렌더의 중복 effect 에서 두 번 호출하지 않게 한다.
    if (!summaryNeedsRegen || anyGeneratePending || autoRegenAttempted.has(summaryTargetKey))
      return;
    autoRegenAttempted.add(summaryTargetKey);
    generateDailyMutate({ force: true });
  }, [summaryNeedsRegen, anyGeneratePending, generateDailyMutate, summaryTargetKey]);

  // "상세 보기"(온디맨드) — 기본 요약(daily/period)과 달리 캐시를 useQuery로 미리 조회하지 않고
  // 버튼을 눌렀을 때만 generate_summary_detail을 호출한다(재캡처 없이 같은 발췌를 상세 프롬프트로
  // 재호출, force=false라 이미 캐시가 있으면 재호출 없이 그 값을 그대로 받는다).
  const detailMutation = useMutation({
    mutationFn: (target: { scope: SummaryDetailScope; scopeKey: string }) =>
      generateSummaryDetail(target.scope, target.scopeKey, LOCAL_TIMEZONE, locale, false),
  });
  // 받은 상세 캐시가 stale 이면 force 로 한 번 다시 만든다 — 별도 뮤테이션이라 재생성 중·실패 시에도
  // detailMutation.data(기존 캐시)가 그대로 남아 보인다.
  const detailRefreshMutation = useMutation({
    mutationFn: (target: { scope: SummaryDetailScope; scopeKey: string }) =>
      generateSummaryDetail(target.scope, target.scopeKey, LOCAL_TIMEZONE, locale, true),
  });
  const detailData = detailRefreshMutation.data ?? detailMutation.data;
  const detailPending = detailMutation.isPending || detailRefreshMutation.isPending;
  const detailError = detailMutation.error ?? detailRefreshMutation.error;

  const detailTarget = detailMutation.variables;
  const detailTargetKey = detailTarget
    ? `detail:${detailTarget.scope}:${detailTarget.scopeKey}:${locale}`
    : "";
  const detailNeedsRegen =
    detailTarget !== undefined &&
    shouldAutoRegenerate(detailMutation.data, engineEnabled, autoRegenAttempted, detailTargetKey);
  const { mutate: detailRefreshMutate } = detailRefreshMutation;
  useEffect(() => {
    if (!detailNeedsRegen || !detailTarget || anyGeneratePending) return;
    if (autoRegenAttempted.has(detailTargetKey)) return;
    autoRegenAttempted.add(detailTargetKey);
    detailRefreshMutate(detailTarget);
  }, [detailNeedsRegen, detailTarget, detailTargetKey, anyGeneratePending, detailRefreshMutate]);

  // 대상(스코프·기간/날짜)이 바뀌면 펼침 상태와 이전 상세 결과를 초기화한다 — 다른 날짜/기간의
  // 상세가 남아 있으면 안 된다(명세). `detailMutation.reset`은 TanStack Query가 매 렌더 안정적으로
  // 재사용하는 함수 참조라 무한 재실행 걱정 없이 의존성 배열에 넣을 수 있다.
  // biome-ignore lint/correctness/useExhaustiveDependencies: scope/periodKey는 본문에서 읽진 않지만 "대상이 바뀌었는지"를 감지하는 의도적 리셋 트리거다.
  useEffect(() => {
    setDetailOpen(false);
    detailMutation.reset();
    detailRefreshMutation.reset();
  }, [scope, periodKey, detailMutation.reset, detailRefreshMutation.reset]);

  // 재개 스코프 전용 — 캐시가 없어 daily/period처럼 useQuery+get*Summary 조합이 아니라, "프로젝트
  // 선택 시(또는 재생성 버튼) 1회 생성"을 useMutation으로 표현한다(구 ResumeBriefingDialog.tsx와
  // 동일한 모델).
  const resumeMutation = useMutation({
    mutationFn: (project: string) => generateResumeBriefing(project, LOCAL_TIMEZONE, locale),
  });

  /** 스코프 전환 시 재개 상태를 항상 초기화한다 — 재개는 캐시가 없어(항상 최신) 이전 프로젝트의
   * 브리핑 결과가 남아 있으면 다른 스코프를 왕복한 뒤 낡은 결과가 노출된다(리뷰 Warning). */
  function setScope(next: SummaryScope) {
    if (next !== scope) {
      setResumeProject(null);
      setResumeCopied(false);
      resumeMutation.reset();
    }
    setScopeState(next);
  }

  function handleResumeProjectChange(project: string | null) {
    setResumeProject(project);
    setResumeCopied(false);
    if (project !== null) {
      resumeMutation.mutate(project);
    }
  }

  function handleResumeCopy() {
    if (!resumeMutation.data) return;
    void writeText(resumeMutation.data.content).then(() => {
      setResumeCopied(true);
      setTimeout(() => setResumeCopied(false), COPIED_NOTICE_MS);
    });
  }

  /** "상세 보기" 버튼 토글 — 펼쳐진 상태에서 다시 누르면 접기만 하고(결과는 유지), 접힌 상태에서
   * 누르면 펼치면서 아직 결과가 없을 때만(또는 로딩 중이 아닐 때만) 생성 요청을 보낸다 — 접었다
   * 펴도 재호출하지 않는다는 명세를 만족한다. */
  function handleToggleDetail() {
    if (detailOpen) {
      setDetailOpen(false);
      return;
    }
    setDetailOpen(true);
    if (detailScope && !detailMutation.data && !detailMutation.isPending) {
      detailMutation.mutate({ scope: detailScope, scopeKey: periodKey });
    }
  }

  const periodLabel =
    scope === "week" ? formatWeekRangeLabel(periodKey) : formatMonthLabel(periodKey, locale);
  const previewTarget: PreviewTarget = isResume
    ? { kind: "resume", project: resumeProject ?? "" }
    : isPeriod
      ? { kind: "period", periodType, periodKey }
      : { kind: "day", localDate: currentDate };

  return (
    <div className="relative flex h-full">
      <div className="h-full min-w-0 flex-1 overflow-y-auto">
        <div className="mx-auto w-full max-w-2xl px-6 py-6">
          <header className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="flex items-center gap-2 text-base font-semibold">
                <Sparkles className="size-4 text-muted-foreground" aria-hidden="true" />
                {t("summary.card.heading")}
              </h2>
              <ScopeToggle scope={scope} onChange={setScope} />
            </div>
            <div className="flex items-center gap-2">
              {!isResume && summary && (
                <span className="text-xs text-muted-foreground">
                  {t("summary.card.generatedAt", { time: formatGeneratedAt(summary.createdAt) })}
                </span>
              )}
              {isResume && resumeMutation.data && (
                <span className="text-xs text-muted-foreground">
                  {t("summary.card.generatedAt", {
                    time: formatGeneratedAt(resumeMutation.data.createdAt),
                  })}
                </span>
              )}
              {isDay && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  aria-pressed={messagesOpen}
                  aria-label={t("summary.messages.toggle")}
                  title={t("summary.messages.toggle")}
                  onClick={toggleMessagesOpen}
                >
                  <PanelRight className="size-4" aria-hidden="true" />
                </Button>
              )}
            </div>
          </header>

          {isReview ? (
            <ReviewPanel />
          ) : (
            <>
              {isResume ? (
                <ResumeProjectSelect project={resumeProject} onChange={handleResumeProjectChange} />
              ) : scope === "day" ? (
                <p className="mt-1 text-sm text-muted-foreground tabular-nums">{currentDate}</p>
              ) : (
                <div className="mt-1 flex items-center gap-1">
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("summary.view.prevPeriod")}
                    onClick={() =>
                      setCurrentDate(
                        scope === "week"
                          ? shiftLocalDate(currentDate, -7)
                          : shiftLocalMonth(currentDate, -1),
                      )
                    }
                  >
                    <ChevronLeft className="size-4" />
                  </Button>
                  <span className="text-sm font-medium tabular-nums">{periodLabel}</span>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("summary.view.nextPeriod")}
                    onClick={() =>
                      setCurrentDate(
                        scope === "week"
                          ? shiftLocalDate(currentDate, 7)
                          : shiftLocalMonth(currentDate, 1),
                      )
                    }
                  >
                    <ChevronRight className="size-4" />
                  </Button>
                </div>
              )}

              {/* 팩트 스트립은 주간/월간 전용 — 재개(resume)는 프로젝트당 활동 집계 개념이 없어 숨긴다(명세). */}
              {isPeriod && periodStats && <PeriodStatsStrip stats={periodStats} />}
              {/* 통계 조회 실패를 "활동 없음"과 구분해 노출한다(리뷰 Nit — 조용히 사라지면 오해). */}
              {isPeriod && periodStatsError && (
                <p className="mt-4 text-xs text-muted-foreground">{t("summary.view.statsError")}</p>
              )}

              {isResume ? (
                <>
                  {resumeProject === null && (
                    <p className="mt-4 text-sm text-muted-foreground">
                      {t("summary.view.resumeSelectPrompt")}
                    </p>
                  )}
                  {resumeMutation.isPending && (
                    <p className="mt-4 text-sm text-muted-foreground">
                      {t("resume.dialog.generating")}
                    </p>
                  )}
                  {resumeMutation.isError && (
                    <p
                      className={cn(
                        "mt-3 text-sm",
                        isNoDataError(resumeMutation.error)
                          ? "text-muted-foreground"
                          : "text-destructive",
                      )}
                    >
                      {t(resumeErrorKey(resumeMutation.error))}
                    </p>
                  )}
                  {resumeMutation.isSuccess && (
                    <>
                      <SummaryContent content={resumeMutation.data.content} />
                      <div className="mt-4 flex items-center gap-2">
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          disabled={resumeMutation.isPending || !resumeProject}
                          onClick={() => resumeProject && resumeMutation.mutate(resumeProject)}
                        >
                          <RefreshCw
                            className={cn("size-3.5", resumeMutation.isPending && "animate-spin")}
                            aria-hidden="true"
                          />
                          {resumeMutation.isPending
                            ? t("resume.dialog.regenerating")
                            : t("resume.dialog.regenerateButton")}
                        </Button>
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          onClick={() => setPreviewOpen(true)}
                        >
                          {t("resume.dialog.previewButton")}
                        </Button>
                        <Button
                          type="button"
                          variant="default"
                          size="sm"
                          onClick={handleResumeCopy}
                        >
                          {resumeCopied ? (
                            <Check className="size-3.5" aria-hidden="true" />
                          ) : (
                            <Copy className="size-3.5" aria-hidden="true" />
                          )}
                          {t("resume.dialog.copyButton")}
                        </Button>
                        {resumeCopied && (
                          <span className="text-xs text-muted-foreground">
                            {t("resume.dialog.copiedNotice")}
                          </span>
                        )}
                      </div>
                    </>
                  )}
                </>
              ) : (
                <>
                  {isLoading && (
                    <p className="mt-4 text-sm text-muted-foreground">{t("common.loading")}</p>
                  )}

                  {!isLoading && summary && (
                    <>
                      <SummaryContent content={summary.content} />
                      {/* 주간·월간은 stale 이어도 자동 재생성하지 않는다(하위 요약 캐스케이드 비용) — 안내만. */}
                      {isPeriod && summary.stale && (
                        <p className="mt-4 text-xs text-muted-foreground">
                          {t("summary.card.staleNotice")}
                        </p>
                      )}
                      <div
                        className={cn(
                          "flex items-center gap-2",
                          isPeriod && summary.stale ? "mt-2" : "mt-4",
                        )}
                      >
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={() => generateMutation.mutate({ force: true })}
                          disabled={generateMutation.isPending}
                        >
                          <RefreshCw
                            className={cn("size-3.5", generateMutation.isPending && "animate-spin")}
                            aria-hidden="true"
                          />
                          {generateMutation.isPending
                            ? t("summary.card.regenerating")
                            : t("summary.card.regenerateButton")}
                        </Button>
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          onClick={() => setPreviewOpen(true)}
                        >
                          {t("summary.card.previewButton")}
                        </Button>
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          aria-expanded={detailOpen}
                          aria-controls={SUMMARY_DETAIL_SECTION_ID}
                          aria-busy={detailPending}
                          disabled={detailPending}
                          onClick={handleToggleDetail}
                        >
                          {detailPending
                            ? t("summary.detail.loading")
                            : detailOpen
                              ? t("summary.detail.hide")
                              : t("summary.detail.button")}
                        </Button>
                      </div>
                      {detailError && (
                        <p
                          // 생성 실패는 사용자가 버튼을 누른 뒤 비동기로 나타나는 상태 메시지라
                          // 스크린리더에 즉시 알려야 한다(a11y 감사 지적, WCAG 4.1.3).
                          role="alert"
                          className={cn(
                            "mt-2 text-xs",
                            isNoDataError(detailError)
                              ? "text-muted-foreground"
                              : "text-destructive",
                          )}
                        >
                          {t(summaryErrorKey(detailError, "summary.detail.error"))}
                        </p>
                      )}
                      {/* `id`는 토글 버튼의 aria-controls 대상 — 펼침 전에도 요소가 존재해야 참조가
                      끊기지 않으므로 컨테이너는 항상 렌더하고 내용만 조건부로 채운다. */}
                      <div id={SUMMARY_DETAIL_SECTION_ID}>
                        {detailOpen && detailData && (
                          <div className="mt-4 border-t border-border pt-4">
                            <SummaryContent content={detailData.content} />
                          </div>
                        )}
                      </div>
                    </>
                  )}

                  {!isLoading && !summary && config?.enabled && (
                    <div className="mt-4 space-y-3">
                      <p className="text-sm text-muted-foreground">
                        {t(
                          isPeriod ? "summary.view.emptyNoticePeriod" : "summary.view.emptyNotice",
                        )}
                      </p>
                      <div className="flex items-center gap-2">
                        <Button
                          type="button"
                          variant="default"
                          size="sm"
                          onClick={() => generateMutation.mutate({ force: false })}
                          disabled={generateMutation.isPending}
                        >
                          {generateMutation.isPending
                            ? t("summary.card.generating")
                            : t("summary.card.generateButton")}
                        </Button>
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={() => setPreviewOpen(true)}
                        >
                          {t("summary.card.previewButton")}
                        </Button>
                      </div>
                    </div>
                  )}
                </>
              )}

              {!isLoading && !summary && config && !config.enabled && (
                <div className="mt-4 space-y-3">
                  <p className="text-sm text-muted-foreground">
                    {t("summary.card.disabledNotice")}
                  </p>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => setSettingsOpen(true)}
                  >
                    {t("summary.card.openSettingsButton")}
                  </Button>
                </div>
              )}

              {generateMutation.isError && (
                <p
                  className={cn(
                    "mt-3 text-xs",
                    isNoDataError(generateMutation.error)
                      ? "text-muted-foreground"
                      : "text-destructive",
                  )}
                >
                  {t(summaryErrorKey(generateMutation.error, "summary.card.generateError"))}
                </p>
              )}

              <PreviewDialog
                open={previewOpen}
                onOpenChange={setPreviewOpen}
                target={previewTarget}
                locale={locale}
                generatePending={generateMutation.isPending}
                onGenerate={(excerptOverride) => {
                  // 수정본으로 생성 = 항상 재생성(force) — 캐시가 있어도 사용자가 의도적으로 입력을
                  // 바꿨으므로. 다이얼로그는 바로 닫고 본문 쪽 로딩/에러 표시로 이어진다.
                  generateMutation.mutate({ force: true, excerptOverride });
                  setPreviewOpen(false);
                }}
              />
            </>
          )}
        </div>
      </div>
      {showMessages && (
        <aside
          aria-labelledby={DAY_MESSAGES_HEADING_ID}
          className={cn(
            "flex h-full flex-col border-l border-border",
            "absolute inset-y-0 right-0 z-10 w-[320px] max-w-full bg-background shadow-2xl",
            "lg:static lg:z-auto lg:w-[320px] lg:shrink-0 lg:shadow-none",
          )}
        >
          <DayMessagesPanel localDate={currentDate} onClose={closeMessages} />
        </aside>
      )}
    </div>
  );
}
