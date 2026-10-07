import { useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { ChevronLeft, ChevronRight, Search } from "lucide-react";
import { useEffect } from "react";
import { DigestView } from "@/components/DigestView";
import { OnboardingDialog } from "@/components/OnboardingDialog";
import { SearchDialog } from "@/components/SearchDialog";
import { SettingsDialog } from "@/components/SettingsDialog";
import { Sidebar } from "@/components/Sidebar";
import { StreamDetailPanel } from "@/components/StreamDetailPanel";
import { SummaryView } from "@/components/SummaryView";
import { Timeline } from "@/components/Timeline";
import { UpdateModal } from "@/components/UpdateModal";
import { Button } from "@/components/ui/button";
import { useAutoDailySummary } from "@/hooks/useAutoDailySummary";
import { useGlobalKeyboardNav } from "@/hooks/useGlobalKeyboardNav";
import { resolveLocale, useT } from "@/i18n";
import {
  QUERY_KEY_CAPTURE_HEALTH,
  QUERY_KEY_CAPTURE_PAUSED,
  QUERY_KEY_DIGEST,
  QUERY_KEY_EVENTS_BY_DAY,
  QUERY_KEY_STREAM,
  setAppLocale,
} from "@/lib/api";
import type { UpdateInfo } from "@/lib/types";
import { cn } from "@/lib/utils";
import { shiftLocalDate, toLocalDateString, useAppStore } from "@/store";

/** Rust 인제스트 경로에서 새 이벤트가 저장될 때마다 emit되는 이벤트명 (docs/05-ui-ux.md). */
const EVENT_INGESTED = "event-ingested";
/** 트레이 메뉴 "오늘 요약" 클릭 시 emit되는 이벤트명 — 다이제스트 뷰로 전환한다(docs/05-ui-ux.md "앱 표면"). */
const EVENT_OPEN_DIGEST = "open-digest";
/** 트레이 "캡처 일시정지" 토글이 emit하는 이벤트명. 설정 다이얼로그/사이드바 배지 쪽에서도 즉시
 * 반영되도록 관련 쿼리를 invalidate한다(양방향 동기화 — Rust `tray.rs`). */
const EVENT_CAPTURE_PAUSED_CHANGED = "capture-paused-changed";
/** 백그라운드/수동 업데이트 확인으로 새 버전을 발견했을 때 emit되는 이벤트명(M5, ADR-0013,
 * Rust `update.rs`). 자동 다운로드 없이 알림만 — store에 담아 사이드바 배지 + 설정 다이얼로그가 공유한다. */
const EVENT_UPDATE_AVAILABLE = "update-available";

/**
 * LogRoom Today 화면(슬랙식 셸 개편): 좌 사이드 레일(뷰 전환·프로젝트/소스 필터·헬스·설정) +
 * 얇은 헤더(날짜 네비 + ⌘K 검색 트리거) + 본문(타임라인/다이제스트, 상세 선택 시 전폭에서
 * 우측 드로어가 슬라이드 인) + ⌘K 검색 다이얼로그. docs/05-ui-ux.md M1/M4 화면 ①②,
 * docs/02-data-model.md 레인 배치/시간 처리 참고.
 */
function App() {
  const t = useT();
  const queryClient = useQueryClient();
  const view = useAppStore((state) => state.view);
  const setView = useAppStore((state) => state.setView);
  const theme = useAppStore((state) => state.theme);
  const locale = useAppStore((state) => state.locale);
  const currentDate = useAppStore((state) => state.currentDate);
  const setCurrentDate = useAppStore((state) => state.setCurrentDate);
  const setSearchOpen = useAppStore((state) => state.setSearchOpen);
  const selectedStreamId = useAppStore((state) => state.selectedStreamId);
  const setUpdateAvailable = useAppStore((state) => state.setUpdateAvailable);

  /** 우측 드로어 열림 여부 — 별도 store 상태 없이 selectedStreamId로 파생(제약: store 시그니처 유지). */
  const isDetailOpen = selectedStreamId !== null;

  useGlobalKeyboardNav();
  useAutoDailySummary();

  /** 테마 변경 시 `<html>` 클래스를 동기화한다(index.css `.dark`/`.theme-warm`). index.html의
   * 하드코딩 `class="dark"` + 인라인 스크립트는 첫 페인트까지의 FOUC 방지 기본값이고, 마운트
   * 이후에는 이 effect가 store(theme, localStorage persist)를 단일 소스로 클래스를 맞춘다. */
  useEffect(() => {
    const root = document.documentElement;
    root.classList.toggle("dark", theme === "dark");
    root.classList.toggle("theme-warm", theme === "warm");
  }, [theme]);

  /** 트레이 메뉴 로케일을 Rust config.json에 동기화한다(적용은 다음 재시작부터, Settings "언어"
   * 섹션 안내 문구 참고). "system"은 여기서 실제 표시 언어("en"/"ko")로 해석해 넘긴다 — Rust는
   * "system" 개념 없이 항상 해석된 값만 저장한다. */
  useEffect(() => {
    void setAppLocale(resolveLocale(locale)).catch(() => {
      // 트레이 라벨은 다음 재시작에만 반영되는 부가 동기화라 실패해도 앱 사용에는 영향 없다.
    });
  }, [locale]);

  useEffect(() => {
    const unlistenPromise = listen(EVENT_INGESTED, () => {
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_EVENTS_BY_DAY] });
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_STREAM] });
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_DIGEST] });
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [queryClient]);

  useEffect(() => {
    const unlistenPromise = listen(EVENT_OPEN_DIGEST, () => {
      setView("digest");
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [setView]);

  useEffect(() => {
    const unlistenPromise = listen(EVENT_CAPTURE_PAUSED_CHANGED, () => {
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_CAPTURE_PAUSED] });
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_CAPTURE_HEALTH] });
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [queryClient]);

  useEffect(() => {
    const unlistenPromise = listen<UpdateInfo>(EVENT_UPDATE_AVAILABLE, (event) => {
      setUpdateAvailable(event.payload);
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [setUpdateAvailable]);

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-background text-foreground">
      <Sidebar />
      <div className="relative flex min-w-0 flex-1 flex-col overflow-hidden">
        <header className="flex h-12 shrink-0 items-center justify-between gap-2 border-b border-border px-4">
          <div className="flex items-center gap-1">
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={t("app.header.prevDay")}
              onClick={() => setCurrentDate(shiftLocalDate(currentDate, -1))}
            >
              <ChevronLeft className="size-4" />
            </Button>
            {/* 날짜 클릭 → 네이티브 캘린더로 특정 날짜 즉시 이동(연타 네비 pain 해소).
                input[type=date] 자체를 표시해 별도 팝오버/패키지 없이 webview 기본 피커 사용. */}
            <input
              type="date"
              aria-label={t("app.header.selectDate")}
              value={currentDate}
              onChange={(e) => {
                if (e.target.value) setCurrentDate(e.target.value);
              }}
              className="min-w-24 cursor-pointer rounded-md border border-transparent bg-transparent px-1 text-center text-sm font-medium tabular-nums hover:border-border focus-visible:border-border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-calendar-picker-indicator]:cursor-pointer"
            />
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={t("app.header.nextDay")}
              onClick={() => setCurrentDate(shiftLocalDate(currentDate, 1))}
            >
              <ChevronRight className="size-4" />
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setCurrentDate(toLocalDateString(new Date()))}
            >
              {t("app.header.today")}
            </Button>
          </div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="gap-1.5"
            onClick={() => setSearchOpen(true)}
          >
            <Search className="size-3.5" />
            {t("app.header.search")}
            <kbd className="rounded-sm border border-border bg-muted px-1.5 py-0.5 font-mono text-[10px]">
              ⌘K
            </kbd>
          </Button>
        </header>
        <main className="min-h-0 flex-1 overflow-hidden">
          {view === "timeline" ? (
            <Timeline />
          ) : view === "summary" ? (
            <SummaryView />
          ) : (
            <DigestView />
          )}
        </main>
        <aside
          aria-label={t("app.streamDetailAriaLabel")}
          inert={!isDetailOpen}
          className={cn(
            "absolute inset-y-0 right-0 z-20 flex w-[420px] max-w-full flex-col border-l border-border bg-background shadow-2xl transition-transform duration-200 ease-in-out motion-reduce:transition-none",
            isDetailOpen ? "translate-x-0" : "translate-x-full",
          )}
        >
          <StreamDetailPanel />
        </aside>
      </div>
      <SearchDialog />
      <SettingsDialog />
      <OnboardingDialog />
      <UpdateModal />
    </div>
  );
}

export default App;
