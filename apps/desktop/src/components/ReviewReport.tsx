import type { ReactNode } from "react";
import { useState } from "react";
import type { Translate } from "@/i18n";
import { useResolvedLocale, useT } from "@/i18n";
import type {
  PerformanceReview,
  ReviewPeriodType,
  ReviewSeriesPoint,
  ReviewStats,
} from "@/lib/types";
import { cn } from "@/lib/utils";

/** 프로젝트 비중 차트에 이름으로 보여 줄 최대 개수 — 나머지는 "기타" 한 줄로 접는다(dataviz 기준:
 * 색을 늘리지 않고 꼬리를 접는다). */
const TOP_PROJECTS = 6;

/** 평가서 축 태그(프롬프트 `[축]`, summary/prompts.rs `PROMPT_REVIEW_*`) — ko/en 두 벌. */
const AXIS_TAGS = [
  "성과",
  "완결",
  "전문성",
  "협업",
  "성장",
  "Impact",
  "Execution",
  "Expertise",
  "Collaboration",
  "Growth",
];

/** 평가서 한 항목 — 프롬프트가 쓰는 "- **요지** — 설명 [축] (근거)" 한 줄을 쪼갠 것. 형식을 벗어난
 * 줄(예: "- 기록상 뚜렷한 개선점 없음")은 `headline` 없이 `body`만 채운다. */
export interface ReviewItem {
  headline: string | null;
  body: string;
  axis: string | null;
  evidence: string[];
  /** 프롬프트가 목표에 없던 성과에 붙이는 "(목표 외)"/"(beyond goals)" 표시. */
  beyondGoals: boolean;
}

/** 목표에 없던 성과 표시(프롬프트 `PROMPT_REVIEW_*` "기대 수준" 문단) — 본문에서 떼어 칩으로 보여 준다. */
const BEYOND_GOALS_MARK = /\s*\((목표 외|beyond goals)\)/i;

/** 세 칸 — 순서로 식별한다(제목은 로케일·종류마다 다르다: "못한 것"/"밀린 것·막힌 것"/"What fell short"). */
export type ReviewSectionKind = "good" | "bad" | "next";

export interface ParsedReview {
  /** 첫 `## ` 앞의 `> ` 총평(근거 범위 한 줄). */
  overview: string[];
  sections: Array<{ kind: ReviewSectionKind; heading: string; items: ReviewItem[] }>;
}

const SECTION_KINDS: ReviewSectionKind[] = ["good", "bad", "next"];

export function parseReviewItem(raw: string): ReviewItem {
  let rest = raw.trim();
  let headline: string | null = null;
  const headlineMatch = rest.match(/^\*\*(.+?)\*\*\s*[—–-]\s*(.*)$/);
  if (headlineMatch?.[1] !== undefined) {
    headline = headlineMatch[1].trim();
    rest = headlineMatch[2] ?? "";
  }
  let evidence: string[] = [];
  const evidenceMatch = rest.match(/\s*\(([^()]*)\)\s*$/);
  if (evidenceMatch?.[1] !== undefined) {
    evidence = evidenceMatch[1]
      .split(/\s*[·,]\s*/)
      .map((ref) => ref.trim())
      .filter(Boolean);
    rest = rest.slice(0, evidenceMatch.index);
  }
  let axis: string | null = null;
  const axisMatch = rest.match(/\s*\[([^\]]+)\]\s*$/);
  const axisTag = axisMatch?.[1]?.trim();
  if (axisMatch && axisTag && AXIS_TAGS.includes(axisTag)) {
    axis = axisTag;
    rest = rest.slice(0, axisMatch.index);
  }
  const beyondGoals = BEYOND_GOALS_MARK.test(rest);
  rest = rest.replace(BEYOND_GOALS_MARK, "");
  return { headline, body: rest.replace(/\*\*/g, "").trim(), axis, evidence, beyondGoals };
}

/** 평가서 본문(마크다운)을 총평 + 세 칸으로 파싱한다. `## `가 하나도 없으면 sections가 빈 배열 —
 * 호출부는 일반 요약 렌더로 떨어진다. */
