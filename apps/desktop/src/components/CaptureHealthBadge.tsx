import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Popover } from "radix-ui";
import { Button } from "@/components/ui/button";
import type { Translate, UiKey } from "@/i18n";
import { useT } from "@/i18n";
import { getCaptureHealth, getCapturePaused, queryKeys, setCapturePaused } from "@/lib/api";
import type { CaptureHealthStatus, HealthSource } from "@/lib/types";
import { cn } from "@/lib/utils";

/** 30초마다 재조회. BE 스캔(디스크 mtime 확인)은 이 커맨드 호출 시점에만 수행되므로
 * 폴링 간격이 곧 "캡처 멈춤" 감지 지연이다(docs/03-capture.md "캡처 헬스"). */
const CAPTURE_HEALTH_POLL_MS = 30_000;

/** HealthSource 브랜드명 — 로케일과 무관하게 고정(Claude Code/Kiro CLI/Slack/GitHub/Linear/Notion). */
const SOURCE_LABELS: Record<HealthSource, string> = {
  claude_code: "Claude Code",
  kiro_cli: "Kiro CLI",
  slack: "Slack",
  github: "GitHub",
  linear: "Linear",
  notion: "Notion",
};

/** Slack/GitHub/Linear/Notion(폴러형)는 파일 root 개념이 없다 — Slack은 설정 여부(0/1)만,
 * GitHub/Linear/Notion은 다중 계정 지원이라 설정된 계정 수를 표시한다(docs/08-connectors.md
 * "헬스 판정" — BE `roots` 필드 의미가 소스마다 다르다).
 * 주의: Notion은 status가 ok여도 페이지 연결이 없으면 캡처가 0건이다 — 헬스로는 구분되지 않는다. */
function formatRootsLabel(source: HealthSource, roots: number, t: Translate): string {
  if (source === "slack") return roots > 0 ? t("health.configured") : t("health.notConfigured");
  if (source === "github" || source === "linear" || source === "notion")
    return t("health.accountsCount", { n: roots });
  return t("health.rootsCount", { n: roots });
}

/** CaptureHealthStatus → i18n 키(타입 안전 매핑, StreamDetailPanel.tsx의 EVENT_TYPE_KEYS와 동일 패턴). */
const STATUS_LABEL_KEYS: Record<CaptureHealthStatus, UiKey> = {
  ok: "health.status.ok",
  stale: "health.status.stale",
  inactive: "health.status.inactive",
};

const STATUS_DOT_CLASS: Record<CaptureHealthStatus, string> = {
  ok: "bg-emerald-500",
  stale: "bg-amber-500",
  inactive: "bg-zinc-500",
};

