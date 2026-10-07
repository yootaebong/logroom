import type { EventType, Source } from "@logroom/core";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { useT } from "@/i18n";
import { listEventsByDay, queryKeys } from "@/lib/api";
import type { EventMarker, StreamRow } from "@/lib/types";
import {
  cn,
  getParentStreamId,
  isHubStream,
  MESSAGE_SOURCES,
  projectMatchesFilter,
  sourceLabel,
  uniqueProjectNames,
} from "@/lib/utils";
import type { ViewFilters } from "@/store";
import { LOCAL_TIMEZONE, toLocalDateString, useAppStore } from "@/store";

const DAY_MS = 24 * 60 * 60 * 1000;
const MINUTE_MS = 60 * 1000;
const HOUR_MS = 60 * MINUTE_MS;
/** 축 패딩(활동 구간 앞뒤 여백) — docs/05 "동적 시간축". */
const AXIS_PADDING_MS = 30 * MINUTE_MS;
/** 극단적으로 짧은 활동 구간이어도 보장하는 최소 축 스팬. */
const MIN_AXIS_SPAN_MS = 2 * HOUR_MS;
/** 시간 눈금 후보 간격(분). 축 스팬에 맞춰 눈금 개수가 TICK_TARGET_COUNT 이하가 되는 첫 값을 쓴다. */
const TICK_INTERVALS_MIN = [15, 30, 60, 120, 180, 360, 720];
const TICK_TARGET_COUNT = 8;

const LANE_HEIGHT_PX = 44;
const LANE_PADDING_PX = 6;
/** 메시지형(Slack 등) 바 높이 — 세션 바(LANE_HEIGHT_PX - LANE_PADDING_PX)보다 낮게 그려 "지속형"이
 * 아닌 "발화형" 활동임을 시각적으로 구분한다. 레인 안에서 세로로 중앙 정렬된다. */
const MESSAGE_BAR_HEIGHT_PX = 24;
/** 클릭 가능한 최소 크기 확보를 위한 바 최소폭(축 스팬 대비 %). */
const MIN_BAR_WIDTH_PCT = 2.5;
/** agent 하위 레인 시각적 들여쓰기(px). 실제 시간 정렬에서 미세하게 벗어나지만 계층을 표현한다. */
const AGENT_INDENT_PX = 12;
/** 독립 에이전트(엣지 케이스) 섹션 라벨의 높이(px). */
const ORPHAN_HEADER_PX = 20;

// text-white 대비 4.5:1(WCAG AA) 미달 색상은 한 단계 진한 값으로 대체.
const SOURCE_INITIALS: Record<Source, string> = {
  claude_code: "C",
  kiro_cli: "K",
  kiro_ide: "I",
  gemini_web: "G",
  manual: "M",
  slack: "S",
  github: "G",
  linear: "L",
  notion: "N",
};

/** 스트림이 메시지형(발화 기록) 소스인지 판정한다 — 세션과 달리 started_at~ended_at이 "지속"을
 * 의미하지 않고 채널/DM/저장소 전체 수명을 가리킬 수 있다(수개월 단위). 소스 목록은
 * lib/utils.ts::MESSAGE_SOURCES 공용. */
function isMessageStream(stream: StreamRow): boolean {
  return MESSAGE_SOURCES.includes(stream.source);
}

/** project 문자열 해시 → 고정 팔레트. 전부 700계열(text-white 대비 AA 4.5:1 이상 확인됨). */
const PROJECT_COLOR_PALETTE = [
  "bg-red-700",
  "bg-orange-700",
  "bg-amber-700",
  "bg-green-700",
  "bg-teal-700",
  "bg-cyan-700",
  "bg-blue-700",
  "bg-indigo-700",
  "bg-purple-700",
  "bg-pink-700",
];
/** project가 null인 스트림에 쓰는 중립색. */
const NEUTRAL_PROJECT_COLOR = "bg-zinc-600";