export function parseReview(content: string): ParsedReview {
  const overview: string[] = [];
  const sections: ParsedReview["sections"] = [];
  let lastItems: string[] | null = null;
  for (const line of content.split("\n")) {
    if (line.startsWith("## ")) {
      const kind = SECTION_KINDS[sections.length];
      if (!kind) break;
      sections.push({ kind, heading: line.slice(3).trim(), items: [] });
      lastItems = null;
      continue;
    }
    const current = sections[sections.length - 1];
    if (line.startsWith("> ") && !current) {
      overview.push(line.slice(2).trim());
      continue;
    }
    if (line.startsWith("- ") && current) {
      lastItems = [line.slice(2)];
      current.items.push(parseReviewItem(line.slice(2)));
      continue;
    }
    const trimmed = line.trim();
    // 불릿이 여러 줄로 이어지면(모델이 줄을 바꾼 경우) 앞 불릿에 붙여 다시 파싱한다.
    if (trimmed && current && lastItems) {
      lastItems.push(trimmed);
      current.items[current.items.length - 1] = parseReviewItem(lastItems.join(" "));
    }
  }
  return { overview, sections };
}

/** 회사 양식에 붙일 평문 — 마크다운 기호(`## `, `> `, `**`)를 걷어낸다. 불릿 `- `는 남긴다. */
export function reviewToPlainText(content: string): string {
  return content
    .split("\n")
    .map((line) => {
      if (line.startsWith("## ")) return line.slice(3).trim();
      if (line.startsWith("> ")) return line.slice(2);
      return line;
    })
    .join("\n")
    .replace(/\*\*/g, "")
    .trim();
}

const numberFormat = new Intl.NumberFormat();

function formatCount(value: number): string {
  return numberFormat.format(value);
}

/** "2026-07-06" → "7/6" */
function formatShortDate(key: string): string {
  const [, month, day] = key.split("-").map(Number);
  return `${month}/${day}`;
}

function formatPointLabel(t: Translate, key: string, unit: "week" | "day"): string {
  return unit === "week"
    ? t("review.report.weekOf", { date: formatShortDate(key) })
    : formatShortDate(key);
}

// ── 숫자 타일 ─────────────────────────────────────────────────────

function StatTiles({ stats }: { stats: ReviewStats }) {
  const t = useT();
  const median =
    stats.linear.medianCycleTenths === null
      ? null
      : (stats.linear.medianCycleTenths / 10).toFixed(1);
  // 활동일 외에는 0인 타일을 뺀다 — 연결하지 않은 소스의 "0"이 "안 했다"로 읽히지 않게(발췌의 사실 신호와
  // 같은 원칙, summary/review.rs::render_signals).
  const tiles: Array<{ label: string; value: number; hint?: string }> = [
    { label: t("review.report.tile.activeDays"), value: stats.activeDays },
    { label: t("review.report.tile.requests"), value: stats.prompts },
    {
      label: t("review.report.tile.linearCompleted"),
      value: stats.linear.completed,
      hint: median ? t("review.report.tile.medianCycle", { days: median }) : undefined,
    },
    {
      label: t("review.report.tile.carryOver"),
      value: stats.linear.carryOver,
      hint:
        stats.linear.stalled > 0
          ? t("review.report.tile.stalledHint", { count: stats.linear.stalled })
          : undefined,
    },
    { label: t("review.report.tile.prsOpened"), value: stats.github.prsOpened },
    { label: t("review.report.tile.commentsReviews"), value: stats.github.commentsReviews },
  ].filter((tile, index) => index === 0 || tile.value > 0);

  return (
    <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
      {tiles.map((tile) => (
        <div key={tile.label} className="rounded-md border border-border px-3 py-2">
          <p className="text-xs text-muted-foreground">{tile.label}</p>
          <p className="mt-0.5 text-lg font-semibold">{formatCount(tile.value)}</p>
          {tile.hint && <p className="text-[11px] text-muted-foreground">{tile.hint}</p>}
        </div>
      ))}
    </div>
  );
}

// ── 활동 추이(작은 막대 차트 3개 — 단위가 달라 한 축에 겹치지 않는다) ─────────

type SeriesField = "requests" | "toolActivity" | "completed";

