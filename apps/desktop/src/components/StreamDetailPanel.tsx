import type { EventType } from "@logroom/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ArrowDownUp, ExternalLink, Pencil, X } from "lucide-react";
import { Fragment, type KeyboardEvent, useEffect, useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import type { Translate, UiKey } from "@/i18n";
import { useT } from "@/i18n";
import type { GetStreamRange } from "@/lib/api";
import {
  getStream,
  QUERY_KEY_DIGEST,
  QUERY_KEY_EVENTS_BY_DAY,
  QUERY_KEY_SEARCH,
  queryKeys,
  renameStream,
} from "@/lib/api";
import type { EventFull } from "@/lib/types";
import { getParentStreamId, isHubStream } from "@/lib/utils";
import { useAppStore } from "@/store";

const MINUTE_MS = 60 * 1000;
const DAY_MS = 24 * 60 * 60 * 1000;

/**
 * 메시지형(발화 기록) 스트림 id 접두어 — Slack(`capture/slack.rs::stream_id`의 `"slack:<channel_id>"`)
 * + GitHub(`capture/github.rs::stream_id`의 `"github:<owner/repo>"`) + Linear
 * (`capture/linear.rs::stream_id`의 `"linear:<team key>"`) 규칙과 대응한다. 이벤트 데이터를
 * 조회하기 전에도(스트림 id만으로) 판정할 수 있어, 메시지형 스트림은 쿼리 시점부터 "그날" 범위로
 * 좁힐 수 있다(대화형과 달리 채팅/커밋 등 발화 기록은 누적하지 않고 1일 단위로 본다).
 */
const MESSAGE_SOURCE_PREFIXES = ["slack:", "github:", "linear:"] as const;

function isMessageStreamId(streamId: string): boolean {
  return MESSAGE_SOURCE_PREFIXES.some((prefix) => streamId.startsWith(prefix));
}

/** 로컬 날짜(YYYY-MM-DD) 자정~다음 자정의 [fromTs, toTs) 구간(Timeline.tsx::getDayBounds /
 * SearchDialog.tsx::localMidnightMs와 동일한 OS 로컬 타임존 기준 계산). */
function localDayRange(localDate: string): GetStreamRange {
  const [year, month, day] = localDate.split("-").map(Number);
  const fromTs = new Date(year ?? 1970, (month ?? 1) - 1, day ?? 1, 0, 0, 0, 0).getTime();
  return { fromTs, toTs: fromTs + DAY_MS };
}

/** 도구 활동 그룹 점진 렌더: 최초 노출 개수 + "더 보기" 클릭당 추가 개수. */
const TOOL_GROUP_INITIAL_VISIBLE = 50;
const TOOL_GROUP_LOAD_MORE_STEP = 100;

/** 연속된 tool_use/tool_result는 "도구 활동 그룹"으로 묶어 접는다. */
const TOOL_EVENT_TYPES = new Set<EventType>(["tool_use", "tool_result"]);

type RenderItem =
  | { kind: "event"; event: EventFull }
  | { kind: "toolGroup"; key: string; events: EventFull[] };

/** prompt 1건 = 턴 1개. prompt 이전 이벤트는 prompt: null인 "요청 전 활동" 턴으로 묶인다. */
interface Turn {
  key: string; // 턴의 첫 이벤트 id (펼침 state 키)
  prompt: EventFull | null;
  children: EventFull[];
}

/** EventType → i18n 키. `EVENT_TYPE_LABELS`(Record<EventType, string>) 대신 키만 담아 `t()`로
 * 해석한다 — 값 자체를 로케일에 따라 바꾸는 대신 키를 고정해 타입 안전하게 매핑한다. */
const EVENT_TYPE_KEYS: Record<EventType, UiKey> = {
  prompt: "stream.eventType.prompt",
  response: "stream.eventType.response",
  tool_use: "stream.eventType.tool_use",
  tool_result: "stream.eventType.tool_result",
  message: "stream.eventType.message",
  file_edit: "stream.eventType.file_edit",
  note: "stream.eventType.note",
};

function eventTypeLabel(type: EventType, t: Translate): string {
  return t(EVENT_TYPE_KEYS[type]);
}

export function formatTime(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function formatDuration(startedAt: number, endedAt: number | null, t: Translate): string {
  const end = endedAt ?? Date.now();
  const minutes = Math.max(Math.round((end - startedAt) / MINUTE_MS), 0);
  if (minutes < 60) return t("stream.duration.minutes", { n: minutes });
  const hours = Math.floor(minutes / 60);
  const remaining = minutes % 60;
  return remaining === 0
    ? t("stream.duration.hours", { h: hours })
    : t("stream.duration.hoursMinutes", { h: hours, m: remaining });
}

/** 이벤트(ts ASC)를 연속된 tool_use/tool_result run 단위로 그룹핑한다. */
function groupToolActivity(events: EventFull[]): RenderItem[] {
  const items: RenderItem[] = [];
  let buffer: EventFull[] = [];

  const flushBuffer = () => {
    const first = buffer[0];
    if (!first) return;
    items.push({ kind: "toolGroup", key: first.id, events: buffer });
    buffer = [];
  };

  for (const event of events) {
    if (TOOL_EVENT_TYPES.has(event.type)) {
      buffer.push(event);
    } else {
      flushBuffer();
      items.push({ kind: "event", event });
    }
  }
  flushBuffer();

  return items;
}

/** 이벤트(ts ASC)를 prompt 기준 턴으로 분할한다. 첫 prompt 이전 이벤트는 별도 턴으로 묶인다. */
function buildTurns(events: EventFull[]): Turn[] {
  const turns: Turn[] = [];
  const preTurnEvents: EventFull[] = [];
  let current: Turn | null = null;

  for (const event of events) {
    if (event.type === "prompt") {
      current = { key: event.id, prompt: event, children: [] };
      turns.push(current);
      continue;
    }
    if (current) {
      current.children.push(event);
    } else {
      preTurnEvents.push(event);
    }
  }

  const firstPreTurnEvent = preTurnEvents[0];
  if (firstPreTurnEvent) {
    turns.unshift({ key: firstPreTurnEvent.id, prompt: null, children: preTurnEvents });
  }

  return turns;
}

/** 그룹 내 tool_use title 고유값 앞 3개를 미리보기로 만든다. */
function toolGroupPreview(events: EventFull[]): string | null {
  const titles: string[] = [];
  for (const event of events) {
    if (event.type !== "tool_use" || !event.title) continue;
    if (!titles.includes(event.title)) titles.push(event.title);
  }
  if (titles.length === 0) return null;
  const preview = titles.slice(0, 3).join(", ");
  return titles.length > 3 ? `${preview} …` : preview;
}

function EventRow({ event }: { event: EventFull }) {
  const t = useT();
  return (
    <li className="rounded-md border border-border p-3">
      <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
        <span className="font-medium text-foreground">{eventTypeLabel(event.type, t)}</span>
        <span className="tabular-nums">{formatTime(event.ts)}</span>
      </div>
      {event.title && <p className="mt-1 text-sm font-medium">{event.title}</p>}
      {event.body && (
        <p className="mt-1 whitespace-pre-wrap text-sm text-muted-foreground">{event.body}</p>
      )}
      {(event.model || event.tokensIn != null || event.tokensOut != null) && (
        <div className="mt-2 flex flex-wrap gap-2 text-[11px] text-muted-foreground">
          {event.model && <span>{t("stream.event.model", { model: event.model })}</span>}
          {event.tokensIn != null && <span>in: {event.tokensIn}</span>}
          {event.tokensOut != null && <span>out: {event.tokensOut}</span>}
        </div>
      )}
    </li>
  );
}

function ToolActivityGroup({
  groupKey,
  events,
  expanded,
  onToggle,
}: {
  groupKey: string;
  events: EventFull[];
  expanded: boolean;
  onToggle: () => void;
}) {
  const t = useT();
  const preview = toolGroupPreview(events);
  const contentId = `tool-activity-${groupKey}`;
  // 그룹이 크면(수백 건) 전체를 한 번에 렌더하지 않고 점진 렌더한다 — 접히면 다음 펼침 때 초기값으로 리셋.
  const [visibleCount, setVisibleCount] = useState(TOOL_GROUP_INITIAL_VISIBLE);

  useEffect(() => {
    if (!expanded) setVisibleCount(TOOL_GROUP_INITIAL_VISIBLE);
  }, [expanded]);

  const visibleEvents = events.slice(0, visibleCount);
  const remainingCount = events.length - visibleEvents.length;

  return (
    <li className="rounded-md border border-border">
      <button
        type="button"
        aria-expanded={expanded}
        aria-controls={contentId}
        onClick={onToggle}
        className="flex w-full items-center justify-between gap-2 p-3 text-left text-xs text-muted-foreground hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <span className="truncate">
          {t("stream.toolGroup.summary", { n: events.length })}
          {preview && <span className="ml-2 text-foreground">{preview}</span>}
        </span>
        <span className="shrink-0">{expanded ? t("common.collapse") : t("common.expand")}</span>
      </button>
      <ul id={contentId} hidden={!expanded} className="space-y-2 border-t border-border p-3">
        {expanded && (
          <>
            {visibleEvents.map((event) => (
              <EventRow key={event.id} event={event} />
            ))}
            {remainingCount > 0 && (
              <li>
                <Button
                  type="button"
                  variant="outline"
                  size="xs"
                  aria-label={t("stream.toolGroup.loadMoreAriaLabel", { n: remainingCount })}
                  onClick={() => setVisibleCount((prev) => prev + TOOL_GROUP_LOAD_MORE_STEP)}
                >
                  {t("stream.toolGroup.loadMore", { n: remainingCount })}
                </Button>
              </li>
            )}
          </>
        )}
      </ul>
    </li>
  );
}

/** 턴 헤더에 표시할 텍스트. prompt가 있으면 title(없으면 body), 없으면 "요청 전 활동" 라벨. */
function turnHeaderText(turn: Turn, t: Translate): string {
  const preTurnLabel = t("stream.preTurnLabel");
  if (!turn.prompt) return preTurnLabel;
  return turn.prompt.title || turn.prompt.body || preTurnLabel;
}

/** 턴 1개(요청 + 그 응답/도구 활동)를 접힌 목차 행으로 렌더링한다. */
function TurnItem({
  turn,
  expanded,
  onToggle,
  expandedGroups,
  onToggleGroup,
}: {
  turn: Turn;
  expanded: boolean;
  onToggle: () => void;
  expandedGroups: Set<string>;
  onToggleGroup: (key: string) => void;
}) {
  const t = useT();
  const items = useMemo(() => groupToolActivity(turn.children), [turn.children]);
  const contentId = `turn-${turn.key}`;
  const headerTs = turn.prompt?.ts ?? turn.children[0]?.ts ?? 0;
  const responseCount = turn.children.filter((event) => event.type === "response").length;
  const toolCount = turn.children.filter((event) => TOOL_EVENT_TYPES.has(event.type)).length;

  return (
    <li className="rounded-md border border-border">
      <button
        type="button"
        aria-expanded={expanded}
        aria-controls={contentId}
        onClick={onToggle}
        className="flex w-full items-start justify-between gap-3 p-3 text-left hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <div className="min-w-0 flex-1">
          <span className="block text-xs tabular-nums text-muted-foreground">
            {formatTime(headerTs)}
          </span>
          <p className="mt-1 line-clamp-2 text-sm font-medium">{turnHeaderText(turn, t)}</p>
        </div>
        <div className="flex shrink-0 flex-col items-end gap-1 text-xs text-muted-foreground">
          <span>{t("stream.turn.counts", { responses: responseCount, tools: toolCount })}</span>
          <span>{expanded ? t("common.collapse") : t("common.expand")}</span>
        </div>
      </button>
      <div id={contentId} hidden={!expanded} className="border-t border-border p-3">
        {expanded && (
          <ul className="space-y-2 border-l border-border pl-4">
            {items.map((item) =>
              item.kind === "event" ? (
                <EventRow key={item.event.id} event={item.event} />
              ) : (
                <ToolActivityGroup
                  key={item.key}
                  groupKey={item.key}
                  events={item.events}
                  expanded={expandedGroups.has(item.key)}
                  onToggle={() => onToggleGroup(item.key)}
                />
              ),
            )}
          </ul>
        )}
      </div>
    </li>
  );
}

/** 턴의 대표 시각 — 날짜 디바이더 그룹핑용. prompt가 있으면 그 ts, 없으면(요청 전 활동 턴) 첫
 * 자식 이벤트 ts. 둘 다 없으면 0(사실상 없음). */
function turnRepresentativeTs(turn: Turn): number {
  return turn.prompt?.ts ?? turn.children[0]?.ts ?? 0;
}

/** 턴 분할 결과 + 펼침 state를 소유. `key={streamId}`로 마운트되어 스트림 변경 시 자연스레 리셋된다. */
function StreamEventList({ events, sortOrder }: { events: EventFull[]; sortOrder: SortOrder }) {
  const turns = useMemo(() => {
    const built = buildTurns(events); // ts ASC(턴 순서·턴 내부 모두 오름차순)
    // desc면 턴 순서만 뒤집는다 — 턴 내부(요청→응답→도구)는 항상 시간순이 자연스러우므로 유지.
    return sortOrder === "desc" ? [...built].reverse() : built;
  }, [events, sortOrder]);
  const [expandedTurns, setExpandedTurns] = useState<Set<string>>(new Set());
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());

  const toggleTurn = (key: string) => {
    setExpandedTurns((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };

  const toggleGroup = (key: string) => {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };

  let lastDate = "";
  return (
    <ul className="min-h-0 flex-1 space-y-2 overflow-y-auto p-4">
      {turns.map((turn) => {
        const date = toLocalDateLabel(turnRepresentativeTs(turn));
        const showDivider = date !== lastDate;
        lastDate = date;
        // TurnItem이 자체 <li>라 디바이더는 별도 <li>로 감싼다(li 중첩 방지). 디바이더 li는
        // 표시용이라 스크린리더 목록 항목 수에서 제외(presentation 아님 — 날짜 자체가 정보라 유지).
        return (
          <Fragment key={turn.key}>
            {showDivider && (
              <li>
                <DateDivider date={date} />
              </li>
            )}
            <TurnItem
              turn={turn}
              expanded={expandedTurns.has(turn.key)}
              onToggle={() => toggleTurn(turn.key)}
              expandedGroups={expandedGroups}
              onToggleGroup={toggleGroup}
            />
          </Fragment>
        );
      })}
    </ul>
  );
}

/**
 * 원격(제3자 제어) URL 안전 검증 — Slack permalink/GitHub 커밋·PR·이슈 링크/Linear 이슈·코멘트
 * 링크만 바로가기로 노출한다. https 스킴 + 호스트 화이트리스트(*.slack.com, github.com, linear.app)
 * 검증(보안 리뷰 권고: API 응답 데이터를 링크로 렌더할 땐 스킴/도메인 검증 필수). 기존 Slack 전용
 * `safeSlackPermalink`를 GitHub/Linear 커넥터 추가에 맞춰 일반화했다.
 */
const EXTERNAL_LINK_ALLOWED_HOSTS = ["slack.com", "github.com", "linear.app"] as const;

export function safeExternalLinkUrl(url: string | null): string | null {
  if (!url) return null;
  try {
    const u = new URL(url);
    if (u.protocol !== "https:") return null;
    const isAllowed = EXTERNAL_LINK_ALLOWED_HOSTS.some(
      (host) => u.hostname === host || u.hostname.endsWith(`.${host}`),
    );
    if (!isAllowed) return null;
    return u.toString();
  } catch {
    return null;
  }
}

/** 메시지형 스트림(커넥터)의 이벤트 1건 — 시간 + 본문 + 바로가기(Slack permalink/GitHub 링크). */
function MessageEventRow({ event }: { event: EventFull }) {
  const t = useT();
  const link = safeExternalLinkUrl(event.url);
  return (
    <div className="group flex items-baseline gap-2 px-1 py-1 text-sm">
      <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
        {formatTime(event.ts)}
      </span>
      <span className="min-w-0 flex-1 whitespace-pre-wrap">{event.body ?? event.title ?? ""}</span>
      {link && (
        <button
          type="button"
          aria-label={t("stream.openExternalLink")}
          title={t("stream.openExternalLink")}
          onClick={() => void openUrl(link)}
          className="shrink-0 rounded-sm p-0.5 text-muted-foreground opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring group-hover:opacity-100"
        >
          <ExternalLink className="size-3.5" aria-hidden="true" />
        </button>
      )}
    </div>
  );
}

/** epoch ms → 로컬 날짜 문자열(YYYY-MM-DD) — 날짜 디바이더 그룹핑용. */
function toLocalDateLabel(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** 이벤트 시간 정렬 방향 — "asc"(오래된→최신, 대화 흐름) / "desc"(최신→오래된, "오늘 뭐 했지" 스캔용). */
type SortOrder = "asc" | "desc";

/** localStorage 키 — 상세 패널 정렬 방향(테마 등과 동일한 persist 패턴). 기본 "asc". */
const DETAIL_SORT_STORAGE_KEY = "logroom-detail-sort";

function getInitialSortOrder(): SortOrder {
  return window.localStorage.getItem(DETAIL_SORT_STORAGE_KEY) === "desc" ? "desc" : "asc";
}

/** 날짜 구분선(가로선 + YYYY-MM-DD) — 세션형/메시지형 상세 리스트 공용. 긴 세션이 여러 날에
 * 걸칠 때 날짜가 바뀌는 지점을 구분한다(사용자 요구: "세션이 길어져 겹쳐 보기 어렵다"). */
function DateDivider({ date }: { date: string }) {
  return (
    <div className="flex items-center gap-2 pt-3 pb-1 first:pt-0">
      <span className="h-px flex-1 bg-border" aria-hidden="true" />
      <span className="shrink-0 font-mono text-[11px] tabular-nums text-muted-foreground">
        {date}
      </span>
      <span className="h-px flex-1 bg-border" aria-hidden="true" />
    </div>
  );
}

/**
 * 메시지형 스트림(id가 MESSAGE_SOURCE_PREFIXES로 시작하는 커넥터 — Slack/GitHub/Linear) 전용 렌더러.
 * 턴 트리/도구 그룹핑/"(요청 전 활동)" 라벨 없이 이벤트를 시간순 그대로 나열한다.
 * 채널 스트림은 수년에 걸친 메시지가 아니라 "그날"(currentDate) 범위만 조회해오므로, 날짜
 * 디바이더는 하루 안이라 사실상 최대 1개만 보이지만 자정 근처 표시 안전망으로 유지한다.
 * 그날 메시지가 없으면(events 빈 배열) 빈 상태 문구를 보여준다.
 */
function MessageStreamEventList({
  events,
  sortOrder,
}: {
  events: EventFull[];
  sortOrder: SortOrder;
}) {
  const t = useT();
  // events는 ts ASC로 들어온다 — desc면 역순 렌더(원본 불변 유지 위해 복사). 날짜 디바이더는
  // 정렬된 순서 기준으로 다시 계산된다.
  const ordered = useMemo(
    () => (sortOrder === "desc" ? [...events].reverse() : events),
    [events, sortOrder],
  );

  if (ordered.length === 0) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center p-6 text-center text-sm text-muted-foreground">
        {t("stream.noMessagesThisDay")}
      </div>
    );
  }

  let lastDate = "";
  return (
    <ul className="min-h-0 flex-1 space-y-0.5 overflow-y-auto p-4">
      {ordered.map((event) => {
        const date = toLocalDateLabel(event.ts);
        const showDivider = date !== lastDate;
        lastDate = date;
        return (
          <li key={event.id}>
            {showDivider && <DateDivider date={date} />}
            <MessageEventRow event={event} />
          </li>
        );
      })}
    </ul>
  );
}

/**
 * 스트림 제목 표시 + 수동 편집(연필 아이콘 → input 전환). `key={streamId}`로 마운트되어
 * 스트림 변경 시 편집 state가 자연스레 리셋된다(StreamEventList와 동일 패턴).
 * Enter 저장(rename_stream → stream/day/digest/search 쿼리 invalidate) / Esc 취소.
 */
function StreamTitleEditor({ streamId, title }: { streamId: string; title: string | null }) {
  const t = useT();
  const queryClient = useQueryClient();
  const [isEditing, setIsEditing] = useState(false);
  const [draft, setDraft] = useState(title ?? "");

  const renameMutation = useMutation({
    mutationFn: (nextTitle: string) => renameStream(streamId, nextTitle),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.stream(streamId) });
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_EVENTS_BY_DAY] });
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_DIGEST] });
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_SEARCH] });
      setIsEditing(false);
    },
  });

  function handleStartEditing() {
    setDraft(title ?? "");
    renameMutation.reset();
    setIsEditing(true);
  }

  function handleCancel() {
    setDraft(title ?? "");
    renameMutation.reset();
    setIsEditing(false);
  }

  function handleSubmit() {
    const trimmed = draft.trim();
    if (!trimmed || renameMutation.isPending) return;
    renameMutation.mutate(trimmed);
  }

  function handleKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Enter") {
      event.preventDefault();
      handleSubmit();
    } else if (event.key === "Escape") {
      event.preventDefault();
      handleCancel();
    }
  }

  if (isEditing) {
    return (
      <div>
        <input
          // biome-ignore lint/a11y/noAutofocus: 연필 아이콘 클릭(명시적 사용자 액션)으로 여는 인라인 편집이라 즉시 타이핑 가능해야 함.
          autoFocus
          value={draft}
          disabled={renameMutation.isPending}
          aria-label={t("stream.titleInputAriaLabel")}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={handleKeyDown}
          className="w-full truncate rounded-md border border-border bg-background px-1.5 py-0.5 text-sm font-semibold outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
        />
        {renameMutation.isError && (
          <p className="mt-1 text-xs text-destructive">{t("stream.renameError")}</p>
        )}
      </div>
    );
  }

  return (
    <div className="flex items-center gap-1">
      <h2 className="truncate text-sm font-semibold">{title ?? streamId}</h2>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        aria-label={t("stream.editTitleAriaLabel")}
        onClick={handleStartEditing}
      >
        <Pencil className="size-3" />
      </Button>
    </div>
  );
}

