import type { Source } from "@logroom/core";
import { Clock, Keyboard, Newspaper, Settings, Sparkles } from "lucide-react";
import { Popover } from "radix-ui";
import type { ComponentType } from "react";
import { CaptureHealthBadge } from "@/components/CaptureHealthBadge";
import { Button } from "@/components/ui/button";
import type { ResolvedLocale, UiKey } from "@/i18n";
import { useResolvedLocale, useT } from "@/i18n";
import { cn } from "@/lib/utils";
import type { AppView } from "@/store";
import { useAppStore } from "@/store";

const WEEKDAY_LABELS_KO = ["일", "월", "화", "수", "목", "금", "토"];

/** currentDate(YYYY-MM-DD)를 사이드바 상단에 표시할 문구로 포맷한다(docs/02 로컬 TZ 표시 기준).
 * ko: "7월 8일 (화)", en: "Jul 8 (Tue)"(Intl `toLocaleDateString` 활용, en-US 로케일 고정). */
function formatSidebarDate(localDate: string, locale: ResolvedLocale): string {
  const [year, month, day] = localDate.split("-").map(Number);
  const date = new Date(year ?? 1970, (month ?? 1) - 1, day ?? 1);
  if (locale === "ko") {
    return `${date.getMonth() + 1}월 ${date.getDate()}일 (${WEEKDAY_LABELS_KO[date.getDay()]})`;
  }
  const monthLabel = date.toLocaleDateString("en-US", { month: "short" });
  const weekdayLabel = date.toLocaleDateString("en-US", { weekday: "short" });
  return `${monthLabel} ${date.getDate()} (${weekdayLabel})`;
}

/** 사이드바 "뷰" 섹션 항목 — 슬랙의 채널 리스트처럼 아이콘+라벨, 활성 항목은 배경 하이라이트. */
const VIEW_ITEMS: Array<{
  value: AppView;
  labelKey: UiKey;
  icon: ComponentType<{ className?: string }>;
}> = [
  { value: "digest", labelKey: "sidebar.view.digest", icon: Newspaper },
  { value: "timeline", labelKey: "sidebar.view.timeline", icon: Clock },
  { value: "summary", labelKey: "sidebar.view.summary", icon: Sparkles },
];

/** 프로젝트 섹션 위 소스 필터 토글(Claude/Kiro/Slack/GitHub/Linear/Notion) — 브랜드명이라 로케일과 무관하게 고정. */
const VIEW_FILTER_SOURCES: Array<{ value: Source; label: string }> = [
  { value: "claude_code", label: "Claude" },
  { value: "kiro_cli", label: "Kiro" },
  { value: "slack", label: "Slack" },
  { value: "github", label: "GitHub" },
  { value: "linear", label: "Linear" },
  { value: "notion", label: "Notion" },
];

/** 사이드바 하단의 키보드 단축키 힌트 버튼 — 기존 App.tsx KeyboardShortcutsHint에서 이전. */
const KEYBOARD_SHORTCUTS: Array<{ keys: string; labelKey: UiKey }> = [
  { keys: "← / →", labelKey: "sidebar.shortcuts.dateNav" },
  { keys: "t", labelKey: "sidebar.shortcuts.today" },
  { keys: "j / k", labelKey: "sidebar.shortcuts.streamNav" },
  { keys: "⌘K", labelKey: "sidebar.shortcuts.search" },
  { keys: "Esc", labelKey: "sidebar.shortcuts.closeDetail" },
];