function hashStringToIndex(value: string, modulo: number): number {
  let hash = 0;
  for (let i = 0; i < value.length; i += 1) {
    hash = (hash * 31 + value.charCodeAt(i)) | 0;
  }
  return Math.abs(hash) % modulo;
}

/** project 문자열 → 고정 색상 클래스. Digest 등 다른 화면에서도 재사용 가능하도록 export. */
export function getProjectColor(project: string | null): string {
  if (!project) return NEUTRAL_PROJECT_COLOR;
  const index = hashStringToIndex(project, PROJECT_COLOR_PALETTE.length);
  return PROJECT_COLOR_PALETTE[index] ?? NEUTRAL_PROJECT_COLOR;
}

interface ClippedStream {
  stream: StreamRow;
  clippedStart: number;
  clippedEnd: number;
}

interface LanePlacement extends ClippedStream {
  lane: number;
}

function getDayBounds(localDate: string): { startMs: number; endMs: number } {
  const [year, month, day] = localDate.split("-").map(Number);
  const startMs = new Date(year ?? 1970, (month ?? 1) - 1, day ?? 1, 0, 0, 0, 0).getTime();
  return { startMs, endMs: startMs + DAY_MS };
}

/**
 * 스트림을 [startMs, endMs] 구간으로 클리핑하고 clippedStart 오름차순 정렬한다.
 * 메시지형 스트림(isMessageStream)은 started_at~ended_at 대신 그날 마커(markerRangeByStream)의
 * min~max ts를 구간으로 쓴다 — "지속"이 아니라 "발화 시점들"이 활동 범위이기 때문(docs 참고).
 * 그날 마커가 없는 메시지형 스트림(엣지 케이스)은 결과에서 제외한다.
 */
function clipToRange(
  streams: StreamRow[],
  startMs: number,
  endMs: number,
  markerRangeByStream: Map<string, { min: number; max: number }>,
): ClippedStream[] {
  const items: ClippedStream[] = [];
  for (const stream of streams) {
    if (isMessageStream(stream)) {
      const range = markerRangeByStream.get(stream.id);
      if (!range) continue;
      items.push({ stream, clippedStart: range.min, clippedEnd: range.max });
      continue;
    }
    const clippedStart = Math.max(stream.startedAt, startMs);
    const clippedEnd = Math.min(stream.endedAt ?? Date.now(), endMs);
    if (clippedEnd < clippedStart) continue;
    items.push({ stream, clippedStart, clippedEnd });
  }
  return items.sort((a, b) => a.clippedStart - b.clippedStart);
}

/** 구간 분할(greedy interval partitioning) — docs/02 "레인 배치" 알고리즘. kind==="session"만 대상. */
function assignLanes(
  streams: StreamRow[],
  startMs: number,
  endMs: number,
  markerRangeByStream: Map<string, { min: number; max: number }>,
): LanePlacement[] {
  const clipped = clipToRange(streams, startMs, endMs, markerRangeByStream);
  const laneEnds: number[] = [];
  const placements: LanePlacement[] = [];

  for (const item of clipped) {
    let lane = laneEnds.findIndex((end) => end <= item.clippedStart);
    if (lane === -1) {
      lane = laneEnds.length;
      laneEnds.push(item.clippedEnd);
    } else {
      laneEnds[lane] = item.clippedEnd;
    }
    placements.push({ ...item, lane });
  }

  return placements;
}

/** 소스/프로젝트 뷰 필터에 스트림 1개가 부합하는지 판정한다(빈 sources/project=null은 전체).
 * `filters.project`는 project 원본 키가 아니라 표시 이름이라 `projectMatchesFilter`로 비교한다
 * (project 키가 소스마다 달라도 표시 이름이 같으면 매칭 — lib/utils.ts 참고). */
function matchesViewFilters(stream: StreamRow, filters: ViewFilters): boolean {
  if (filters.sources.length > 0 && !filters.sources.includes(stream.source)) return false;
  if (filters.project !== null && !projectMatchesFilter(stream.project, filters.project))
    return false;
  return true;
}

