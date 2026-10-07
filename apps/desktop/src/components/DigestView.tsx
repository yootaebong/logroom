import type { Source } from "@logroom/core";
import { useQuery } from "@tanstack/react-query";
import {
  Bot,
  Code2,
  FileText,
  GitBranch,
  ListTodo,
  MessageSquare,
  PenLine,
  Sparkles,
  Terminal,
} from "lucide-react";
import { type ComponentType, useEffect, useMemo, useState } from "react";
import type { Translate } from "@/i18n";
import { useResolvedLocale, useT } from "@/i18n";
import { getDailySummary, getDigest, queryKeys } from "@/lib/api";
import type { DigestStreamItem, DigestTotals } from "@/lib/types";
import {
  cn,
  lastPathSegment,
  MESSAGE_SOURCES,
  projectDisplayName,
  projectMatchesFilter,
  sourceLabel,
  uniqueProjectNames,
} from "@/lib/utils";
import { LOCAL_TIMEZONE, useAppStore } from "@/store";

/** 토큰 수 k 단위 축약 기준값. */
const TOKEN_UNIT_DIVISOR = 1000;

/** 섹션당 기본 표시 행 수 — 초과분은 "더보기 N건"으로 접는다(Linear 이슈 27행 같은 대량 나열 방지,
 * 실사용 피드백 "다이제스트 목록 보기 어렵다"). */
const MAX_VISIBLE_ITEMS = 5;

const SOURCE_ICONS: Record<Source, ComponentType<{ className?: string }>> = {
  claude_code: Bot,
  kiro_cli: Terminal,
  kiro_ide: Code2,
  gemini_web: Sparkles,
  manual: PenLine,
  slack: MessageSquare,
  github: GitBranch,
  linear: ListTodo,
  notion: FileText,
};

function formatTime(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function formatTimeRange(startMs: number, endMs: number): string {
  return `${formatTime(startMs)}~${formatTime(endMs)}`;
}

/** 1000 미만은 그대로, 이상이면 소수 1자리 k 단위로 축약(예: 12345 -> "12.3k"). */
function formatTokenCount(n: number): string {
  if (n < TOKEN_UNIT_DIVISOR) return String(n);
  return `${(n / TOKEN_UNIT_DIVISOR).toFixed(1)}k`;
}

/** AI 요약 바로가기 한 줄 — 원래 다이제스트 상단에 요약 카드(DailySummaryCard)가 통째로 있었지만
 * "읽는 콘텐츠"라 훑기용 다이제스트와 밀도가 충돌해(실사용 피드백) 전용 뷰(SummaryView)로 분리하고
 * 여기엔 바로가기만 남긴다. 캐시 존재 여부로 라벨을 바꾼다(보기 vs 생성). */
function SummaryShortcutRow() {
  const t = useT();
  const locale = useResolvedLocale();
  const currentDate = useAppStore((state) => state.currentDate);
  const setView = useAppStore((state) => state.setView);
  const { data: summary } = useQuery({
    queryKey: queryKeys.dailySummary(currentDate, locale),
    queryFn: () => getDailySummary(currentDate, locale),
  });

  return (
    <div className="border-b border-border px-4 py-1.5">
      <button
        type="button"
        onClick={() => setView("summary")}
        className="flex items-center gap-1.5 rounded-md text-xs text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <Sparkles className="size-3.5" aria-hidden="true" />
        {summary ? t("digest.summaryShortcutView") : t("digest.summaryShortcutGenerate")}
        <span aria-hidden="true">→</span>
      </button>
    </div>
  );
}

function SummaryRow({ totals }: { totals: DigestTotals }) {
  const t = useT();
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 border-b border-border px-4 py-3 text-sm">
      <span>
        {t("common.label.sessions")}{" "}
        <strong className="font-semibold tabular-nums">{totals.streams}</strong>
        <span className="text-muted-foreground">
          {" "}
          ({t("common.label.agents")} {totals.agentStreams})
        </span>
      </span>
      <span aria-hidden="true" className="text-muted-foreground">
        ·
      </span>
      <span>
        {t("common.label.prompts")}{" "}
        <strong className="font-semibold tabular-nums">{totals.prompts}</strong>
      </span>
      <span aria-hidden="true" className="text-muted-foreground">
        ·
      </span>
      <span>
        {t("common.label.responses")}{" "}
        <strong className="font-semibold tabular-nums">{totals.responses}</strong>
      </span>
      <span aria-hidden="true" className="text-muted-foreground">
        ·
      </span>
      <span>
        {t("common.label.tools")}{" "}
        <strong className="font-semibold tabular-nums">{totals.toolEvents}</strong>
      </span>
      <span aria-hidden="true" className="text-muted-foreground">
        ·
      </span>
      <span className="tabular-nums">
        {t("common.tokensInOut", {
          inCount: formatTokenCount(totals.tokensIn),
          outCount: formatTokenCount(totals.tokensOut),
        })}
      </span>
      <span aria-hidden="true" className="text-muted-foreground">
        ·
      </span>
      <span className="tabular-nums">
        {t("common.label.activeTime")}{" "}
        {totals.firstTs !== null && totals.lastTs !== null
          ? formatTimeRange(totals.firstTs, totals.lastTs)
          : "-"}
      </span>
    </div>
  );
}