function MiniColumns({
  points,
  field,
  title,
  unit,
}: {
  points: ReviewSeriesPoint[];
  field: SeriesField;
  title: string;
  unit: "week" | "day";
}) {
  const t = useT();
  const values = points.map((point) => point[field]);
  const max = Math.max(0, ...values);
  const total = values.reduce((sum, value) => sum + value, 0);
  if (total === 0) return null;
  // 가로축 라벨은 처음·가운데·끝 세 개만 — 모든 칸에 라벨을 달면 읽히지 않는다.
  const labelIndexes = new Set([0, Math.floor((points.length - 1) / 2), points.length - 1]);

  return (
    <figure className="space-y-1.5">
      <figcaption className="flex items-baseline justify-between gap-2 text-xs">
        <span className="font-medium text-foreground">{title}</span>
        <span className="text-muted-foreground">
          {t("review.report.series.summary", {
            total: formatCount(total),
            max: formatCount(max),
          })}
        </span>
      </figcaption>
      <div className="flex h-16 items-end gap-[2px] border-b border-border" aria-hidden="true">
        {points.map((point) => {
          const value = point[field];
          const label = formatPointLabel(t, point.key, unit);
          return (
            <div
              key={point.key}
              // 막대가 아니라 칸 전체가 호버 대상이다 — 얇은 막대를 정확히 겨누지 않아도 된다. 스크린리더·
              // 키보드는 아래 "표로 보기"로 같은 값을 읽는다(툴팁은 보조일 뿐 유일한 경로가 아니다).
              className="group relative flex h-full flex-1 items-end justify-center"
            >
              {value > 0 && (
                <div
                  className="w-full max-w-6 rounded-t-[4px] bg-[var(--viz-series-1)] group-hover:opacity-80"
                  style={{ height: `${Math.max((value / max) * 100, 3)}%` }}
                />
              )}
              <div className="pointer-events-none absolute bottom-full z-10 mb-1 hidden whitespace-nowrap rounded border border-border bg-popover px-1.5 py-0.5 text-[11px] shadow-sm group-hover:block">
                <span className="font-semibold text-foreground">{formatCount(value)}</span>{" "}
                <span className="text-muted-foreground">{label}</span>
              </div>
            </div>
          );
        })}
      </div>
      <div className="flex text-[10px] text-muted-foreground">
        {points.map((point, index) => (
          <span key={point.key} className="flex-1 text-center tabular-nums">
            {labelIndexes.has(index) ? formatShortDate(point.key) : ""}
          </span>
        ))}
      </div>
    </figure>
  );
}

function ProjectShares({ projects }: { projects: ReviewStats["projects"] }) {
  const t = useT();
  const total = projects.reduce((sum, project) => sum + project.activity, 0);
  if (total === 0) return null;
  const head = projects.slice(0, TOP_PROJECTS);
  const tailTotal = projects.slice(TOP_PROJECTS).reduce((sum, p) => sum + p.activity, 0);
  const rows = [
    ...head.map((project) => ({ name: project.name, value: project.activity, other: false })),
    ...(tailTotal > 0 ? [{ name: t("review.report.other"), value: tailTotal, other: true }] : []),
  ];
  const max = Math.max(...rows.map((row) => row.value));

  return (
    <figure className="space-y-1.5">
      <figcaption className="text-xs font-medium text-foreground">
        {t("review.report.projectsHeading")}
      </figcaption>
      <ul className="space-y-1">
        {rows.map((row) => (
          <li key={row.name} className="grid grid-cols-[8rem_1fr_auto] items-center gap-2 text-xs">
            <span className="truncate text-muted-foreground" title={row.name}>
              {row.name}
            </span>
            <div className="h-2.5">
              <div
                className={cn(
                  "h-full rounded-r-[4px]",
                  row.other ? "bg-muted-foreground/40" : "bg-[var(--viz-series-1)]",
                )}
                style={{ width: `${Math.max((row.value / max) * 100, 1)}%` }}
              />
            </div>
            <span className="tabular-nums text-muted-foreground">
              {formatCount(row.value)} · {Math.round((row.value / total) * 100)}%
            </span>
          </li>
        ))}
      </ul>
    </figure>
  );
}

