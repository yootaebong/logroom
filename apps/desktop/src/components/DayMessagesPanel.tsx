import { useQuery } from "@tanstack/react-query";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ArrowDownUp, ExternalLink, X } from "lucide-react";
import { useState } from "react";
import { formatTime, safeExternalLinkUrl } from "@/components/StreamDetailPanel";
import { Button } from "@/components/ui/button";
import { useT } from "@/i18n";
import { listMyMessagesByDay, queryKeys } from "@/lib/api";
import type { DayMessage } from "@/lib/types";
import { LOCAL_TIMEZONE } from "@/store";

/** 이벤트 시간 정렬 방향 — StreamDetailPanel.tsx와 동일한 asc/desc 개념이지만 이 패널 전용
 * localStorage 키를 따로 쓴다(패널이 다르면 정렬 취향도 다를 수 있음). */
type SortOrder = "asc" | "desc";

const MESSAGES_SORT_STORAGE_KEY = "logroom-summary-messages-sort";

/** 이 패널 `<h2>` 제목의 id — SummaryView.tsx의 `<aside aria-labelledby>`가 참조한다(리뷰 지적,
 * id 문자열을 양쪽에 하드코딩하지 않고 이 상수 하나만 공유). */
export const DAY_MESSAGES_HEADING_ID = "day-messages-heading";

function getInitialSortOrder(): SortOrder {
  return window.localStorage.getItem(MESSAGES_SORT_STORAGE_KEY) === "desc" ? "desc" : "asc";
}

interface DayMessagesPanelProps {
  localDate: string;
  onClose: () => void;
}

/** "내가 쓴 메시지" 1건 — 패널 폭(320px)이 좁아 한 줄로는 시각·스트림·본문을 다 담기 어려워 2줄
 * 구조로 렌더한다(1줄: 시각 + 스트림 배지, 2줄: 본문). 외부 링크 버튼은
 * StreamDetailPanel.tsx::MessageEventRow와 동일한 패턴(safeExternalLinkUrl 통과 시에만 노출). */
function DayMessageRow({ message }: { message: DayMessage }) {
  const t = useT();
  const link = safeExternalLinkUrl(message.url);
  return (
    <li className="group rounded-md px-2 py-1.5 hover:bg-accent/40">
      <div className="flex items-center gap-2">
        <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
          {formatTime(message.ts)}
        </span>
        {message.streamTitle && (
          <span className="min-w-0 truncate text-[11px] text-muted-foreground">
            {message.streamTitle}
          </span>
        )}
        {link && (
          <button
            type="button"
            aria-label={t("stream.openExternalLink")}
            title={t("stream.openExternalLink")}
            onClick={() => void openUrl(link)}
            className="ml-auto shrink-0 rounded-sm p-0.5 text-muted-foreground opacity-0 transition-opacity hover:text-foreground focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring group-hover:opacity-100"
          >
            <ExternalLink className="size-3.5" aria-hidden="true" />
          </button>
        )}
      </div>
      <p className="mt-0.5 whitespace-pre-wrap break-words text-sm">
        {message.body ?? message.title ?? ""}
      </p>
    </li>
  );
}

/**
 * 일간 AI 요약 화면 우측 패널(SummaryView.tsx) — 그날 내가 쓴 메시지(Slack/GitHub/Linear 원문)를
 * 원문 그대로 나열한다. 요약은 LLM이 재해석한 서사라 "내가 정확히 뭐라고 썼더라"를 확인하려면
 * 원문 대조가 필요하다는 요구에서 나왔다(설계 원칙 "숫자는 DB, 서사는 LLM"과 같은 맥락 — 원문도
 * 가공 없이 DB 그대로 노출). 창 폭 ≥1024px(lg)면 요약과 나란히 정적 배치, 미만이면 오버레이
 * 드로어로 뜬다(SummaryView.tsx의 반응형 aside 클래스 분기).
 */
export function DayMessagesPanel({ localDate, onClose }: DayMessagesPanelProps) {
  const t = useT();
  const [sortOrder, setSortOrder] = useState<SortOrder>(getInitialSortOrder);

  const { data, isLoading, isError } = useQuery({
    queryKey: queryKeys.myMessages(localDate, LOCAL_TIMEZONE),
    queryFn: () => listMyMessagesByDay(localDate, LOCAL_TIMEZONE),
  });

  const messages = data?.messages ?? [];
  // ts ASC로 들어온다 — desc면 역순 렌더(원본 배열 불변 유지 위해 복사, StreamDetailPanel.tsx와 동일 패턴).
  const ordered = sortOrder === "desc" ? [...messages].reverse() : messages;

  // 스크린리더에 현재 정렬 방향 + 전환 결과를 함께 전달(리뷰 지적 — 고정 문구는 방향을 알 수 없음).
  const sortButtonLabel = t(
    sortOrder === "asc" ? "summary.messages.sortAsc" : "summary.messages.sortDesc",
  );

  function toggleSortOrder() {
    setSortOrder((prev) => {
      const next: SortOrder = prev === "asc" ? "desc" : "asc";
      window.localStorage.setItem(MESSAGES_SORT_STORAGE_KEY, next);
      return next;
    });
  }

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center justify-between gap-2 border-b border-border p-4">
        <div className="min-w-0">
          <h2 id={DAY_MESSAGES_HEADING_ID} className="truncate text-sm font-semibold">
            {t("summary.messages.heading")}
          </h2>
          <p className="text-xs text-muted-foreground">
            {t("summary.messages.count", { count: messages.length })}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={sortButtonLabel}
            title={sortButtonLabel}
            onClick={toggleSortOrder}
          >
            <ArrowDownUp className="size-4" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={t("summary.messages.close")}
            onClick={onClose}
          >
            <X className="size-4" />
          </Button>
        </div>
      </header>
      {isLoading ? (
        <p className="p-4 text-sm text-muted-foreground">{t("common.loading")}</p>
      ) : isError ? (
        <div className="flex min-h-0 flex-1 items-center justify-center p-6 text-center text-sm text-muted-foreground">
          {t("summary.messages.error")}
        </div>
      ) : ordered.length === 0 ? (
        <div className="flex min-h-0 flex-1 items-center justify-center p-6 text-center text-sm text-muted-foreground">
          {t("summary.messages.empty")}
        </div>
      ) : (
        <ul className="min-h-0 flex-1 space-y-0.5 overflow-y-auto p-2">
          {ordered.map((message) => (
            <DayMessageRow key={message.id} message={message} />
          ))}
        </ul>
      )}
    </div>
  );
}