/** 그날 스트림 전체에 뷰 필터를 적용한다. agent 스트림은 자신의 source/project가 아니라
 * 부모 세션(metadata.parentStream) 기준으로 필터를 따른다(부모를 찾을 수 없는 엣지 케이스만
 * agent 자신의 필드로 판정). 단 부모가 hub 세션(metadata.hub)이면 부모 project 는 레포 밖 시작
 * 폴더라 의미가 없으므로, 실제 레포로 고쳐 둔 agent 자신의 project 로 판정한다. */
function filterStreamsByView(streams: StreamRow[], filters: ViewFilters): StreamRow[] {
  const byId = new Map(streams.map((stream) => [stream.id, stream]));
  return streams.filter((stream) => {
    if (stream.kind !== "agent") return matchesViewFilters(stream, filters);
    const parentId = getParentStreamId(stream.metadata);
    const parent = parentId ? byId.get(parentId) : undefined;
    if (parent && isHubStream(parent.metadata)) return matchesViewFilters(stream, filters);
    return matchesViewFilters(parent ?? stream, filters);
  });
}

/** agent 스트림을 metadata.parentStream 기준으로 부모 세션 id에 그룹핑한다. */
function groupAgentsByParent(agentStreams: StreamRow[]): Map<string, StreamRow[]> {
  const map = new Map<string, StreamRow[]>();
  for (const agent of agentStreams) {
    const parentId = getParentStreamId(agent.metadata);
    if (!parentId) continue;
    const existing = map.get(parentId);
    if (existing) {
      existing.push(agent);
    } else {
      map.set(parentId, [agent]);
    }
  }
  return map;
}

/** 그날 스트림들의 클리핑 구간 min~max ±30분 패딩(최소 스팬 2시간 보장). 데이터 없으면 하루 전체.
 * 메시지형 스트림은 started_at~ended_at(채널 전체 수명)이 아니라 그날 마커 범위를 반영한다. */
function computeAxisRange(
  streams: StreamRow[],
  dayStart: number,
  dayEnd: number,
  markerRangeByStream: Map<string, { min: number; max: number }>,
): { axisStart: number; axisEnd: number } {
  let min = Number.POSITIVE_INFINITY;
  let max = Number.NEGATIVE_INFINITY;
  for (const stream of streams) {
    let start: number;
    let end: number;
    if (isMessageStream(stream)) {
      const range = markerRangeByStream.get(stream.id);
      if (!range) continue;
      start = range.min;
      end = range.max;
    } else {
      start = Math.max(stream.startedAt, dayStart);
      end = Math.min(stream.endedAt ?? Date.now(), dayEnd);
    }
    if (end < start) continue;
    min = Math.min(min, start);
    max = Math.max(max, end);
  }

  if (!Number.isFinite(min) || !Number.isFinite(max)) {
    return { axisStart: dayStart, axisEnd: dayEnd };
  }

  let axisStart = Math.max(min - AXIS_PADDING_MS, dayStart);
  let axisEnd = Math.min(max + AXIS_PADDING_MS, dayEnd);

  if (axisEnd - axisStart < MIN_AXIS_SPAN_MS) {
    const center = (axisStart + axisEnd) / 2;
    let candidateStart = center - MIN_AXIS_SPAN_MS / 2;
    let candidateEnd = center + MIN_AXIS_SPAN_MS / 2;
    if (candidateStart < dayStart) {
      candidateEnd += dayStart - candidateStart;
      candidateStart = dayStart;
    }
    if (candidateEnd > dayEnd) {
      candidateStart -= candidateEnd - dayEnd;
      candidateEnd = dayEnd;
    }
    axisStart = Math.max(candidateStart, dayStart);
    axisEnd = Math.min(candidateEnd, dayEnd);
  }

  return { axisStart, axisEnd };
}