export function StreamDetailPanel() {
  const t = useT();
  const selectedStreamId = useAppStore((state) => state.selectedStreamId);
  const setSelectedStreamId = useAppStore((state) => state.setSelectedStreamId);
  const currentDate = useAppStore((state) => state.currentDate);

  // 메시지형(Slack 등) 스트림은 채팅을 누적 표시하지 않고 currentDate의 "그날"만 조회한다
  // (판정은 이벤트 데이터 없이도 가능한 id prefix 기준 — isMessageStreamId 참고). 대화형은 range
  // 없이 현행대로 스트림 전체를 조회하며, currentDate가 바뀌어도 쿼리에 영향을 주지 않는다.
  const isMessageStream = selectedStreamId !== null && isMessageStreamId(selectedStreamId);
  const range = isMessageStream ? localDayRange(currentDate) : undefined;

  // 이벤트 시간 정렬 방향(사용자 요구 — "오늘 뭐 했지"는 최신순이 편함). localStorage persist.
  const [sortOrder, setSortOrder] = useState<SortOrder>(getInitialSortOrder);
  const toggleSortOrder = () => {
    setSortOrder((prev) => {
      const next: SortOrder = prev === "asc" ? "desc" : "asc";
      window.localStorage.setItem(DETAIL_SORT_STORAGE_KEY, next);
      return next;
    });
  };

  const { data, isLoading, isError } = useQuery({
    queryKey: queryKeys.stream(selectedStreamId ?? "", range),
    queryFn: () => getStream(selectedStreamId ?? "", range),
    enabled: selectedStreamId !== null,
  });

  const isAgent = data?.stream.kind === "agent";
  const parentStreamId = data ? getParentStreamId(data.stream.metadata) : null;

  /** agent 스트림일 때만 부모 세션 title 조회(소속 표시용). hooks 순서 유지를 위해 early return 이전에 호출. */
  const { data: parentData, isPending: isParentPending } = useQuery({
    queryKey: queryKeys.stream(parentStreamId ?? ""),
    queryFn: () => getStream(parentStreamId ?? ""),
    enabled: isAgent && parentStreamId !== null,
  });

  if (selectedStreamId === null) {
    return (
      <div className="flex h-full items-center justify-center p-6 text-center text-sm text-muted-foreground">
        {t("stream.selectPrompt")}
      </div>
    );
  }

  if (isLoading) {
    return <div className="p-6 text-sm text-muted-foreground">{t("stream.loading")}</div>;
  }

  if (isError || !data) {
    return <div className="p-6 text-sm text-destructive">{t("stream.notFound")}</div>;
  }

  const { stream } = data;

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-start justify-between gap-2 border-b border-border p-4">
        <div className="min-w-0">
          <StreamTitleEditor key={stream.id} streamId={stream.id} title={stream.title} />
          <p className="mt-1 text-xs text-muted-foreground">
            {stream.source} · {stream.project ?? t("common.noProject")}
            {stream.gitBranch && ` · ${stream.gitBranch}`}
            {isHubStream(stream.metadata) && (
              <span className="ml-1.5 inline-block rounded-full border border-border px-1.5 py-0.5 align-middle text-[10px] font-medium text-muted-foreground">
                {t("common.viaAgent")}
              </span>
            )}
          </p>
          {isAgent && parentStreamId && (
            <Button
              type="button"
              variant="ghost"
              size="xs"
              className="mt-1 h-auto px-1.5 py-0.5 text-xs text-muted-foreground"
              disabled={isParentPending}
              aria-label={
                isParentPending
                  ? t("stream.parentLoading")
                  : t("stream.parentNavigateAriaLabel", {
                      title: parentData?.stream.title ?? parentData?.stream.id ?? parentStreamId,
                    })
              }
              onClick={() => setSelectedStreamId(parentStreamId)}
            >
              {isParentPending
                ? t("stream.parentLoading")
                : t("stream.parentButtonLabel", {
                    title: parentData?.stream.title ?? parentData?.stream.id ?? parentStreamId,
                  })}
            </Button>
          )}
          <p className="mt-1 text-xs text-muted-foreground">
            {formatTime(stream.startedAt)} ~{" "}
            {stream.endedAt ? formatTime(stream.endedAt) : t("stream.inProgress")} (
            {formatDuration(stream.startedAt, stream.endedAt, t)})
          </p>
          {isMessageStream && (
            <p className="mt-1 text-xs font-medium text-muted-foreground">
              {t("stream.messagesOnDate", { date: currentDate })}
            </p>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={
              sortOrder === "asc" ? t("stream.sort.toNewestFirst") : t("stream.sort.toOldestFirst")
            }
            title={
              sortOrder === "asc" ? t("stream.sort.oldestFirst") : t("stream.sort.newestFirst")
            }
            onClick={toggleSortOrder}
          >
            <ArrowDownUp className="size-4" />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={t("common.closeDetail")}
            onClick={() => setSelectedStreamId(null)}
          >
            <X className="size-4" />
          </Button>
        </div>
      </header>
      {isMessageStream ? (
        <MessageStreamEventList key={stream.id} events={data.events} sortOrder={sortOrder} />
      ) : (
        <StreamEventList key={stream.id} events={data.events} sortOrder={sortOrder} />
      )}
    </div>
  );
}