function KeyboardShortcutsHint() {
  const t = useT();
  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          aria-label={t("sidebar.shortcutsAriaLabel")}
        >
          <Keyboard className="size-4" />
        </Button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          side="top"
          align="start"
          sideOffset={8}
          className="z-50 w-64 rounded-lg border border-border bg-popover p-3 text-popover-foreground shadow-lg outline-none"
        >
          <p className="text-xs font-semibold text-muted-foreground">
            {t("sidebar.shortcutsHeading")}
          </p>
          <ul className="mt-2 space-y-1.5">
            {KEYBOARD_SHORTCUTS.map((shortcut) => (
              <li key={shortcut.keys} className="flex items-center justify-between gap-3 text-xs">
                <span className="text-muted-foreground">{t(shortcut.labelKey)}</span>
                <kbd className="rounded-sm border border-border bg-muted px-1.5 py-0.5 font-mono text-[11px]">
                  {shortcut.keys}
                </kbd>
              </li>
            ))}
          </ul>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

/**
 * LogRoom 좌측 사이드 레일(슬랙 워크스페이스 사이드바 메타포, docs/05-ui-ux.md 개편).
 * 뷰 전환(다이제스트/타임라인) + 프로젝트 필터(채널 리스트 메타포) + 소스 필터를 담고,
 * 하단에는 캡처 헬스 배지·설정·키보드 힌트를 고정 배치한다. 헤더에 있던 뷰 탭/필터 바를
 * 대체한다 — store의 `view`/`viewFilters`/`dayProjects`를 그대로 재사용(시그니처 불변).
 */
export function Sidebar() {
  const t = useT();
  const locale = useResolvedLocale();
  const view = useAppStore((state) => state.view);
  const setView = useAppStore((state) => state.setView);
  const viewFilters = useAppStore((state) => state.viewFilters);
  const setViewFilterSources = useAppStore((state) => state.setViewFilterSources);
  const setViewFilterProject = useAppStore((state) => state.setViewFilterProject);
  const resetViewFilters = useAppStore((state) => state.resetViewFilters);
  const dayProjects = useAppStore((state) => state.dayProjects);
  const currentDate = useAppStore((state) => state.currentDate);
  const setSettingsOpen = useAppStore((state) => state.setSettingsOpen);
  const updateAvailable = useAppStore((state) => state.updateAvailable);

  const isFilterActive = viewFilters.sources.length > 0 || viewFilters.project !== null;

  function toggleSource(source: Source) {
    const next = viewFilters.sources.includes(source)
      ? viewFilters.sources.filter((s) => s !== source)
      : [...viewFilters.sources, source];
    setViewFilterSources(next);
  }

  return (
    <nav
      aria-label={t("sidebar.ariaLabel")}
      className="flex w-[230px] shrink-0 flex-col border-r border-border bg-[var(--sidebar-bg)]"
    >
      <div className="px-3 pt-3 pb-2">
        <p className="text-sm font-semibold tracking-tight">LogRoom</p>
        <p className="mt-0.5 text-[11px] text-muted-foreground tabular-nums">
          {formatSidebarDate(currentDate, locale)}
        </p>
      </div>

      <div className="px-2 pb-2">
        <p className="px-1 pb-1 text-[11px] font-semibold tracking-wide text-muted-foreground uppercase">
          {t("sidebar.viewSectionLabel")}
        </p>
        <ul className="space-y-0.5">
          {VIEW_ITEMS.map(({ value, labelKey, icon: Icon }) => {
            const isActive = view === value;
            return (
              <li key={value}>
                <button
                  type="button"
                  aria-current={isActive ? "true" : undefined}
                  onClick={() => setView(value)}
                  className={cn(
                    "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-accent/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    isActive
                      ? "bg-accent font-medium text-accent-foreground"
                      : "text-muted-foreground",
                  )}
                >
                  <Icon className="size-4 shrink-0" />
                  {t(labelKey)}
                </button>
              </li>
            );
          })}
        </ul>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-2 pb-2">
        <div className="flex items-center justify-between gap-2 px-1 pb-1">
          <p className="text-[11px] font-semibold tracking-wide text-muted-foreground uppercase">
            {t("sidebar.projectSectionLabel")}
          </p>
          <div className="flex items-center gap-2">
            {isFilterActive && (
              <button
                type="button"
                onClick={resetViewFilters}
                className="text-[11px] text-muted-foreground hover:text-foreground hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                {t("sidebar.resetFilters")}
              </button>
            )}
          </div>
        </div>

        {/* 커넥터가 늘어도 가로 스크롤이 생기지 않게 줄바꿈(flex-wrap) — 230px 고정 폭 사이드바에서
            칩 5개+부터 한 줄을 넘친다(실사용 리포트). */}
        <fieldset className="mb-1.5 flex flex-wrap items-center gap-1 px-1">
          <legend className="sr-only">{t("sidebar.sourceFilterLegend")}</legend>
          {VIEW_FILTER_SOURCES.map(({ value, label }) => {
            const isActive = viewFilters.sources.includes(value);
            return (
              <button
                key={value}
                type="button"
                aria-pressed={isActive}
                onClick={() => toggleSource(value)}
                className={cn(
                  "rounded-full border px-2 py-0.5 text-[11px] transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                  isActive
                    ? "border-primary bg-primary/20 text-foreground"
                    : "border-border text-muted-foreground hover:bg-accent hover:text-accent-foreground",
                )}
              >
                {label}
              </button>
            );
          })}
        </fieldset>

        <ul className="space-y-0.5">
          <li>
            <button
              type="button"
              aria-current={viewFilters.project === null ? "true" : undefined}
              onClick={() => setViewFilterProject(null)}
              className={cn(
                "flex w-full items-center rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-accent/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                viewFilters.project === null
                  ? "bg-accent font-medium text-accent-foreground"
                  : "text-muted-foreground",
              )}
            >
              {t("sidebar.allProjects")}
            </button>
          </li>
          {dayProjects.map((project) => {
            // dayProjects는 이미 표시 이름(마지막 세그먼트) 기준으로 dedupe된 목록이다 —
            // project 원본 키가 소스마다 달라도(Claude 절대경로 vs GitHub owner/repo) 표시 이름이
            // 같으면 여기서 하나로 묶여 나온다(lib/utils.ts::uniqueProjectNames 참고).
            const isActive = viewFilters.project === project;
            return (
              <li key={project}>
                <button
                  type="button"
                  title={project}
                  aria-current={isActive ? "true" : undefined}
                  onClick={() => setViewFilterProject(isActive ? null : project)}
                  className={cn(
                    "flex w-full items-center rounded-md px-2 py-1.5 text-left font-mono text-[13px] transition-colors hover:bg-accent/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    isActive
                      ? "bg-accent font-medium text-accent-foreground"
                      : "text-muted-foreground",
                  )}
                >
                  <span className="truncate">
                    <span aria-hidden="true">#</span> {project}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      </div>

      <div className="border-t border-border px-2 py-2">
        <CaptureHealthBadge />
        <div className="mt-1 flex items-center justify-end gap-1">
          <KeyboardShortcutsHint />
          <div className="relative">
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={
                updateAvailable
                  ? t("sidebar.settingsAriaLabelWithUpdate", { version: updateAvailable.version })
                  : t("sidebar.settingsAriaLabel")
              }
              onClick={() => setSettingsOpen(true)}
            >
              <Settings className="size-4" />
            </Button>
            {updateAvailable && (
              <span
                aria-hidden="true"
                className="absolute top-0.5 right-0.5 size-1.5 rounded-full bg-amber-500"
              />
            )}
          </div>
        </div>
      </div>
    </nav>
  );
}