/** 축 스팬에 맞춰 정시/30분 등 간격의 눈금(epoch ms) 목록을 만든다. dayStart를 기준으로 정렬한다. */
function computeAxisTicks(axisStart: number, axisEnd: number, dayStart: number): number[] {
  const spanMin = (axisEnd - axisStart) / MINUTE_MS;
  const intervalMin =
    TICK_INTERVALS_MIN.find((candidate) => spanMin / candidate <= TICK_TARGET_COUNT) ??
    TICK_INTERVALS_MIN[TICK_INTERVALS_MIN.length - 1] ??
    60;
  const intervalMs = intervalMin * MINUTE_MS;
  const firstTick = dayStart + Math.ceil((axisStart - dayStart) / intervalMs) * intervalMs;

  const ticks: number[] = [];
  for (let tick = firstTick; tick <= axisEnd; tick += intervalMs) {
    ticks.push(tick);
  }
  return ticks;
}

function formatHourMinute(ms: number): string {
  const date = new Date(ms);
  const hours = String(date.getHours()).padStart(2, "0");
  const minutes = String(date.getMinutes()).padStart(2, "0");
  return `${hours}:${minutes}`;
}

function toLeftPct(ms: number, axisStart: number, axisSpanMs: number): number {
  return ((ms - axisStart) / axisSpanMs) * 100;
}

function toWidthPct(startMsVal: number, endMsVal: number, axisSpanMs: number): number {
  return Math.max(((endMsVal - startMsVal) / axisSpanMs) * 100, MIN_BAR_WIDTH_PCT);
}

/** 바 위 마커는 prompt/response(요청-응답 히스토리)만 표시한다. */
const TIMELINE_MARKER_TYPES = new Set<EventType>(["prompt", "response"]);

/**
 * streamId → 그날 마커의 [min, max] ts. 메시지형 스트림(clipToRange/computeAxisRange)의 활동 구간
 * 산출용 — 이벤트 타입 제한 없이 그날 전체 마커를 본다(TIMELINE_MARKER_TYPES는 바 위 점 표시 필터일
 * 뿐 구간 산출과 무관하며, Slack 이벤트는 type="message"라 그 필터에 포함되지 않는다).
 */
function computeMarkerRangeByStream(
  events: EventMarker[],
): Map<string, { min: number; max: number }> {
  const map = new Map<string, { min: number; max: number }>();
  for (const marker of events) {
    const existing = map.get(marker.streamId);
    if (existing) {
      existing.min = Math.min(existing.min, marker.ts);
      existing.max = Math.max(existing.max, marker.ts);
    } else {
      map.set(marker.streamId, { min: marker.ts, max: marker.ts });
    }
  }
  return map;
}

function groupMarkersByStream(events: EventMarker[]): Map<string, EventMarker[]> {
  const map = new Map<string, EventMarker[]>();
  for (const marker of events) {
    if (!TIMELINE_MARKER_TYPES.has(marker.type)) continue;
    const existing = map.get(marker.streamId);
    if (existing) {
      existing.push(marker);
    } else {
      map.set(marker.streamId, [marker]);
    }
  }
  return map;
}

interface AgentBadgeProps {
  count: number;
  expanded: boolean;
  onToggle: () => void;
}

interface TimelineBarProps {
  stream: StreamRow;
  top: number;
  leftPct: number;
  widthPct: number;
  markers: EventMarker[];
  clippedStart: number;
  clippedEnd: number;
  isSelected: boolean;
  onSelect: () => void;
  indent?: boolean;
  /** indent(하위 에이전트) 바의 접근성 이름에 쓸 부모 세션 title. 알 수 없으면 생략. */
  parentTitle?: string;
  agentBadge?: AgentBadgeProps;
}