function formatCapturedAt(ms: number | null, t: Translate): string {
  if (ms === null) return t("common.none");
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/**
 * 사이드바 하단의 캡처 상태 배지(docs/03-capture.md "캡처 헬스").
 * 트리거는 **집계형**(대표 점 1개 + "캡처 정상 · N개 소스") — 초기엔 소스별 점+이니셜을 나열했지만
 * 커넥터가 늘며 230px 사이드바 폭을 넘쳐(실사용 리포트) 소스별 상세는 팝오버로 일원화했다.
 * 색만으로 상태를 전달하지 않도록 상태 문구를 함께 보여주고,
 * 지연 의심(stale) 소스가 하나라도 있으면 트리거 자체에 "캡처 지연" 문구를 강조한다.
 * **일시정지(M4)** 중이면 지연보다 우선해 "⏸ 캡처 일시정지됨"을 강조 표시한다
 * (몰래 꺼진 채로 잊히는 것을 막기 위해 일부러 눈에 띄게 — docs/04-privacy-security.md).
 * 클릭하면 소스별 상세(상태·마지막 캡처 시각·root 수) + 일시정지 토글을 팝오버로 보여준다.
 */
export function CaptureHealthBadge() {
  const t = useT();
  const queryClient = useQueryClient();

  const { data } = useQuery({
    queryKey: queryKeys.captureHealth(),
    queryFn: getCaptureHealth,
    refetchInterval: CAPTURE_HEALTH_POLL_MS,
  });
  const { data: paused } = useQuery({
    queryKey: queryKeys.capturePaused(),
    queryFn: getCapturePaused,
    refetchInterval: CAPTURE_HEALTH_POLL_MS,
  });

  const pauseMutation = useMutation({
    mutationFn: (nextPaused: boolean) => setCapturePaused(nextPaused),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.capturePaused() });
      queryClient.invalidateQueries({ queryKey: queryKeys.captureHealth() });
    },
  });

  const sources = data?.sources ?? [];
  const hasStale = sources.some((source) => source.status === "stale");
  const isPaused = paused === true;
  const isPausedUnknown = paused === undefined;

  // 트리거 문구 우선순위: 일시정지 > 지연 > 정상 (일시정지가 가장 눈에 띄어야 함).
  const triggerLabel = isPaused
    ? t("health.trigger.paused")
    : hasStale
      ? t("health.trigger.stale")
      : t("health.trigger.ok");
  const triggerHighlighted = isPaused || hasStale;

  // 소스별 상태 + 일시정지 여부까지 접근성 이름에 포함(색상만으로 전달 금지 — WCAG 1.4.1).
  const statusWord = isPaused
    ? t("health.state.paused")
    : hasStale
      ? t("health.state.delayed")
      : t("health.state.normal");
  const ariaLabel = t("health.ariaLabel", {
    status: statusWord,
    sources: sources
      .map((s) => `${SOURCE_LABELS[s.source]} ${t(STATUS_LABEL_KEYS[s.status])}`)
      .join(", "),
  });

  // 집계 점 색: 일시정지/지연=amber, 하나라도 정상 캡처 중=emerald, 전부 미감지=zinc.
  const aggregateDotClass =
    isPaused || hasStale
      ? "bg-amber-500"
      : sources.some((source) => source.status === "ok")
        ? "bg-emerald-500"
        : "bg-zinc-500";

  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          aria-label={ariaLabel}
          title={sources
            .map((s) => `${SOURCE_LABELS[s.source]}: ${t(STATUS_LABEL_KEYS[s.status])}`)
            .join(" · ")}
          className="w-full justify-start gap-1.5"
        >
          <span aria-hidden="true" className={cn("size-2 rounded-full", aggregateDotClass)} />
          <span
            className={cn(
              "truncate text-xs",
              triggerHighlighted
                ? "font-medium text-amber-600 dark:text-amber-400"
                : "text-muted-foreground",
            )}
          >
            {triggerLabel}
            {sources.length > 0 && (
              <span className="text-muted-foreground">
                {" · "}
                {t("health.trigger.sourceCount", { n: sources.length })}
              </span>
            )}
          </span>
        </Button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          align="end"
          sideOffset={8}
          aria-live="polite"
          className="z-50 w-72 rounded-lg border border-border bg-popover p-3 text-popover-foreground shadow-lg outline-none"
        >
          <p className="text-xs font-semibold text-muted-foreground">
            {t("health.popoverHeading")}
          </p>
          <ul className="mt-2 space-y-2">
            {sources.map((source) => (
              <li key={source.source} className="flex items-start justify-between gap-3 text-xs">
                <div className="flex items-center gap-1.5 font-medium">
                  <span
                    aria-hidden="true"
                    className={cn("size-2 shrink-0 rounded-full", STATUS_DOT_CLASS[source.status])}
                  />
                  {SOURCE_LABELS[source.source]}
                </div>
                <div className="text-right text-muted-foreground">
                  <p>
                    {t(STATUS_LABEL_KEYS[source.status])} ·{" "}
                    {formatRootsLabel(source.source, source.roots, t)}
                  </p>
                  <p>
                    {t("health.lastCaptured", { time: formatCapturedAt(source.lastCapturedAt, t) })}
                  </p>
                </div>
              </li>
            ))}
          </ul>

          <div className="mt-3 space-y-2 border-t border-border pt-3">
            <p className="flex items-center gap-1.5 text-xs">
              <span
                aria-hidden="true"
                className={cn(
                  "size-2 shrink-0 rounded-full",
                  isPaused ? "bg-amber-500" : "bg-emerald-500",
                )}
              />
              <span
                className={cn(
                  "font-medium",
                  isPaused ? "text-amber-600 dark:text-amber-400" : "text-muted-foreground",
                )}
              >
                {isPaused ? t("health.popover.paused") : t("health.popover.running")}
              </span>
            </p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="w-full"
              aria-label={isPaused ? t("health.resumeAriaLabel") : t("health.pauseAriaLabel")}
              onClick={() => pauseMutation.mutate(!isPaused)}
              disabled={isPausedUnknown || pauseMutation.isPending}
            >
              {pauseMutation.isPending
                ? t("common.processing")
                : isPaused
                  ? t("health.resumeButton")
                  : t("health.pauseButton")}
            </Button>
          </div>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