/** 같은 표시 이름(마지막 세그먼트)의 BE 프로젝트 섹션들을 병합한 렌더 단위 — Claude(로컬 경로)와
 * GitHub(owner/repo)의 project 원본 키가 달라 같은 저장소가 소스별로 섹션이 분열되던 문제의 해법
 * (실사용: 하루 17섹션 중 5쌍이 중복이었음). */
interface MergedProjectSection {
  /** 병합 키 = 표시 이름(미상 프로젝트는 `__unknown-<i>`로 병합하지 않음). */
  key: string;
  label: string;
  /** 병합된 원본 project 키들( title 속성용, " · " 연결). */
  full: string;
  streams: number;
  prompts: number;
  tokensIn: number;
  tokensOut: number;
  firstTs: number;
  lastTs: number;
  sources: Source[];
  items: DigestStreamItem[];
}

/** 행 제목: 메시지형 스트림 제목이 프로젝트 표시 이름과 같으면(예: GitHub repo 스트림 title =
 * `owner/repo`) 정보가 0이므로 "GitHub 활동" 같은 소스 라벨로 대체한다. */
function rowTitle(item: DigestStreamItem, projectLabel: string, t: Translate): string {
  const raw = item.title ?? item.streamId;
  if (MESSAGE_SOURCES.includes(item.source) && lastPathSegment(raw) === projectLabel) {
    return t("digest.sourceActivity", { source: sourceLabel(item.source, t) });
  }
  return raw;
}

interface ProjectStreamRowProps {
  item: DigestStreamItem;
  projectLabel: string;
  isSelected: boolean;
  onSelect: () => void;
}

function ProjectStreamRow({ item, projectLabel, isSelected, onSelect }: ProjectStreamRowProps) {
  const t = useT();
  const timeRange = formatTimeRange(item.startedAt, item.endedAt);
  const title = rowTitle(item, projectLabel, t);
  const Icon = SOURCE_ICONS[item.source];
  // 메시지형(Slack/GitHub/Linear/Notion)은 prompt 이벤트가 없어 "프롬프트 0"이 무의미 — 숨긴다.
  const promptsLabel = MESSAGE_SOURCES.includes(item.source)
    ? null
    : `${t("common.label.prompts")} ${item.prompts}`;
  const hoverTitle = [`${sourceLabel(item.source, t)} · ${title}`, timeRange, promptsLabel]
    .filter(Boolean)
    .join(" • ");

  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        aria-pressed={isSelected}
        title={hoverTitle}
        className={cn(
          "flex w-full items-center gap-3 rounded-md px-3 py-2 text-left text-sm transition-colors hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
          isSelected && "bg-accent/60 outline outline-2 outline-primary",
        )}
      >
        <span className="shrink-0 tabular-nums text-xs text-muted-foreground">{timeRange}</span>
        <Icon className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
        <span className="min-w-0 flex-1 truncate font-medium">{title}</span>
        {promptsLabel && (
          <span className="shrink-0 tabular-nums text-xs text-muted-foreground">
            {promptsLabel}
          </span>
        )}
      </button>
    </li>
  );
}

interface ProjectSectionProps {
  section: MergedProjectSection;
  isExpanded: boolean;
  onToggleExpand: () => void;
  selectedStreamId: string | null;
  onSelectStream: (id: string) => void;
}