/** 타임라인 바 1개(세션/agent 공용). 배지가 있으면 바 클릭과 분리된 독립 버튼으로 렌더한다. */
function TimelineBar({
  stream,
  top,
  leftPct,
  widthPct,
  markers,
  clippedStart,
  clippedEnd,
  isSelected,
  onSelect,
  indent = false,
  parentTitle,
  agentBadge,
}: TimelineBarProps) {
  const t = useT();
  const isActive = stream.status === "active";
  const isAgent = stream.kind === "agent";
  const isMessage = isMessageStream(stream);
  const barHeightPx = isMessage ? MESSAGE_BAR_HEIGHT_PX : LANE_HEIGHT_PX - LANE_PADDING_PX;
  const barTopOffsetPx = isMessage
    ? (LANE_HEIGHT_PX - LANE_PADDING_PX - MESSAGE_BAR_HEIGHT_PX) / 2
    : 0;
  const barSpanMs = Math.max(clippedEnd - clippedStart, 1);
  const title = stream.title ?? stream.id;
  const timeRange = `${formatHourMinute(clippedStart)}~${formatHourMinute(clippedEnd)}`;
  const tooltip = [
    stream.source,
    title,
    stream.project ?? t("common.noProject"),
    timeRange,
    agentBadge ? t("timeline.agentCountTooltip", { count: agentBadge.count }) : null,
    isHubStream(stream.metadata) ? t("common.viaAgent") : null,
  ]
    .filter(Boolean)
    .join(" · ");
  /**
   * 버튼 접근성 이름. 하위 에이전트(↳) 바는 시각 기호 대신 "에이전트: … (부모: …)"로,
   * 그 외 바는 source 전체명을 포함해 뱃지(장식)만으로는 전달되지 않는 정보를 보완한다.
   */
  const accessibleName =
    indent && isAgent
      ? [
          t("timeline.agentAccessibleName", { title }),
          parentTitle ? t("timeline.parentAccessibleSuffix", { parent: parentTitle }) : null,
        ]
          .filter(Boolean)
          .join(" ")
      : [sourceLabel(stream.source, t), title, isActive ? `(${t("timeline.liveLabel")})` : null]
          .filter(Boolean)
          .join(" • ");

  return (
    <div
      className="absolute flex items-stretch overflow-hidden rounded-md shadow-sm"
      style={{
        top: top + barTopOffsetPx,
        height: barHeightPx,
        left: indent ? `calc(${leftPct}% + ${AGENT_INDENT_PX}px)` : `${leftPct}%`,
        width: indent ? `calc(${widthPct}% - ${AGENT_INDENT_PX}px)` : `${widthPct}%`,
      }}
    >
      <button
        type="button"
        onClick={onSelect}
        title={tooltip}
        aria-label={accessibleName}
        aria-pressed={isSelected}
        className={cn(
          "relative flex min-w-0 flex-1 items-center gap-1 overflow-hidden px-2 text-left text-xs font-medium text-white transition-opacity hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
          getProjectColor(stream.project),
          isAgent && "border-2 border-dashed border-white/70",
          isMessage && "opacity-80",
          isActive && "ring-2 ring-emerald-400 ring-offset-1 ring-offset-background",
          isSelected && "outline outline-2 outline-primary",
        )}
      >
        <span
          className="flex size-4 shrink-0 items-center justify-center rounded-sm bg-black/30 text-[9px] font-bold"
          aria-hidden="true"
        >
          {SOURCE_INITIALS[stream.source]}
        </span>
        <span className="truncate">
          {indent && "↳ "}
          {title}
        </span>
        {isActive && (
          <span
            className="ml-1 shrink-0 rounded-sm bg-black/30 px-1 text-[9px] font-semibold tracking-wide"
            aria-hidden="true"
          >
            ● {t("timeline.liveTag")}
          </span>
        )}
        {markers.map((marker) => {
          const markerLeftPct = Math.min(
            Math.max(((marker.ts - clippedStart) / barSpanMs) * 100, 0),
            100,
          );
          return (
            <span
              key={marker.id}
              className="absolute bottom-0.5 size-1 rounded-full bg-white/90"
              style={{ left: `${markerLeftPct}%` }}
              aria-hidden="true"
            />
          );
        })}
      </button>
      {agentBadge && (
        <button
          type="button"
          onClick={agentBadge.onToggle}
          aria-expanded={agentBadge.expanded}
          aria-label={t("timeline.agentBadgeAriaLabel", {
            count: agentBadge.count,
            action: agentBadge.expanded ? t("common.collapse") : t("common.expand"),
          })}
          className="shrink-0 border-l border-white/30 bg-black/20 px-1.5 text-[10px] font-semibold text-white hover:bg-black/30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          🤖 {agentBadge.count}
        </button>
      )}
    </div>
  );
}

