import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { Check, ChevronLeft, ChevronRight, Copy, RefreshCw } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { ReviewReport, reviewToPlainText } from "@/components/ReviewReport";
import {
  isNoDataError,
  PreviewDialog,
  type PreviewTarget,
  SummaryContent,
  summaryErrorKey,
} from "@/components/SummaryShared";
import { Button } from "@/components/ui/button";
import type { ResolvedLocale, Translate, UiKey } from "@/i18n";
import { useResolvedLocale, useT } from "@/i18n";
import {
  generatePerformanceReview,
  getPerformanceReview,
  getReviewProfile,
  getSummaryConfig,
  listReviewProjects,
  queryKeys,
  setReviewProfile,
} from "@/lib/api";
import type {
  ReviewInputs,
  ReviewLevel,
  ReviewPeriodType,
  ReviewProfile,
  ReviewProject,
} from "@/lib/types";
import { cn } from "@/lib/utils";
import {
  LOCAL_TIMEZONE,
  localDateToMonthKey,
  localDateToQuarterKey,
  QUARTER_MONTHS,
  shiftLocalMonth,
  useAppStore,
} from "@/store";

/** "복사됨" 표시가 유지되는 시간(ms) — SummaryView.tsx의 재개 스코프 복사 버튼과 동일한 피드백. */
const COPIED_NOTICE_MS = 2000;

/** 직무 자유 입력 최대 길이(BE로 그대로 전송돼 발췌에 포함되므로 과도한 입력을 막는다). */
const ROLE_MAX_LENGTH = 60;

/** 기간 목표 자유 입력 최대 길이. */
const GOALS_MAX_LENGTH = 2000;

/** localStorage 키 — "포함할 프로젝트" 체크리스트에서 사용자가 제외한 프로젝트 표시명 목록(JSON
 * 배열). 기간별로 나누지 않는다 — 새 프로젝트는 항상 기본 포함이고, 한 번 제외한 프로젝트명은
 * 다른 기간에서도 같은 이름이면 계속 제외 상태로 남는다(테마/로케일과 동일한 persist 패턴). */
const EXCLUDED_PROJECTS_STORAGE_KEY = "logroom-review-excluded-projects";