function ProjectSection({
  section,
  isExpanded,
  onToggleExpand,
  selectedStreamId,
  onSelectStream,
}: ProjectSectionProps) {
  const t = useT();
  const hiddenCount = section.items.length - MAX_VISIBLE_ITEMS;
  const visibleItems = isExpanded ? section.items : section.items.slice(0, MAX_VISIBLE_ITEMS);

  return (
    <section className="border-b border-border last:border-b-0">
      <header className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 bg-muted/30 px-4 py-2">
        <h2 className="min-w-0 truncate text-sm font-semibold" title={section.full}>
          {section.label}
        </h2>
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
          <span className="tabular-nums">
            {t("common.label.sessions")} {section.streams}
          </span>
          <span className="tabular-nums">
            {t("common.label.prompts")} {section.prompts}
          </span>
          <span className="tabular-nums">
            {t("common.tokensInOut", {
              inCount: formatTokenCount(section.tokensIn),
              outCount: formatTokenCount(section.tokensOut),
            })}
          </span>
          <span className="tabular-nums">{formatTimeRange(section.firstTs, section.lastTs)}</span>
          <span className="flex flex-wrap items-center gap-x-2">
            {section.sources.map((source) => {
              const Icon = SOURCE_ICONS[source];
              return (
                <span key={source} className="flex items-center gap-1">
                  <Icon className="size-3.5" aria-hidden="true" />
                  {sourceLabel(source, t)}
                </span>
              );
            })}
          </span>
        </div>
      </header>
      <ul className="space-y-0.5 p-2">
        {visibleItems.map((item) => (
          <ProjectStreamRow
            key={item.streamId}
            item={item}
            projectLabel={section.label}
            isSelected={selectedStreamId === item.streamId}
            onSelect={() => onSelectStream(item.streamId)}
          />
        ))}
        {hiddenCount > 0 && (
          <li>
            <button
              type="button"
              aria-expanded={isExpanded}
              onClick={onToggleExpand}
              className="w-full rounded-md px-3 py-1.5 text-left text-xs text-muted-foreground transition-colors hover:bg-accent/40 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              {isExpanded ? t("digest.showLess") : t("digest.showMore", { n: hiddenCount })}
            </button>
          </li>
        )}
      </ul>
    </section>
  );
}

/**
 * LogRoom 다이제스트("오늘 한 일") 화면: 날짜별 요약 카드 + 프로젝트별 스트림 목록.
 * docs/05-ui-ux.md M4 참고. 스트림 클릭 시 기존 우측 상세 패널(StreamDetailPanel)이 연다.
 */
