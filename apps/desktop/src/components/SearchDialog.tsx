import type { EventType, Source } from "@logroom/core";
import { useQuery } from "@tanstack/react-query";
import { Search } from "lucide-react";
import { Dialog } from "radix-ui";
import { type ReactNode, useDeferredValue, useEffect, useRef, useState } from "react";
import type { UiKey } from "@/i18n";
import { useT } from "@/i18n";
import { queryKeys, searchEvents } from "@/lib/api";
import type { SearchHit } from "@/lib/types";
import { cn } from "@/lib/utils";
import { shiftLocalDate, toLocalDateString, useAppStore } from "@/store";

/** FTS5 snippet()에서 매칭 구간을 감싸는 마커(Rust 쪽 snippet 호출과 합의된 문자). */
const SNIPPET_HIGHLIGHT_PATTERN = /\[([^\]]*)\]/g;
const MIN_QUERY_LENGTH = 2;

/** 검색 필터: 소스(전체/Claude/Kiro) — search_events sources 파라미터에 매핑(docs/05 ② "필터"). */
type SourceFilter = "all" | Extract<Source, "claude_code" | "kiro_cli">;
/** 검색 필터: 타입(전체/요청/응답) — search_events types 파라미터에 매핑. */
type TypeFilter = "all" | Extract<EventType, "prompt" | "response">;
/** 검색 필터: 기간(전체/오늘/7일/30일) — 로컬 자정 기준 fromTs로 환산(computePeriodFromTs). */
type PeriodFilter = "all" | "today" | "7d" | "30d";

/** 필터 칩 옵션 — `labelKey`(번역)와 `label`(브랜드명 등 고정 문자열) 중 하나만 준다
 * (FilterChipGroup이 `labelKey`를 우선 사용). */
interface FilterChipOption<T extends string> {
  value: T;
  labelKey?: UiKey;
  label?: string;
}

/** 소스 필터 칩 — "전체"만 번역하고 브랜드명(Claude/Kiro)은 로케일과 무관하게 고정한다. */
const SOURCE_FILTER_OPTIONS: Array<FilterChipOption<SourceFilter>> = [
  { value: "all", labelKey: "common.all" },
  { value: "claude_code", label: "Claude" },
  { value: "kiro_cli", label: "Kiro" },
];
const TYPE_FILTER_OPTIONS: Array<FilterChipOption<TypeFilter>> = [
  { value: "all", labelKey: "common.all" },
  { value: "prompt", labelKey: "search.filter.type.request" },
  { value: "response", labelKey: "search.filter.type.response" },
];
const PERIOD_FILTER_OPTIONS: Array<FilterChipOption<PeriodFilter>> = [
  { value: "all", labelKey: "common.all" },
  { value: "today", labelKey: "search.filter.period.today" },
  { value: "7d", labelKey: "search.filter.period.sevenDays" },
  { value: "30d", labelKey: "search.filter.period.thirtyDays" },
];

/** "today"/"7d"/"30d" 선택 시 거슬러 올라갈 일수(오늘 포함) — 0이면 오늘 자정부터. */
const PERIOD_LOOKBACK_DAYS: Record<Exclude<PeriodFilter, "all">, number> = {
  today: 0,
  "7d": 6,
  "30d": 29,
};

/** 로컬 날짜(YYYY-MM-DD) 자정의 epoch ms. */
function localMidnightMs(localDate: string): number {
  const [year, month, day] = localDate.split("-").map(Number);
  return new Date(year ?? 1970, (month ?? 1) - 1, day ?? 1, 0, 0, 0, 0).getTime();
}

/** 기간 필터 → search_events `fromTs`(로컬 자정 기준). "전체"는 필터 없음(undefined). */
function computePeriodFromTs(period: PeriodFilter): number | undefined {
  if (period === "all") return undefined;
  const todayLocalDate = toLocalDateString(new Date());
  const startLocalDate = shiftLocalDate(todayLocalDate, -PERIOD_LOOKBACK_DAYS[period]);
  return localMidnightMs(startLocalDate);
}

/** 검색 필터 행 칩 그룹 1개(예: 소스/타입/기간) — 단일 선택, 활성 칩은 aria-pressed로 강조. */
function FilterChipGroup<T extends string>({
  legendKey,
  value,
  options,
  onChange,
}: {
  legendKey: UiKey;
  value: T;
  options: Array<FilterChipOption<T>>;
  onChange: (value: T) => void;
}) {
  const t = useT();
  const legend = t(legendKey);
  return (
    <fieldset className="flex items-center gap-1">
      <legend className="sr-only">{legend}</legend>
      <span className="mr-0.5 text-[11px] text-muted-foreground" aria-hidden="true">
        {legend}
      </span>
      {options.map((option) => {
        const isActive = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            aria-pressed={isActive}
            onClick={() => onChange(option.value)}
            className={cn(
              "rounded-full border px-2 py-0.5 text-[11px] transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
              isActive
                ? "border-primary bg-primary/20 text-foreground"
                : "border-border text-muted-foreground hover:bg-accent hover:text-accent-foreground",
            )}
          >
            {option.labelKey ? t(option.labelKey) : option.label}
          </button>
        );
      })}
    </fieldset>
  );
}