function readExcludedProjects(): string[] {
  try {
    const raw = window.localStorage.getItem(EXCLUDED_PROJECTS_STORAGE_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((value): value is string => typeof value === "string");
  } catch {
    return [];
  }
}

function writeExcludedProjects(names: string[]): void {
  window.localStorage.setItem(EXCLUDED_PROJECTS_STORAGE_KEY, JSON.stringify(names));
}

function formatGeneratedAt(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** 보고서 부제 — 캐시된 평가서의 `inputs` 스냅샷 기준(지금 폼 상태가 아니라 "이 평가서가 실제로
 * 무엇으로 만들어졌는지"): 직무 · 연차 · 팀원 관리 · 포함 프로젝트 · 목표 유무. */
function formatReviewSubtitle(t: Translate, inputs: ReviewInputs): string {
  const { profile } = inputs;
  const projectsText =
    inputs.projects === null
      ? t("review.report.allProjects")
      : t("review.report.projectsCount", { count: inputs.projects.length });
  return [
    profile.role || null,
    profile.level ? t(LEVEL_LABEL_KEYS[profile.level]) : null,
    profile.managesPeople ? t("review.profile.managesPeopleShort") : null,
    projectsText,
    `${t("review.report.goals")}: ${inputs.goals ? t("review.goals.set") : t("review.goals.unset")}`,
  ]
    .filter(Boolean)
    .join(" · ");
}

function isReviewLevel(value: string): value is ReviewLevel {
  return (
    value === "" || value === "junior" || value === "mid" || value === "senior" || value === "lead"
  );
}

function sameProfile(a: ReviewProfile, b: ReviewProfile): boolean {
  return a.role === b.role && a.level === b.level && a.managesPeople === b.managesPeople;
}

const LEVEL_LABEL_KEYS: Record<ReviewLevel, UiKey> = {
  "": "review.profile.level.none",
  junior: "review.profile.level.junior",
  mid: "review.profile.level.mid",
  senior: "review.profile.level.senior",
  lead: "review.profile.level.lead",
};

/** 평가 프로필 입력 폼(필드 + 저장 버튼) — ReviewPanel(평가 화면의 접이식 프로필 블록)과
 * SettingsDialog.tsx(설정 → AI 요약 → 평가 프로필 섹션) 양쪽에서 재사용한다. 저장은
 * `queryKeys.reviewProfile()`을 invalidate해 두 위치가 항상 같은 값을 보게 한다(단일 소스).
 * 두 위치가 동시에 떠 있을 수 있어(평가 화면 위에 설정 다이얼로그) 입력 id는 `useId`로 만든다. */
export function ReviewProfileForm({
  profile,
  onSaved,
}: {
  profile: ReviewProfile;
  /** 저장 성공 후 호출 — ReviewPanel의 프로필 블록이 펼친 폼을 다시 접는 데 쓴다. */
  onSaved?: () => void;
}) {
  const t = useT();
  const idPrefix = useId();
  const queryClient = useQueryClient();
  const [localProfile, setLocalProfile] = useState<ReviewProfile>(profile);
  const [savedVisible, setSavedVisible] = useState(false);

  // profile prop이 바뀌면(다른 위치에서 저장했거나 재조회) 로컬 값을 맞추되, **이 폼에서 고치던 중이면
  // 건드리지 않는다** — 평가 화면과 설정 다이얼로그에 폼이 동시에 떠 있을 수 있어, 한쪽 저장이 다른
  // 쪽의 저장 안 한 입력을 말없이 덮으면 안 된다.
  const syncedProfileRef = useRef(profile);
  useEffect(() => {
    const synced = syncedProfileRef.current;
    setLocalProfile((current) => (sameProfile(current, synced) ? profile : current));
    syncedProfileRef.current = profile;
  }, [profile]);

  const saveMutation = useMutation({
    mutationFn: (next: ReviewProfile) => setReviewProfile(next),
    onSuccess: () => {
      setSavedVisible(true);
      queryClient.invalidateQueries({ queryKey: queryKeys.reviewProfile() });
      onSaved?.();
    },
  });

  function handleChange(next: ReviewProfile) {
    setLocalProfile(next);
    setSavedVisible(false);
  }

  return (
    <div className="space-y-3">
      <div>
        <label htmlFor={`${idPrefix}-role`} className="text-xs font-medium text-muted-foreground">
          {t("review.profile.role")}
        </label>
        <input
          id={`${idPrefix}-role`}
          type="text"
          value={localProfile.role}
          maxLength={ROLE_MAX_LENGTH}
          placeholder={t("review.profile.rolePlaceholder")}
          autoComplete="off"
          onChange={(event) => handleChange({ ...localProfile, role: event.target.value })}
          className="mt-1 w-full rounded-md border border-border bg-transparent px-2 py-1.5 text-sm outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring"
        />
      </div>
      <div>
        <label htmlFor={`${idPrefix}-level`} className="text-xs font-medium text-muted-foreground">
          {t("review.profile.level")}
        </label>
        <select
          id={`${idPrefix}-level`}
          value={localProfile.level}
          onChange={(event) => {
            const { value } = event.target;
            if (isReviewLevel(value)) handleChange({ ...localProfile, level: value });
          }}
          className="mt-1 w-full rounded-md border border-border bg-transparent px-2 py-1.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <option value="">{t("review.profile.level.none")}</option>
          <option value="junior">{t("review.profile.level.junior")}</option>
          <option value="mid">{t("review.profile.level.mid")}</option>
          <option value="senior">{t("review.profile.level.senior")}</option>
          <option value="lead">{t("review.profile.level.lead")}</option>
        </select>
      </div>
      <label htmlFor={`${idPrefix}-manages`} className="flex items-center gap-2 text-sm">
        <input
          id={`${idPrefix}-manages`}
          type="checkbox"
          checked={localProfile.managesPeople}
          onChange={(event) =>
            handleChange({ ...localProfile, managesPeople: event.target.checked })
          }
          className="size-4 rounded border-border accent-primary"
        />
        {t("review.profile.managesPeople")}
      </label>
      <div className="flex items-center gap-2">
        <Button
          type="button"
          variant="default"
          size="sm"
          onClick={() => saveMutation.mutate(localProfile)}
          disabled={saveMutation.isPending}
        >
          {saveMutation.isPending ? t("common.saving") : t("common.save")}
        </Button>
      </div>
      <p className="min-h-4 text-xs" aria-live="polite">
        {saveMutation.isError && <span className="text-destructive">{t("common.saveError")}</span>}
        {savedVisible && !saveMutation.isError && (
          <span className="text-muted-foreground">{t("review.profile.saved")}</span>
        )}
      </p>
    </div>
  );
}

/** ReviewPanel 전용 프로필 블록 — 비어 있으면 폼을 바로 펼쳐 보이고 안내 문구를 곁들이고, 채워져
 * 있으면 한 줄 요약(직무 · 연차 · 팀원 관리)과 "수정" 버튼으로 폼을 토글한다(SettingsDialog.tsx의
 * `ReviewProfileSection`은 이 토글 없이 폼을 항상 보여주는 별도 래퍼 — 이 컴포넌트를 재사용하지
 * 않는다). */
function ReviewProfileBlock() {
  const t = useT();
  const { data, isLoading } = useQuery({
    queryKey: queryKeys.reviewProfile(),
    queryFn: getReviewProfile,
  });
  const [expanded, setExpanded] = useState(false);

  return (
    <div className="space-y-2 rounded-md border border-border p-3">
      <div>
        <h3 className="text-sm font-semibold">{t("review.profile.heading")}</h3>
        <p className="mt-1 text-xs text-muted-foreground">{t("review.profile.description")}</p>
      </div>
      {isLoading && <p className="text-sm text-muted-foreground">{t("common.loading")}</p>}
      {data &&
        (data.role === "" && data.level === "" && !data.managesPeople ? (
          <>
            <p className="text-xs text-muted-foreground">{t("review.profile.firstTimeHint")}</p>
            <ReviewProfileForm profile={data} />
          </>
        ) : expanded ? (
          <ReviewProfileForm profile={data} onSaved={() => setExpanded(false)} />
        ) : (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <span className="text-muted-foreground">
              {[
                data.role || null,
                t(LEVEL_LABEL_KEYS[data.level]),
                data.managesPeople ? t("review.profile.managesPeopleShort") : null,
              ]
                .filter(Boolean)
                .join(" · ")}
            </span>
            <Button type="button" variant="ghost" size="sm" onClick={() => setExpanded(true)}>
              {t("review.profile.edit")}
            </Button>
          </div>
        ))}
    </div>
  );
}

interface ReviewProjectsSectionProps {
  projects: ReviewProject[];
  isLoading: boolean;
  excludedProjects: string[];
  onToggle: (name: string) => void;
  onSelectAll: () => void;
  onClearAll: () => void;
}

/** "포함할 프로젝트" 접이식 체크리스트 — 순수 표시 컴포넌트(데이터/제외 목록은 ReviewPanel이
 * 소유). `<summary>`에 포함/전체 건수를 함께 보여줘 접힌 상태에서도 한눈에 파악할 수 있게 한다. */
function ReviewProjectsSection({
  projects,
  isLoading,
  excludedProjects,
  onToggle,
  onSelectAll,
  onClearAll,
}: ReviewProjectsSectionProps) {
  const t = useT();
  const total = projects.length;
  const included = projects.filter((project) => !excludedProjects.includes(project.name)).length;

  return (
    <details className="rounded-md border border-border p-3">
      <summary className="cursor-pointer text-sm font-semibold">
        {t("review.projects.heading")}
        {!isLoading && total > 0 && ` — ${t("review.projects.summary", { included, total })}`}
      </summary>
      <div className="mt-3 space-y-2">
        {isLoading && <p className="text-xs text-muted-foreground">{t("common.loading")}</p>}
        {!isLoading && total === 0 && (
          <p className="text-xs text-muted-foreground">{t("review.projects.empty")}</p>
        )}
        {!isLoading && total > 0 && (
          <>
            <div className="flex items-center gap-2">
              <Button type="button" variant="outline" size="xs" onClick={onSelectAll}>
                {t("review.projects.selectAll")}
              </Button>
              <Button type="button" variant="outline" size="xs" onClick={onClearAll}>
                {t("review.projects.clearAll")}
              </Button>
            </div>
            <ul className="space-y-1">
              {projects.map((project) => {
                const checked = !excludedProjects.includes(project.name);
                return (
                  <li
                    key={project.name}
                    className="flex items-center justify-between gap-2 text-sm"
                  >
                    {/* 입력을 label 안에 두어 id 없이 연결한다 — 프로젝트 이름에 공백 등 id에 못
                        쓰는 문자가 들어올 수 있다. */}
                    <label className="flex min-w-0 items-center gap-2">
                      <input
                        type="checkbox"
                        checked={checked}
                        onChange={() => onToggle(project.name)}
                        className="size-4 shrink-0 rounded border-border accent-primary"
                      />
                      <span className="truncate">{project.name}</span>
                    </label>
                    <span className="shrink-0 text-xs text-muted-foreground">
                      {t("review.projects.activity", { count: project.activity })}
                    </span>
                  </li>
                );
              })}
            </ul>
          </>
        )}
      </div>
    </details>
  );
}

const REVIEW_TYPE_OPTIONS: Array<{ value: ReviewPeriodType; labelKey: UiKey }> = [
  { value: "quarter", labelKey: "review.kind.quarter" },
  { value: "month", labelKey: "review.kind.month" },
];

/** 분기 평가/월간 점검 세그먼트 토글 — SummaryView.tsx `ScopeToggle`과 동일한 스타일. */
function ReviewTypeToggle({
  reviewType,
  onChange,
}: {
  reviewType: ReviewPeriodType;
  onChange: (value: ReviewPeriodType) => void;
}) {
  const t = useT();
  return (
    <fieldset className="flex items-center gap-0.5 rounded-md border border-border p-0.5">
      <legend className="sr-only">{t("review.kindGroupAriaLabel")}</legend>
      {REVIEW_TYPE_OPTIONS.map((option) => (
        <button
          key={option.value}
          type="button"
          aria-pressed={reviewType === option.value}
          onClick={() => onChange(option.value)}
          className={cn(
            "rounded px-2 py-1 text-xs font-medium transition-colors",
            reviewType === option.value
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

/** 분기 기간 라벨 — ko "2026년 3분기 (7~9월)", en "Q3 2026 (Jul–Sep)"(SummaryView.tsx
 * `formatMonthLabel`과 동일한 "locale 분기 + 하드코딩 템플릿" 패턴). */
function formatQuarterLabel(periodKey: string, locale: ResolvedLocale): string {
  const [yearStr, quarterStr] = periodKey.split("-Q");
  const year = Number(yearStr);
  const quarter = Number(quarterStr);
  const startMonth = (quarter - 1) * QUARTER_MONTHS + 1;
  const endMonth = startMonth + QUARTER_MONTHS - 1;
  if (locale === "ko") return `${year}년 ${quarter}분기 (${startMonth}~${endMonth}월)`;
  const startName = new Date(year, startMonth - 1, 1).toLocaleDateString("en-US", {
    month: "short",
  });
  const endName = new Date(year, endMonth - 1, 1).toLocaleDateString("en-US", { month: "short" });
  return `Q${quarter} ${year} (${startName}–${endName})`;
}

/** 월간 점검 기간 라벨 — ko "2026년 9월", en "September 2026". */
function formatReviewMonthLabel(periodKey: string, locale: ResolvedLocale): string {
  const [yearStr, monthStr] = periodKey.split("-");
  const year = Number(yearStr);
  const monthIndex = Number(monthStr) - 1;
  if (locale === "ko") return `${year}년 ${monthIndex + 1}월`;
  const monthName = new Date(year, monthIndex, 1).toLocaleDateString("en-US", { month: "long" });
  return `${monthName} ${year}`;
}

/**
 * 업무평가서 화면(ADR-0017) — SummaryView.tsx의 "평가" 스코프 본문 전체를 대체한다(날짜/기간
 * 네비·통계 스트립·일간/기간 본문·비활성 안내·PreviewDialog를 이 컴포넌트가 자체적으로 그린다).
 *
 * 분기 평가/월간 점검(reviewType)은 로컬 state이며 기본값은 "quarter". periodKey는 전역
 * currentDate에서 파생한다(quarter="YYYY-Qn", month="YYYY-MM") — ◀ ▶는 currentDate를
 * ±3개월/±1개월 이동한다(store의 `shiftLocalMonth` 재사용).
 *
 * "포함할 프로젝트" 제외 목록은 localStorage에 전역으로 persist하고(기간 무관, 새 프로젝트는 기본
 * 포함), "목표"는 캐시된 평가서의 `inputs.goals`가 있으면 기간이 바뀔 때 1회만 prefill한다(사용자가
 * 그 뒤 지운 것까지 되돌리지 않기 위해 "1회"로 제한).
 */
export function ReviewPanel() {
  const t = useT();
  const locale = useResolvedLocale();
  const currentDate = useAppStore((state) => state.currentDate);
  const setCurrentDate = useAppStore((state) => state.setCurrentDate);
  const setSettingsOpen = useAppStore((state) => state.setSettingsOpen);
  const queryClient = useQueryClient();

  const [reviewType, setReviewType] = useState<ReviewPeriodType>("quarter");
  const [previewOpen, setPreviewOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const [excludedProjects, setExcludedProjects] = useState<string[]>(readExcludedProjects);
  const [goals, setGoals] = useState("");
  // "기간이 바뀔 때 1회만 prefill" 판정 기준 — reviewType까지 포함해야 같은 달력월이라도 분기/월간
  // 점검을 오가며 다른 캐시를 가리킬 때 잘못 판정하지 않는다.
  const prefilledForRef = useRef<string | null>(null);

  const periodKey =
    reviewType === "quarter"
      ? localDateToQuarterKey(currentDate)
      : localDateToMonthKey(currentDate);
  const periodIdentity = `${reviewType}:${periodKey}`;
  const periodLabel =
    reviewType === "quarter"
      ? formatQuarterLabel(periodKey, locale)
      : formatReviewMonthLabel(periodKey, locale);

  const { data: config } = useQuery({
    queryKey: queryKeys.summaryConfig(),
    queryFn: getSummaryConfig,
  });

  const { data: reviewData, isLoading: reviewLoading } = useQuery({
    queryKey: queryKeys.performanceReview(reviewType, periodKey, locale),
    queryFn: () => getPerformanceReview(reviewType, periodKey, locale),
  });

  const { data: reviewProjectsData, isLoading: reviewProjectsLoading } = useQuery({
    queryKey: queryKeys.reviewProjects(reviewType, periodKey, LOCAL_TIMEZONE),
    queryFn: () => listReviewProjects(reviewType, periodKey, LOCAL_TIMEZONE),
  });
  const projectsList = reviewProjectsData ?? [];

  // 기간(reviewType/periodKey)이 바뀌면 목표 입력을 비우고 prefill 판정을 리셋한다 — 다른 기간의
  // 입력이 남아있으면 안 된다.
  // biome-ignore lint/correctness/useExhaustiveDependencies: periodIdentity만으로 "대상이 바뀌었는지"를 감지하는 의도적 리셋 트리거다.
  useEffect(() => {
    setGoals("");
    prefilledForRef.current = null;
  }, [periodIdentity]);

  // 캐시된 평가서가 로드되고 그 inputs.goals가 있으면, 이번 기간에서 아직 prefill한 적이 없을
  // 때만 1회 채운다(그 뒤 사용자가 지워도 다시 채우지 않는다).
  useEffect(() => {
    if (reviewData?.inputs.goals && prefilledForRef.current !== periodIdentity) {
      setGoals(reviewData.inputs.goals);
      prefilledForRef.current = periodIdentity;
    }
  }, [reviewData, periodIdentity]);

  function toggleProjectExcluded(name: string) {
    setExcludedProjects((prev) => {
      const next = prev.includes(name) ? prev.filter((n) => n !== name) : [...prev, name];
      writeExcludedProjects(next);
      return next;
    });
  }

  function selectAllProjects() {
    setExcludedProjects((prev) => {
      const visibleNames = new Set(projectsList.map((project) => project.name));
      const next = prev.filter((name) => !visibleNames.has(name));
      writeExcludedProjects(next);
      return next;
    });
  }

  function clearAllProjects() {
    setExcludedProjects((prev) => {
      const next = Array.from(new Set([...prev, ...projectsList.map((project) => project.name)]));
      writeExcludedProjects(next);
      return next;
    });
  }

  const includedProjectNames = projectsList
    .filter((project) => !excludedProjects.includes(project.name))
    .map((project) => project.name);
  const allIncluded =
    projectsList.length > 0 && includedProjectNames.length === projectsList.length;
  const projectsParam = allIncluded ? null : includedProjectNames;
  const trimmedGoals = goals.trim();
  const goalsParam = trimmedGoals === "" ? null : trimmedGoals;

  const generateMutation = useMutation({
    mutationFn: (excerptOverride: string | null) =>
      generatePerformanceReview(
        reviewType,
        periodKey,
        LOCAL_TIMEZONE,
        locale,
        projectsParam,
        goalsParam,
        excerptOverride,
      ),
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: queryKeys.performanceReview(reviewType, periodKey, locale),
      });
    },
  });

  function handleCopy() {
    if (!reviewData) return;
    void writeText(reviewToPlainText(reviewData.content)).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), COPIED_NOTICE_MS);
    });
  }

  const previewTarget: PreviewTarget = {
    kind: "review",
    periodType: reviewType,
    periodKey,
    projects: projectsParam,
    goals: goalsParam,
  };

  if (!config) {
    return <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
  }

  if (!config.enabled) {
    return (
      <div className="space-y-3">
        <p className="text-sm text-muted-foreground">{t("summary.card.disabledNotice")}</p>
        <Button type="button" variant="outline" size="sm" onClick={() => setSettingsOpen(true)}>
          {t("summary.card.openSettingsButton")}
        </Button>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <ReviewTypeToggle reviewType={reviewType} onChange={setReviewType} />
        <div className="flex items-center gap-1">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={t("review.prevPeriod")}
            onClick={() =>
              setCurrentDate(
                shiftLocalMonth(currentDate, reviewType === "quarter" ? -QUARTER_MONTHS : -1),
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
            aria-label={t("review.nextPeriod")}
            onClick={() =>
              setCurrentDate(
                shiftLocalMonth(currentDate, reviewType === "quarter" ? QUARTER_MONTHS : 1),
              )
            }
          >
            <ChevronRight className="size-4" />
          </Button>
        </div>
      </div>

      <ReviewProfileBlock />

      <ReviewProjectsSection
        projects={projectsList}
        isLoading={reviewProjectsLoading}
        excludedProjects={excludedProjects}
        onToggle={toggleProjectExcluded}
        onSelectAll={selectAllProjects}
        onClearAll={clearAllProjects}
      />

      <div className="space-y-1.5">
        <label htmlFor="review-goals" className="text-sm font-semibold">
          {t("review.goals.heading")}
        </label>
        <textarea
          id="review-goals"
          rows={4}
          maxLength={GOALS_MAX_LENGTH}
          value={goals}
          onChange={(event) => setGoals(event.target.value)}
          placeholder={t(
            reviewType === "quarter"
              ? "review.goals.placeholderQuarter"
              : "review.goals.placeholderMonth",
          )}
          className="w-full resize-none rounded-md border border-border bg-transparent p-2 text-sm outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring"
        />
      </div>

      {reviewLoading && <p className="text-sm text-muted-foreground">{t("common.loading")}</p>}

      {!reviewLoading && reviewData && (
        <div className="space-y-2">
          <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            <span>
              {t("summary.card.generatedAt", { time: formatGeneratedAt(reviewData.createdAt) })}
            </span>
          </div>
          <ReviewReport
            review={reviewData}
            title={t(
              reviewData.periodType === "quarter"
                ? "review.report.titleQuarter"
                : "review.report.titleMonth",
              { period: periodLabel },
            )}
            subtitle={formatReviewSubtitle(t, reviewData.inputs)}
            fallback={<SummaryContent content={reviewData.content} />}
          />
          <div className="flex flex-wrap items-center gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={generateMutation.isPending || includedProjectNames.length === 0}
              onClick={() => generateMutation.mutate(null)}
            >
              <RefreshCw
                className={cn("size-3.5", generateMutation.isPending && "animate-spin")}
                aria-hidden="true"
              />
              {generateMutation.isPending ? t("review.generating") : t("review.regenerateButton")}
            </Button>
            <Button type="button" variant="ghost" size="sm" onClick={() => setPreviewOpen(true)}>
              {t("summary.card.previewButton")}
            </Button>
            <Button type="button" variant="default" size="sm" onClick={handleCopy}>
              {copied ? (
                <Check className="size-3.5" aria-hidden="true" />
              ) : (
                <Copy className="size-3.5" aria-hidden="true" />
              )}
              {t("review.copyPlain")}
            </Button>
            {copied && (
              <span className="text-xs text-muted-foreground">
                {t("resume.dialog.copiedNotice")}
              </span>
            )}
          </div>
          <p className="text-xs text-muted-foreground">{t("review.draftNotice")}</p>
        </div>
      )}

      {!reviewLoading && !reviewData && (
        <div className="flex items-center gap-2">
          <Button
            type="button"
            variant="default"
            size="sm"
            disabled={generateMutation.isPending || includedProjectNames.length === 0}
            onClick={() => generateMutation.mutate(null)}
          >
            {generateMutation.isPending ? t("review.generating") : t("review.generateButton")}
          </Button>
          <Button type="button" variant="outline" size="sm" onClick={() => setPreviewOpen(true)}>
            {t("summary.card.previewButton")}
          </Button>
        </div>
      )}

      {generateMutation.isError && (
        <p
          className={cn(
            "text-xs",
            isNoDataError(generateMutation.error) ? "text-muted-foreground" : "text-destructive",
          )}
        >
          {t(summaryErrorKey(generateMutation.error, "review.generateError"))}
        </p>
      )}

      <PreviewDialog
        open={previewOpen}
        onOpenChange={setPreviewOpen}
        target={previewTarget}
        locale={locale}
        generatePending={generateMutation.isPending}
        onGenerate={(excerptOverride) => {
          generateMutation.mutate(excerptOverride);
          setPreviewOpen(false);
        }}
      />
    </div>
  );
}