/** 차트의 표 보기 — 툴팁 없이도 모든 값을 읽을 수 있게 한다(dataviz 접근성 기준). */
function SeriesTable({ stats }: { stats: ReviewStats }) {
  const t = useT();
  const { unit, points } = stats.series;
  return (
    <details className="text-xs">
      <summary className="cursor-pointer text-muted-foreground">
        {t("review.report.tableToggle")}
      </summary>
      <table className="mt-2 w-full tabular-nums">
        <thead className="text-muted-foreground">
          <tr>
            <th className="py-1 text-left font-normal">{t("review.report.table.period")}</th>
            <th className="py-1 text-right font-normal">{t("review.report.series.requests")}</th>
            <th className="py-1 text-right font-normal">
              {t("review.report.series.toolActivity")}
            </th>
            <th className="py-1 text-right font-normal">{t("review.report.series.completed")}</th>
          </tr>
        </thead>
        <tbody>
          {points.map((point) => (
            <tr key={point.key} className="border-t border-border">
              <td className="py-1">{formatPointLabel(t, point.key, unit)}</td>
              <td className="py-1 text-right">{formatCount(point.requests)}</td>
              <td className="py-1 text-right">{formatCount(point.toolActivity)}</td>
              <td className="py-1 text-right">{formatCount(point.completed)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </details>
  );
}

/** localStorage 키 — "시간·요일 분포" 옵션(기본 꺼짐). 기록된 활동 시각을 요일×시로 보여 주는데, 감시
 * 도구처럼 읽힐 수 있어 사용자가 켤 때만 그린다(결정 2026-09-23). */
const SHOW_RHYTHM_STORAGE_KEY = "logroom-review-show-rhythm";

const WEEKDAY_LABELS = {
  ko: ["월", "화", "수", "목", "금", "토", "일"],
  en: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
};

/** 칸 농도 단계 — 한 색(강조색)의 불투명도로 적음→많음을 나타낸다(순차 척도: 한 색상, 적을수록 표면에
 * 가깝다). 0건 칸은 색을 칠하지 않는다. */
const RHYTHM_OPACITY = [0.2, 0.4, 0.6, 0.8, 1];

/** 업무시간(평일 9~18시) — 요약 줄의 "업무시간 밖 비중" 기준. */
const WORK_START_HOUR = 9;
const WORK_END_HOUR = 18;
const WEEKEND_ROWS = [5, 6];

const HOURS = Array.from({ length: 24 }, (_, hour) => hour);

function rhythmLevel(value: number, max: number): number {
  if (value <= 0 || max <= 0) return -1;
  return Math.min(RHYTHM_OPACITY.length - 1, Math.floor((value / max) * RHYTHM_OPACITY.length));
}

function RhythmHeatmap({ rhythm }: { rhythm: number[][] }) {
  const t = useT();
  const locale = useResolvedLocale();
  const labels = WEEKDAY_LABELS[locale];
  const cells = rhythm.flatMap((row, weekday) =>
    row.map((count, hour) => ({ weekday, hour, count })),
  );
  const total = cells.reduce((sum, cell) => sum + cell.count, 0);
  if (total === 0) return null;
  const max = Math.max(...cells.map((cell) => cell.count));
  const weekend = cells
    .filter((cell) => WEEKEND_ROWS.includes(cell.weekday))
    .reduce((sum, cell) => sum + cell.count, 0);
  const offHours = cells
    .filter(
      (cell) =>
        WEEKEND_ROWS.includes(cell.weekday) ||
        cell.hour < WORK_START_HOUR ||
        cell.hour >= WORK_END_HOUR,
    )
    .reduce((sum, cell) => sum + cell.count, 0);
  const peak = cells.reduce((best, cell) => (cell.count > best.count ? cell : best));
  const percent = (value: number) => Math.round((value / total) * 100);

  return (
    <figure className="space-y-2">
      <figcaption className="space-y-0.5 text-xs">
        <span className="font-medium text-foreground">{t("review.report.rhythm.heading")}</span>
        <p className="text-muted-foreground">
          {t("review.report.rhythm.summary", {
            offHours: percent(offHours),
            weekend: percent(weekend),
            peak: t("review.report.rhythm.peak", {
              weekday: labels[peak.weekday] ?? "",
              hour: peak.hour,
            }),
          })}
        </p>
        <p className="text-muted-foreground">{t("review.report.rhythm.note")}</p>
      </figcaption>
      <div className="grid grid-cols-[2rem_1fr] gap-x-1 gap-y-[2px]">
        {rhythm.map((row, weekday) => (
          <div key={labels[weekday]} className="contents">
            <span className="text-[10px] leading-4 text-muted-foreground">{labels[weekday]}</span>
            <div className="grid grid-cols-24 gap-[2px]">
              {row.map((count, hour) => {
                const level = rhythmLevel(count, max);
                return (
                  <div
                    // biome-ignore lint/suspicious/noArrayIndexKey: 24칸 고정 배열의 시(hour) 자체가 키다.
                    key={hour}
                    className="group relative h-4 rounded-[2px] bg-muted"
                  >
                    {level >= 0 && (
                      <div
                        className="h-full w-full rounded-[2px] bg-[var(--viz-series-1)]"
                        style={{ opacity: RHYTHM_OPACITY[level] }}
                      />
                    )}
                    <div className="pointer-events-none absolute bottom-full left-1/2 z-10 mb-1 hidden -translate-x-1/2 whitespace-nowrap rounded border border-border bg-popover px-1.5 py-0.5 text-[11px] shadow-sm group-hover:block">
                      {t("review.report.rhythm.cell", {
                        weekday: labels[weekday] ?? "",
                        hour,
                        count: formatCount(count),
                      })}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        ))}
        <span />
        <div className="grid grid-cols-24 text-[10px] text-muted-foreground">
          {HOURS.map((hour) => (
            <span key={hour} className="tabular-nums">
              {hour % 6 === 0 ? hour : ""}
            </span>
          ))}
        </div>
      </div>
      <div className="flex items-center justify-end gap-1 text-[10px] text-muted-foreground">
        <span>{t("review.report.rhythm.less")}</span>
        {RHYTHM_OPACITY.map((opacity) => (
          <span
            key={opacity}
            className="size-3 rounded-[2px] bg-[var(--viz-series-1)]"
            style={{ opacity }}
          />
        ))}
        <span>{t("review.report.rhythm.more")}</span>
      </div>
    </figure>
  );
}

function ActivityPanel({ stats }: { stats: ReviewStats }) {
  const t = useT();
  const { unit, points } = stats.series;
  const [showRhythm, setShowRhythm] = useState(
    () => window.localStorage.getItem(SHOW_RHYTHM_STORAGE_KEY) === "true",
  );

  function toggleRhythm(next: boolean) {
    setShowRhythm(next);
    window.localStorage.setItem(SHOW_RHYTHM_STORAGE_KEY, String(next));
  }

  return (
    <section className="space-y-4 rounded-md border border-border p-4">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h4 className="text-sm font-semibold">{t("review.report.activityHeading")}</h4>
          <p className="mt-0.5 text-xs text-muted-foreground">{t("review.report.activityNote")}</p>
        </div>
        {stats.rhythm && (
          <label className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
            <input
              type="checkbox"
              checked={showRhythm}
              onChange={(event) => toggleRhythm(event.target.checked)}
              className="size-3.5 rounded border-border accent-primary"
            />
            {t("review.report.rhythm.toggle")}
          </label>
        )}
      </div>
      {points.length > 0 && (
        <div className="space-y-4">
          <MiniColumns
            points={points}
            field="requests"
            unit={unit}
            title={t("review.report.series.requests")}
          />
          <MiniColumns
            points={points}
            field="toolActivity"
            unit={unit}
            title={t("review.report.series.toolActivity")}
          />
          <MiniColumns
            points={points}
            field="completed"
            unit={unit}
            title={t("review.report.series.completed")}
          />
        </div>
      )}
      <ProjectShares projects={stats.projects} />
      {showRhythm && stats.rhythm && <RhythmHeatmap rhythm={stats.rhythm} />}
      {points.length > 0 && <SeriesTable stats={stats} />}
    </section>
  );
}

// ── 세 칸 카드 ────────────────────────────────────────────────────

/** 칸별 왼쪽 선 색 — 잘한 것=good, 못한 것=warning(상태색: 의미가 있는 색이고 제목 라벨이 함께 간다),
 * 채우면 좋은 것=기본 강조색. 글자에는 색을 입히지 않는다. */
const SECTION_ACCENT: Record<ReviewSectionKind, string> = {
  good: "border-l-[var(--viz-good)]",
  bad: "border-l-[var(--viz-warning)]",
  next: "border-l-[var(--viz-series-1)]",
};

function axisSummary(items: ReviewItem[]): string {
  const counts = new Map<string, number>();
  for (const item of items) {
    if (item.axis) counts.set(item.axis, (counts.get(item.axis) ?? 0) + 1);
  }
  return [...counts.entries()].map(([axis, count]) => `${axis} ${count}`).join(" · ");
}

/** 칸 제목은 모델이 쓴 문자열 대신 앱 문구를 쓴다 — 모델이 제목을 조금 바꿔 써도 보고서는 일정하다. */
function sectionHeading(
  t: Translate,
  kind: ReviewSectionKind,
  periodType: ReviewPeriodType,
): string {
  if (kind === "good") return t("review.report.section.good");
  if (kind === "next") return t("review.report.section.next");
  return t(periodType === "month" ? "review.report.section.badMonth" : "review.report.section.bad");
}

function SectionCard({
  kind,
  items,
  periodType,
}: ParsedReview["sections"][number] & { periodType: ReviewPeriodType }) {
  const t = useT();
  const summary = axisSummary(items);
  const heading = sectionHeading(t, kind, periodType);
  return (
    <section className={cn("rounded-md border border-border border-l-2 p-4", SECTION_ACCENT[kind])}>
      <header className="flex flex-wrap items-baseline justify-between gap-2">
        <h4 className="text-sm font-semibold">{heading}</h4>
        {summary && (
          <span className="text-[11px] text-muted-foreground">
            {t("review.report.axisSummary")} · {summary}
          </span>
        )}
      </header>
      <ul className="mt-3 space-y-3">
        {items.map((item, index) => (
          // biome-ignore lint/suspicious/noArrayIndexKey: 항목은 본문을 매번 통째로 다시 파싱해 만들어 순서가 바뀌지 않는다.
          <li key={index} className="space-y-1">
            {item.headline && <p className="text-sm font-medium">{item.headline}</p>}
            {item.body && (
              <p
                className={cn(
                  "text-sm leading-relaxed",
                  item.headline ? "text-muted-foreground" : "text-foreground",
                )}
              >
                {item.body}
              </p>
            )}
            {(item.axis || item.beyondGoals || item.evidence.length > 0) && (
              <div className="flex flex-wrap items-center gap-1">
                {item.beyondGoals && (
                  <span className="rounded border border-border px-1.5 py-0.5 text-[11px] text-muted-foreground">
                    {t("review.report.beyondGoals")}
                  </span>
                )}
                {item.axis && (
                  <span className="rounded bg-accent px-1.5 py-0.5 text-[11px] font-medium text-accent-foreground">
                    {item.axis}
                  </span>
                )}
                {item.evidence.map((ref) => (
                  <span
                    key={ref}
                    className="rounded border border-border px-1.5 py-0.5 font-mono text-[10px] text-muted-foreground"
                  >
                    {ref}
                  </span>
                ))}
              </div>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}

/**
 * 업무평가서 보고서 렌더(ADR-0017). AI는 구조가 정해진 텍스트만 쓰고(`- **요지** — 설명 [축] (근거)`),
 * 보고서 모양은 전부 여기서 그린다 — 그리는 데 드는 토큰은 0이다. 숫자 타일·차트는 생성 시점 DB
 * 스냅샷(`review.stats`)이라 AI를 거치지 않는다. 본문이 형식을 벗어나 세 칸을 못 찾으면 `fallback`을
 * 그린다(일반 요약 렌더).
 */
export function ReviewReport({
  review,
  title,
  subtitle,
  fallback,
}: {
  review: PerformanceReview;
  title: string;
  subtitle: string;
  fallback: ReactNode;
}) {
  const parsed = parseReview(review.content);
  return (
    <div className="space-y-4">
      <header className="space-y-1">
        <h3 className="text-lg font-semibold">{title}</h3>
        <p className="text-xs text-muted-foreground">{subtitle}</p>
        {parsed.overview.map((line) => (
          <p key={line} className="text-sm text-muted-foreground">
            {line}
          </p>
        ))}
      </header>
      {review.stats && <StatTiles stats={review.stats} />}
      {parsed.sections.length > 0
        ? parsed.sections.map((section) => (
            <SectionCard key={section.kind} {...section} periodType={review.periodType} />
          ))
        : fallback}
      {review.stats && <ActivityPanel stats={review.stats} />}
    </div>
  );
}