function renderSnippet(snippet: string): ReactNode[] {
  const parts: ReactNode[] = [];
  let lastIndex = 0;
  let key = 0;
  SNIPPET_HIGHLIGHT_PATTERN.lastIndex = 0;
  let match = SNIPPET_HIGHLIGHT_PATTERN.exec(snippet);
  while (match !== null) {
    if (match.index > lastIndex) {
      parts.push(<span key={key++}>{snippet.slice(lastIndex, match.index)}</span>);
    }
    parts.push(
      <mark key={key++} className="rounded bg-primary/50 px-0.5 text-foreground">
        {match[1]}
      </mark>,
    );
    lastIndex = match.index + match[0].length;
    match = SNIPPET_HIGHLIGHT_PATTERN.exec(snippet);
  }
  if (lastIndex < snippet.length) {
    parts.push(<span key={key++}>{snippet.slice(lastIndex)}</span>);
  }
  return parts;
}

function SearchResultItem({
  hit,
  onSelect,
}: {
  hit: SearchHit;
  onSelect: (hit: SearchHit) => void;
}) {
  return (
    <li>
      <button
        type="button"
        onClick={() => onSelect(hit)}
        className="w-full rounded-md px-3 py-2 text-left text-sm hover:bg-accent hover:text-accent-foreground focus-visible:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      >
        <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
          <span className="truncate">{hit.streamTitle ?? hit.streamId}</span>
          <span className="tabular-nums">{new Date(hit.ts).toLocaleString()}</span>
        </div>
        <p className="mt-1 line-clamp-2 text-sm">{renderSnippet(hit.snippet)}</p>
      </button>
    </li>
  );
}

export function SearchDialog() {
  const t = useT();
  const searchOpen = useAppStore((state) => state.searchOpen);
  const setSearchOpen = useAppStore((state) => state.setSearchOpen);
  const setSelectedStreamId = useAppStore((state) => state.setSelectedStreamId);
  const [query, setQuery] = useState("");
  const [sourceFilter, setSourceFilter] = useState<SourceFilter>("all");
  const [typeFilter, setTypeFilter] = useState<TypeFilter>("all");
  const [periodFilter, setPeriodFilter] = useState<PeriodFilter>("all");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.metaKey && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setSearchOpen(!searchOpen);
      }
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [searchOpen, setSearchOpen]);

  const trimmedQuery = query.trim();
  const deferredQuery = useDeferredValue(trimmedQuery);
  const searchParams = {
    sources: sourceFilter === "all" ? undefined : [sourceFilter],
    types: typeFilter === "all" ? undefined : [typeFilter],
    fromTs: computePeriodFromTs(periodFilter),
  };
  const { data: hits, isLoading } = useQuery({
    queryKey: queryKeys.search(deferredQuery, searchParams),
    queryFn: () => searchEvents(deferredQuery, searchParams),
    enabled: deferredQuery.length >= MIN_QUERY_LENGTH,
  });

  function handleSelect(hit: SearchHit) {
    setSelectedStreamId(hit.streamId);
    setSearchOpen(false);
    setQuery("");
  }

  return (
    <Dialog.Root
      open={searchOpen}
      onOpenChange={(open) => {
        setSearchOpen(open);
        if (!open) setQuery("");
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-background/80 backdrop-blur-sm" />
        <Dialog.Content
          className="fixed top-24 left-1/2 z-50 w-full max-w-xl -translate-x-1/2 overflow-hidden rounded-lg border border-border bg-popover text-popover-foreground shadow-lg"
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            inputRef.current?.focus();
          }}
        >
          <Dialog.Title className="sr-only">{t("search.dialogTitle")}</Dialog.Title>
          <Dialog.Description className="sr-only">
            {t("search.dialogDescription")}
          </Dialog.Description>
          <div className="flex items-center gap-2 border-b border-border px-4 py-3">
            <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <input
              ref={inputRef}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t("search.placeholder")}
              aria-label={t("search.inputAriaLabel")}
              className="w-full bg-transparent text-sm outline-none placeholder:text-muted-foreground"
            />
          </div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-4 py-2">
            <FilterChipGroup
              legendKey="search.filter.source.legend"
              value={sourceFilter}
              options={SOURCE_FILTER_OPTIONS}
              onChange={setSourceFilter}
            />
            <FilterChipGroup
              legendKey="search.filter.type.legend"
              value={typeFilter}
              options={TYPE_FILTER_OPTIONS}
              onChange={setTypeFilter}
            />
            <FilterChipGroup
              legendKey="search.filter.period.legend"
              value={periodFilter}
              options={PERIOD_FILTER_OPTIONS}
              onChange={setPeriodFilter}
            />
          </div>
          <div className="max-h-96 overflow-y-auto p-2">
            {trimmedQuery.length < MIN_QUERY_LENGTH && (
              <p className="px-3 py-6 text-center text-sm text-muted-foreground">
                {t("search.minLengthHint")}
              </p>
            )}
            {trimmedQuery.length >= MIN_QUERY_LENGTH && isLoading && (
              <p className="px-3 py-6 text-center text-sm text-muted-foreground">
                {t("search.searching")}
              </p>
            )}
            {trimmedQuery.length >= MIN_QUERY_LENGTH && !isLoading && hits && hits.length === 0 && (
              <p className="px-3 py-6 text-center text-sm text-muted-foreground">
                {t("search.noResults")}
              </p>
            )}
            {hits && hits.length > 0 && (
              <ul
                className="space-y-1"
                aria-label={t("search.resultsAriaLabel")}
                aria-live="polite"
              >
                {hits.map((hit) => (
                  <SearchResultItem key={hit.eventId} hit={hit} onSelect={handleSelect} />
                ))}
              </ul>
            )}
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