export function DigestView() {
  const t = useT();
  const currentDate = useAppStore((state) => state.currentDate);
  const selectedStreamId = useAppStore((state) => state.selectedStreamId);
  const setSelectedStreamId = useAppStore((state) => state.setSelectedStreamId);
  const setNavigableStreamIds = useAppStore((state) => state.setNavigableStreamIds);
  const setDayProjects = useAppStore((state) => state.setDayProjects);
  const viewFilters = useAppStore((state) => state.viewFilters);

  const { data, isLoading, isError } = useQuery({
    queryKey: queryKeys.digest(currentDate, LOCAL_TIMEZONE),
    queryFn: () => getDigest(currentDate, LOCAL_TIMEZONE),
  });

  const isFilterActive = viewFilters.sources.length > 0 || viewFilters.project !== null;

  /** "더보기"로 펼친 섹션 키 집합 — 날짜를 옮기면 초기화한다(다른 날의 같은 프로젝트가 펼쳐진
   * 채로 시작하는 놀람 방지). */
  const [expandedSections, setExpandedSections] = useState<ReadonlySet<string>>(new Set());
  // biome-ignore lint/correctness/useExhaustiveDependencies: currentDate 변경 시에만 초기화하는 의도적 리셋.
  useEffect(() => {
    setExpandedSections(new Set());
  }, [currentDate]);

  /** 소스/프로젝트 뷰 필터를 반영한 뒤 **표시 이름 기준으로 섹션을 병합**한다(totals는 재계산하지
   * 않음 — App.tsx 헤더의 "필터 적용 중" 표시로 요약 수치와의 괴리를 안내한다). `viewFilters.project`
   * 는 표시 이름이라 project 원본 키가 소스마다 달라도(Claude 절대경로 vs GitHub owner/repo)
   * `projectMatchesFilter`로 비교하고, 같은 표시 이름의 섹션(같은 저장소의 Claude/GitHub 분열)은
   * 하나로 합친다 — 수치는 합산, 항목은 시작 시각 오름차순 병합(MergedProjectSection 참고). */
  const visibleProjects = useMemo<MergedProjectSection[]>(() => {
    const projects = data?.projects ?? [];
    const merged = new Map<string, MergedProjectSection>();
    let unknownIndex = 0;
    for (const project of projects) {
      if (
        viewFilters.project !== null &&
        !projectMatchesFilter(project.project, viewFilters.project)
      ) {
        continue;
      }
      const items =
        viewFilters.sources.length === 0
          ? project.items
          : project.items.filter((item) => viewFilters.sources.includes(item.source));
      // 소스 필터로 items가 전부 걸러진 프로젝트는 섹션 자체를 숨긴다 — 빈 헤더(원본 소스
      // 아이콘 포함)가 남으면 "필터가 안 걸린 것처럼" 보인다(실사용 버그 리포트).
      if (viewFilters.sources.length > 0 && items.length === 0) continue;

      const { label, full } = projectDisplayName(project.project, t("common.unknownProject"));
      const key = project.project === null ? `__unknown-${unknownIndex++}` : label;
      const existing = merged.get(key);
      if (!existing) {
        merged.set(key, {
          key,
          label,
          full,
          streams: project.streams,
          prompts: project.prompts,
          tokensIn: project.tokensIn,
          tokensOut: project.tokensOut,
          firstTs: project.firstTs,
          lastTs: project.lastTs,
          sources: [...project.sources],
          items: [...items],
        });
        continue;
      }
      existing.full = `${existing.full} · ${full}`;
      existing.streams += project.streams;
      existing.prompts += project.prompts;
      existing.tokensIn += project.tokensIn;
      existing.tokensOut += project.tokensOut;
      existing.firstTs = Math.min(existing.firstTs, project.firstTs);
      existing.lastTs = Math.max(existing.lastTs, project.lastTs);
      for (const source of project.sources) {
        if (!existing.sources.includes(source)) existing.sources.push(source);
      }
      existing.items.push(...items);
    }
    const sections = [...merged.values()];
    for (const section of sections) {
      section.items.sort((a, b) => a.startedAt - b.startedAt);
    }
    return sections;
  }, [data?.projects, viewFilters, t]);

  /** 그날 존재하는 고유 프로젝트 표시 이름 목록을 등록한다(App.tsx 프로젝트 필터 select 옵션용,
   * 필터 미적용 원본 기준). */
  useEffect(() => {
    setDayProjects(uniqueProjectNames((data?.projects ?? []).map((project) => project.project)));
  }, [data?.projects, setDayProjects]);

  /** 섹션 순회 items 순서(flat)를 키보드 j/k 이동용으로 등록한다(필터 결과 기준, docs/05 "키보드
   * 우선"). "더보기"로 접힌 항목은 화면에 없으므로 네비 대상에서도 제외한다(보이는 것만 이동). */
  useEffect(() => {
    const ids = visibleProjects.flatMap((section) =>
      (expandedSections.has(section.key)
        ? section.items
        : section.items.slice(0, MAX_VISIBLE_ITEMS)
      ).map((item) => item.streamId),
    );
    setNavigableStreamIds(ids);
  }, [visibleProjects, expandedSections, setNavigableStreamIds]);

  function toggleSectionExpanded(key: string) {
    setExpandedSections((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  }

  return (
    <div className="flex h-full flex-col">
      {(isLoading || isError) && (
        <div className="flex items-center gap-2 border-b border-border px-4 py-1.5 text-xs">
          {isLoading && <span className="text-muted-foreground">{t("digest.loading")}</span>}
          {isError && <span className="text-destructive">{t("digest.loadError")}</span>}
        </div>
      )}

      <SummaryShortcutRow />
      {data && <SummaryRow totals={data.totals} />}
      {isFilterActive && (
        <p className="border-b border-border bg-muted/20 px-4 py-1.5 text-xs text-muted-foreground">
          {t("digest.filterActiveNotice")}
        </p>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto">
        {data && data.projects.length === 0 && (
          <p className="py-8 text-center text-sm text-muted-foreground">{t("digest.emptyDay")}</p>
        )}
        {data && data.projects.length > 0 && visibleProjects.length === 0 && (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("digest.emptyFiltered")}
          </p>
        )}
        {visibleProjects.map((section) => (
          <ProjectSection
            key={section.key}
            section={section}
            isExpanded={expandedSections.has(section.key)}
            onToggleExpand={() => toggleSectionExpanded(section.key)}
            selectedStreamId={selectedStreamId}
            onSelectStream={setSelectedStreamId}
          />
        ))}
      </div>
    </div>
  );
}