export function Timeline() {
  const t = useT();
  const currentDate = useAppStore((state) => state.currentDate);
  const selectedStreamId = useAppStore((state) => state.selectedStreamId);
  const setSelectedStreamId = useAppStore((state) => state.setSelectedStreamId);
  const setNavigableStreamIds = useAppStore((state) => state.setNavigableStreamIds);
  const setDayProjects = useAppStore((state) => state.setDayProjects);
  const viewFilters = useAppStore((state) => state.viewFilters);
  /** 펼쳐진 세션(부모) 스트림 id 집합 — 배지 클릭 시 하위 agent 레인을 보여준다. */
  const [expandedSessionIds, setExpandedSessionIds] = useState<Set<string>>(new Set());

  const { data, isLoading, isError } = useQuery({
    queryKey: queryKeys.eventsByDay(currentDate, LOCAL_TIMEZONE),
    queryFn: () => listEventsByDay(currentDate, LOCAL_TIMEZONE),
  });

  const { startMs, endMs } = useMemo(() => getDayBounds(currentDate), [currentDate]);

  /** 그날 존재하는 고유 프로젝트 표시 이름 목록을 등록한다(App.tsx 프로젝트 필터 select 옵션용,
   * 필터 미적용 원본 기준). */
  useEffect(() => {
    setDayProjects(uniqueProjectNames((data?.streams ?? []).map((stream) => stream.project)));
  }, [data?.streams, setDayProjects]);

  /** 소스/프로젝트 뷰 필터 적용 결과(레인 배치 이전). agent는 부모 세션 필터를 따른다. */
  const filteredStreams = useMemo(
    () => filterStreamsByView(data?.streams ?? [], viewFilters),
    [data?.streams, viewFilters],
  );

  const sessionStreams = useMemo(
    () => filteredStreams.filter((stream) => stream.kind !== "agent"),
    [filteredStreams],
  );
  const agentStreams = useMemo(
    () => filteredStreams.filter((stream) => stream.kind === "agent"),
    [filteredStreams],
  );
  const sessionIds = useMemo(() => new Set(sessionStreams.map((s) => s.id)), [sessionStreams]);
  const agentsByParent = useMemo(() => groupAgentsByParent(agentStreams), [agentStreams]);
  /** 부모가 그날 세션 목록에 없는 agent(엣지 케이스) — 하단 "독립 에이전트" 섹션으로 폴백. */
  const orphanAgents = useMemo(
    () =>
      agentStreams.filter((agent) => {
        const parentId = getParentStreamId(agent.metadata);
        return parentId === null || !sessionIds.has(parentId);
      }),
    [agentStreams, sessionIds],
  );

  /** 메시지형 스트림의 구간 산출용 — 그날 마커의 streamId별 min~max ts(위 computeMarkerRangeByStream 참고). */
  const markerRangeByStream = useMemo(
    () => computeMarkerRangeByStream(data?.events ?? []),
    [data?.events],
  );

  const { axisStart, axisEnd } = useMemo(
    () => computeAxisRange(data?.streams ?? [], startMs, endMs, markerRangeByStream),
    [data?.streams, startMs, endMs, markerRangeByStream],
  );
  const axisSpanMs = axisEnd - axisStart;
  const axisTicks = useMemo(
    () => computeAxisTicks(axisStart, axisEnd, startMs),
    [axisStart, axisEnd, startMs],
  );

  const sessionPlacements = useMemo(
    () => assignLanes(sessionStreams, startMs, endMs, markerRangeByStream),
    [sessionStreams, startMs, endMs, markerRangeByStream],
  );
  const orphanPlacements = useMemo(
    () => assignLanes(orphanAgents, startMs, endMs, markerRangeByStream),
    [orphanAgents, startMs, endMs, markerRangeByStream],
  );
  const markersByStream = useMemo(() => groupMarkersByStream(data?.events ?? []), [data?.events]);

  /** 레인 배치된 세션 순서를 키보드 j/k 이동용으로 등록한다(docs/05 "키보드 우선"). */
  useEffect(() => {
    setNavigableStreamIds(sessionPlacements.map((placement) => placement.stream.id));
  }, [sessionPlacements, setNavigableStreamIds]);

  const sessionLaneCount = useMemo(
    () => sessionPlacements.reduce((max, placement) => Math.max(max, placement.lane + 1), 0),
    [sessionPlacements],
  );

  /** 펼쳐진 세션의 하위 agent를 클리핑해 세션 id별로 미리 계산한다. */
  const expandedChildrenBySession = useMemo(() => {
    const map = new Map<string, ClippedStream[]>();
    for (const placement of sessionPlacements) {
      if (!expandedSessionIds.has(placement.stream.id)) continue;
      const children = agentsByParent.get(placement.stream.id) ?? [];
      if (children.length === 0) continue;
      map.set(placement.stream.id, clipToRange(children, startMs, endMs, markerRangeByStream));
    }
    return map;
  }, [sessionPlacements, expandedSessionIds, agentsByParent, startMs, endMs, markerRangeByStream]);

  /** 레인(row)별 세로 시작 위치 — 펼쳐진 세션이 있으면 그 아래 행만큼 다음 레인들이 밀린다. */
  const { rowTops, mainAreaHeight } = useMemo(() => {
    const tops = new Map<number, number>();
    let cursor = 0;
    for (let lane = 0; lane < sessionLaneCount; lane += 1) {
      tops.set(lane, cursor);
      let extra = 0;
      for (const placement of sessionPlacements) {
        if (placement.lane !== lane) continue;
        const children = expandedChildrenBySession.get(placement.stream.id);
        if (children) extra += children.length * LANE_HEIGHT_PX;
      }
      cursor += LANE_HEIGHT_PX + extra;
    }
    return { rowTops: tops, mainAreaHeight: cursor };
  }, [sessionLaneCount, sessionPlacements, expandedChildrenBySession]);

  const orphanLaneCount = useMemo(
    () => orphanPlacements.reduce((max, placement) => Math.max(max, placement.lane + 1), 0),
    [orphanPlacements],
  );
  const orphanSectionTop = mainAreaHeight + (orphanPlacements.length > 0 ? ORPHAN_HEADER_PX : 0);
  const totalHeight = orphanSectionTop + orphanLaneCount * LANE_HEIGHT_PX;

  const toggleSessionExpanded = (streamId: string) => {
    setExpandedSessionIds((prev) => {
      const next = new Set(prev);
      if (next.has(streamId)) {
        next.delete(streamId);
      } else {
        next.add(streamId);
      }
      return next;
    });
  };

  const isToday = currentDate === toLocalDateString(new Date());
  const now = Date.now();
  const nowLeftPct =
    isToday && now >= axisStart && now <= axisEnd ? toLeftPct(now, axisStart, axisSpanMs) : null;

  const hasContent = sessionPlacements.length > 0 || orphanPlacements.length > 0;

  return (
    <div className="flex h-full flex-col">
      {(isLoading || isError) && (
        <div className="flex items-center gap-2 border-b border-border px-4 py-1.5 text-xs">
          {isLoading && <span className="text-muted-foreground">{t("timeline.loading")}</span>}
          {isError && <span className="text-destructive">{t("timeline.loadError")}</span>}
        </div>
      )}

      <div className="relative h-6 shrink-0 border-b border-border text-[10px] text-muted-foreground">
        {axisTicks.map((tick) => (
          <span
            key={tick}
            className="absolute top-1 -translate-x-1/2"
            style={{ left: `${toLeftPct(tick, axisStart, axisSpanMs)}%` }}
          >
            {formatHourMinute(tick)}
          </span>
        ))}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        {!hasContent && !isLoading ? (
          <p className="py-8 text-center text-sm text-muted-foreground">{t("timeline.emptyDay")}</p>
        ) : (
          <div className="relative" style={{ height: totalHeight }}>
            {axisTicks.map((tick) => (
              <div
                key={tick}
                className="absolute inset-y-0 border-l border-border/40"
                style={{ left: `${toLeftPct(tick, axisStart, axisSpanMs)}%` }}
                aria-hidden="true"
              />
            ))}
            {nowLeftPct !== null && (
              <div
                className="absolute inset-y-0 z-10 w-px bg-red-500"
                style={{ left: `${nowLeftPct}%` }}
                aria-hidden="true"
              />
            )}

            {sessionPlacements.flatMap((placement) => {
              const top = rowTops.get(placement.lane) ?? 0;
              const children = agentsByParent.get(placement.stream.id) ?? [];
              const expanded = expandedSessionIds.has(placement.stream.id);
              const bars = [
                <TimelineBar
                  key={placement.stream.id}
                  stream={placement.stream}
                  top={top}
                  leftPct={toLeftPct(placement.clippedStart, axisStart, axisSpanMs)}
                  widthPct={toWidthPct(placement.clippedStart, placement.clippedEnd, axisSpanMs)}
                  markers={markersByStream.get(placement.stream.id) ?? []}
                  clippedStart={placement.clippedStart}
                  clippedEnd={placement.clippedEnd}
                  isSelected={selectedStreamId === placement.stream.id}
                  onSelect={() => setSelectedStreamId(placement.stream.id)}
                  agentBadge={
                    children.length > 0
                      ? {
                          count: children.length,
                          expanded,
                          onToggle: () => toggleSessionExpanded(placement.stream.id),
                        }
                      : undefined
                  }
                />,
              ];

              if (expanded) {
                const clippedChildren = expandedChildrenBySession.get(placement.stream.id) ?? [];
                clippedChildren.forEach((child, index) => {
                  bars.push(
                    <TimelineBar
                      key={child.stream.id}
                      stream={child.stream}
                      top={top + (index + 1) * LANE_HEIGHT_PX}
                      leftPct={toLeftPct(child.clippedStart, axisStart, axisSpanMs)}
                      widthPct={toWidthPct(child.clippedStart, child.clippedEnd, axisSpanMs)}
                      markers={markersByStream.get(child.stream.id) ?? []}
                      clippedStart={child.clippedStart}
                      clippedEnd={child.clippedEnd}
                      isSelected={selectedStreamId === child.stream.id}
                      onSelect={() => setSelectedStreamId(child.stream.id)}
                      indent
                      parentTitle={placement.stream.title ?? placement.stream.id}
                    />,
                  );
                });
              }

              return bars;
            })}

            {orphanPlacements.length > 0 && (
              <h3
                className="absolute text-[10px] font-semibold text-muted-foreground"
                style={{ top: mainAreaHeight, left: 0 }}
              >
                {t("timeline.orphanAgentsHeading")}
              </h3>
            )}
            {orphanPlacements.map((placement) => (
              <TimelineBar
                key={placement.stream.id}
                stream={placement.stream}
                top={orphanSectionTop + placement.lane * LANE_HEIGHT_PX}
                leftPct={toLeftPct(placement.clippedStart, axisStart, axisSpanMs)}
                widthPct={toWidthPct(placement.clippedStart, placement.clippedEnd, axisSpanMs)}
                markers={markersByStream.get(placement.stream.id) ?? []}
                clippedStart={placement.clippedStart}
                clippedEnd={placement.clippedEnd}
                isSelected={selectedStreamId === placement.stream.id}
                onSelect={() => setSelectedStreamId(placement.stream.id)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
