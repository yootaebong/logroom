import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ClipboardList, FolderKanban, Plug, Sparkles } from "lucide-react";
import { Dialog } from "radix-ui";
import { type RefObject, useEffect, useRef, useState } from "react";
import {
  SOURCE_CONFIG_KEYS,
  SOURCE_LABELS,
  ThemeSection,
  withConfigDefaults,
} from "@/components/SettingsDialog";
import { Button } from "@/components/ui/button";
import { useT } from "@/i18n";
import {
  detectSummaryEngine,
  getCaptureConfig,
  getSummaryConfig,
  queryKeys,
  setCaptureConfig,
  setSummaryConfig,
} from "@/lib/api";
import { openFeedbackMail } from "@/lib/feedback";
import type { CaptureConfig, CaptureSource, ResolvedCaptureRoot, SummaryConfig } from "@/lib/types";
import { cn } from "@/lib/utils";
import { useAppStore } from "@/store";

/** 온보딩 전체 스텝 수(환영 → 소스 선택 → 신기능 소개 → AI 요약 opt-in → backfill 안내 → 완료). */
const TOTAL_STEPS = 6;

/** 온보딩에서 소개하는 캡처 소스 — SettingsDialog와 동일한 2종(Slack은 root 개념이 없어
 * 별도 안내 문구만 보여준다, docs/08-connectors.md). */
const ONBOARDING_SOURCES: readonly CaptureSource[] = ["claude_code", "kiro_cli"];

/** 소스별 "감지됨 N개 위치" 계산 — 기본(자동 탐지) root 중 실제 존재하는 것만 센다. */
function countDetectedRoots(resolvedRoots: ResolvedCaptureRoot[], source: CaptureSource): number {
  return resolvedRoots.filter(
    (root) => root.source === source && root.origin === "default" && root.exists,
  ).length;
}

interface StepHeadingProps {
  headingRef: RefObject<HTMLHeadingElement | null>;
}

/** 스텝 1 — 환영 + 프라이버시 선언. 랜딩 프라이버시 서약 톤(no cloud·zero telemetry·시크릿 스크럽
 * 기본 on)을 임팩트 있게, 과하지 않게 전달한다(docs/05-ui-ux.md "첫 실행 온보딩"). */
function WelcomeStep({ headingRef }: StepHeadingProps) {
  const t = useT();
  return (
    <div className="flex h-full flex-col items-center justify-center gap-4 py-6 text-center">
      <p className="text-xs font-semibold tracking-widest text-muted-foreground uppercase">
        LogRoom
      </p>
      <Dialog.Title ref={headingRef} tabIndex={-1} className="text-2xl font-semibold text-balance">
        {t("onboarding.welcome.title")}
      </Dialog.Title>
      <Dialog.Description className="max-w-sm text-sm text-muted-foreground">
        {t("onboarding.welcome.description")}
      </Dialog.Description>
      <ul className="flex flex-wrap items-center justify-center gap-2 text-xs text-muted-foreground">
        <li className="rounded-full border border-border px-2.5 py-1">
          {t("onboarding.welcome.badge.noCloud")}
        </li>
        <li className="rounded-full border border-border px-2.5 py-1">
          {t("onboarding.welcome.badge.noTelemetry")}
        </li>
        <li className="rounded-full border border-border px-2.5 py-1">
          {t("onboarding.welcome.badge.secretScrub")}
        </li>
      </ul>
    </div>
  );
}

interface SourcesStepProps extends StepHeadingProps {
  config: CaptureConfig | null;
  resolvedRoots: ResolvedCaptureRoot[];
  isLoading: boolean;
  isError: boolean;
  saveError: boolean;
  onToggle: (source: CaptureSource, enabled: boolean) => void;
}

/** 스텝 2 — 자동 감지된 소스 목록(Claude Code/Kiro CLI)을 보여주고 enabled 체크박스로 켜고 끈다
 * (기존 `get_capture_config`/`set_capture_config` 흐름 재사용). Slack은 root 개념이 없어 커넥터
 * 안내 문구만 노출한다. */
function SourcesStep({
  headingRef,
  config,
  resolvedRoots,
  isLoading,
  isError,
  saveError,
  onToggle,
}: SourcesStepProps) {
  const t = useT();
  return (
    <div className="space-y-4">
      <div>
        <Dialog.Title ref={headingRef} tabIndex={-1} className="text-lg font-semibold">
          {t("onboarding.sources.title")}
        </Dialog.Title>
        <Dialog.Description className="mt-1 text-sm text-muted-foreground">
          {t("onboarding.sources.description")}
        </Dialog.Description>
      </div>

      {isLoading && (
        <p className="text-sm text-muted-foreground">{t("onboarding.sources.detecting")}</p>
      )}
      {isError && <p className="text-sm text-destructive">{t("settings.loadError")}</p>}

      {config && (
        <ul className="space-y-2">
          {ONBOARDING_SOURCES.map((source) => {
            const sourceConfig = config[SOURCE_CONFIG_KEYS[source]];
            const detected = countDetectedRoots(resolvedRoots, source);
            const inputId = `onboarding-source-${source}`;
            return (
              <li
                key={source}
                className="flex items-center justify-between gap-3 rounded-md border border-border px-3 py-2"
              >
                <label htmlFor={inputId} className="flex items-center gap-2 text-sm font-medium">
                  <input
                    id={inputId}
                    type="checkbox"
                    checked={sourceConfig.enabled}
                    onChange={(event) => onToggle(source, event.target.checked)}
                    className="size-4 rounded border-border accent-primary"
                  />
                  {SOURCE_LABELS[source]}
                </label>
                <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                  <span
                    aria-hidden="true"
                    className={cn(
                      "size-2 rounded-full",
                      detected > 0 ? "bg-emerald-500" : "bg-zinc-500",
                    )}
                  />
                  {detected > 0
                    ? t("onboarding.sources.detected", { n: detected })
                    : t("onboarding.sources.notDetected")}
                </span>
              </li>
            );
          })}
        </ul>
      )}

      <p className="rounded-md border border-dashed border-border px-3 py-2 text-xs text-muted-foreground">
        {t("onboarding.sources.slackNote")}
      </p>

      <p className="min-h-4 text-xs" aria-live="polite">
        {saveError && <span className="text-destructive">{t("common.saveError")}</span>}
      </p>
    </div>
  );
}

/** 하이라이트 카드 1개 — 아이콘 + 제목 + 한 줄 설명. */
const HIGHLIGHT_ITEMS: ReadonlyArray<{
  icon: typeof Sparkles;
  titleKey:
    | "onboarding.highlights.autoOrganize.title"
    | "onboarding.highlights.connectors.title"
    | "onboarding.highlights.aiSummary.title"
    | "onboarding.highlights.resume.title";
  descriptionKey:
    | "onboarding.highlights.autoOrganize.description"
    | "onboarding.highlights.connectors.description"
    | "onboarding.highlights.aiSummary.description"
    | "onboarding.highlights.resume.description";
}> = [
  {
    icon: FolderKanban,
    titleKey: "onboarding.highlights.autoOrganize.title",
    descriptionKey: "onboarding.highlights.autoOrganize.description",
  },
  {
    icon: Plug,
    titleKey: "onboarding.highlights.connectors.title",
    descriptionKey: "onboarding.highlights.connectors.description",
  },
  {
    icon: Sparkles,
    titleKey: "onboarding.highlights.aiSummary.title",
    descriptionKey: "onboarding.highlights.aiSummary.description",
  },
  {
    icon: ClipboardList,
    titleKey: "onboarding.highlights.resume.title",
    descriptionKey: "onboarding.highlights.resume.description",
  },
];

/** 스텝 3 — "이런 걸 할 수 있어요": 자동 정리·커넥터·AI 요약·재개 브리핑을 아이콘+한 줄로 소개한다.
 * "자랑"이 아니라 "이렇게 쓰면 돼요" 관점의 짧은 카드 리스트(핸드오프 톤 지침). */
function HighlightsStep({ headingRef }: StepHeadingProps) {
  const t = useT();
  return (
    <div className="space-y-4">
      <div>
        <Dialog.Title ref={headingRef} tabIndex={-1} className="text-lg font-semibold">
          {t("onboarding.highlights.title")}
        </Dialog.Title>
        <Dialog.Description className="mt-1 text-sm text-muted-foreground">
          {t("onboarding.highlights.description")}
        </Dialog.Description>
      </div>

      <ul className="grid gap-2 sm:grid-cols-2">
        {HIGHLIGHT_ITEMS.map(({ icon: Icon, titleKey, descriptionKey }) => (
          <li key={titleKey} className="flex gap-2.5 rounded-md border border-border px-3 py-2.5">
            <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <div className="min-w-0">
              {/* 제목-설명을 aria-describedby로 연관(a11y 감사 1.3.1) — 스크린리더가 카드 하나를
                  "제목 + 그 설명"으로 이어 읽는다. */}
              <p id={`highlight-${titleKey}`} className="text-sm font-medium">
                {t(titleKey)}
              </p>
              <p
                className="mt-0.5 text-xs text-muted-foreground"
                aria-describedby={`highlight-${titleKey}`}
              >
                {t(descriptionKey)}
              </p>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** `detect_summary_engine`의 `resolved`("cli:claude" 등)를 사람이 읽는 라벨로 쪼갠다.
 * 판정 자체는 BE(decide_engine)가 하고 여기서는 표시만 한다 — 규칙을 FE에 복제하지 않는다. */
function parseResolvedEngine(
  resolved: string | null,
): { kind: "cli" | "api"; provider: string } | null {
  if (!resolved) return null;
  const [kind, provider] = resolved.split(":");
  if ((kind !== "cli" && kind !== "api") || !provider) return null;
  return { kind, provider };
}

/** 스텝 4 — AI 요약·재개 브리핑 **명시적 opt-in**.
 *
 * 스텝 3(Highlights)이 이 두 기능을 소개하는데 `SummaryConfig::default()`가 `enabled: false`라,
 * 이 스텝이 없으면 사용자가 설정 화면을 스스로 찾아 들어가야만 소개받은 기능을 쓸 수 있었다.
 *
 * **기본 켜짐으로 만들지 않는다** — 켜는 순간 캡처된 발췌가 사용자가 고른 AI로 나가므로 동의는
 * 명시적이어야 한다(ADR-0016 opt-in 요구). 대신 여기서 받아서 "찾게" 만들지는 않는다.
 * 감지 결과를 함께 보여주는 이유는 "켜도 되는 상태인가"를 켜기 전에 알 수 있게 하기 위해서다. */
function SummaryOptInStep({ headingRef }: StepHeadingProps) {
  const t = useT();
  const queryClient = useQueryClient();
  const inputId = "onboarding-summary-enabled";

  const configQuery = useQuery({ queryKey: queryKeys.summaryConfig(), queryFn: getSummaryConfig });
  // 이 컴포넌트는 해당 스텝에서만 마운트되므로 감지(로컬 프로세스 실행)도 그때 한 번만 돈다.
  const statusQuery = useQuery({
    queryKey: queryKeys.summaryEngineStatus(),
    queryFn: detectSummaryEngine,
  });

  const saveMutation = useMutation({
    mutationFn: (config: SummaryConfig) => setSummaryConfig(config),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.summaryConfig() });
    },
  });

  const config = configQuery.data;
  const resolved = parseResolvedEngine(statusQuery.data?.resolved ?? null);

  return (
    <div className="space-y-4 py-2">
      <div>
        <Dialog.Title ref={headingRef} tabIndex={-1} className="text-lg font-semibold">
          {t("onboarding.summaryOptIn.title")}
        </Dialog.Title>
        <Dialog.Description className="mt-1 text-sm text-muted-foreground">
          {t("onboarding.summaryOptIn.description")}
        </Dialog.Description>
      </div>

      {configQuery.isError && <p className="text-sm text-destructive">{t("settings.loadError")}</p>}

      {config && (
        <label
          htmlFor={inputId}
          className="flex items-start gap-2.5 rounded-md border border-border px-3 py-2.5 text-sm font-medium"
        >
          <input
            id={inputId}
            type="checkbox"
            checked={config.enabled}
            onChange={(event) => saveMutation.mutate({ ...config, enabled: event.target.checked })}
            className="mt-0.5 size-4 rounded border-border accent-primary"
          />
          {t("onboarding.summaryOptIn.toggleLabel")}
        </label>
      )}

      {/* 감지 상태 — 켜기 전에 "동작할 상태인가"를 알려준다. aria-live로 비동기 결과를 읽어준다. */}
      <p className="flex items-center gap-1.5 text-xs text-muted-foreground" aria-live="polite">
        {statusQuery.isPending ? (
          t("onboarding.summaryOptIn.detecting")
        ) : (
          <>
            <span
              aria-hidden="true"
              className={cn(
                "size-2 shrink-0 rounded-full",
                resolved ? "bg-emerald-500" : "bg-amber-500",
              )}
            />
            {resolved?.kind === "cli"
              ? t("onboarding.summaryOptIn.cliDetected", { provider: resolved.provider })
              : resolved?.kind === "api"
                ? t("onboarding.summaryOptIn.apiKeyReady", { provider: resolved.provider })
                : t("onboarding.summaryOptIn.noEngine")}
          </>
        )}
      </p>

      <p className="rounded-md border border-dashed border-border px-3 py-2 text-xs text-muted-foreground">
        {t("onboarding.summaryOptIn.privacyNote")}
      </p>

      <p className="min-h-4 text-xs" aria-live="polite">
        {saveMutation.isError && <span className="text-destructive">{t("common.saveError")}</span>}
      </p>
    </div>
  );
}

/** 스텝 5 — backfill은 옵션화하지 않고 항상 동작하는 현 동작을 그대로 안내만 한다(과설계 금지). */
function BackfillStep({ headingRef }: StepHeadingProps) {
  const t = useT();
  return (
    <div className="space-y-3 py-2">
      <div>
        <Dialog.Title ref={headingRef} tabIndex={-1} className="text-lg font-semibold">
          {t("onboarding.backfill.title")}
        </Dialog.Title>
        <Dialog.Description className="mt-1 text-sm text-muted-foreground">
          {t("onboarding.backfill.description")}
        </Dialog.Description>
      </div>
      <p className="rounded-md border border-border bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
        {t("onboarding.backfill.note")}
      </p>
    </div>
  );
}

interface CompleteStepProps extends StepHeadingProps {
  sourceConfigChanged: boolean;
}

/** 스텝 6 — 트레이 상주/단축키 안내 + 온보딩 재열람 안내 + 테마 선택(기존 ThemeSection 임베드) 후
 * "시작하기". */
function CompleteStep({ headingRef, sourceConfigChanged }: CompleteStepProps) {
  const t = useT();
  return (
    <div className="space-y-4 py-2">
      <div>
        <Dialog.Title ref={headingRef} tabIndex={-1} className="text-lg font-semibold">
          {t("onboarding.complete.title")}
        </Dialog.Title>
        <Dialog.Description className="mt-1 text-sm text-muted-foreground">
          {t("onboarding.complete.description")}
        </Dialog.Description>
      </div>

      <ul className="space-y-1.5 text-sm text-muted-foreground">
        <li>{t("onboarding.complete.trayNote")}</li>
        <li>
          <kbd className="rounded-sm border border-border bg-muted px-1.5 py-0.5 font-mono text-[11px]">
            ⌘K
          </kbd>{" "}
          {t("onboarding.complete.searchNote")}
        </li>
        <li>{t("onboarding.complete.replayNote")}</li>
      </ul>

      {/* 피드백 안내 — 서버 전송 없이 mailto로 기본 메일 앱을 연다(아웃바운드 0, lib/feedback.ts).
          첫 사용자와의 1:1 대화 창구이자 프라이버시 포지션의 일부. */}
      <p className="rounded-md border border-border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
        {t("onboarding.complete.feedbackNote")}{" "}
        <button
          type="button"
          onClick={() =>
            void openFeedbackMail(t("feedback.mailSubject"), t("feedback.mailBodyHint"))
          }
          className="font-medium text-foreground underline underline-offset-2 hover:text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {t("onboarding.complete.feedbackLink")}
        </button>
      </p>

      <div className="border-t border-border pt-3">
        <ThemeSection />
      </div>

      {sourceConfigChanged && (
        <p className="rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-600 dark:text-amber-400">
          {t("onboarding.complete.sourceChangedWarning")}
        </p>
      )}
    </div>
  );
}

/**
 * 첫 실행 온보딩(M5, docs/05-ui-ux.md "첫 실행 온보딩") — 5스텝 모달: 환영+프라이버시 선언 →
 * 소스 소개/선택 → 신기능 소개(자동 정리·커넥터·AI 요약·재개 브리핑) → backfill 안내 → 완료(트레이/
 * 단축키/테마). localStorage `logroom-onboarded`가 없으면 최초 마운트 시 자동으로 열리고(store.ts
 * `getInitialOnboardingOpen`), 완료/건너뛰기(Esc·바깥 클릭 포함) 시 플래그가 기록된다(store.ts
 * `setOnboardingOpen`). 설정 다이얼로그 "일반" 섹션의 "온보딩 다시 보기"로 완료 이후에도 재열람할 수
 * 있다 — 이때도 플래그는 그대로 유지된다.
 */
export function OnboardingDialog() {
  const t = useT();
  const onboardingOpen = useAppStore((state) => state.onboardingOpen);
  const setOnboardingOpen = useAppStore((state) => state.setOnboardingOpen);
  const queryClient = useQueryClient();

  const [step, setStep] = useState(0);
  const [localConfig, setLocalConfig] = useState<CaptureConfig | null>(null);
  const [sourceConfigChanged, setSourceConfigChanged] = useState(false);
  const headingRef = useRef<HTMLHeadingElement>(null);

  const { data, isLoading, isError } = useQuery({
    queryKey: queryKeys.captureConfig(),
    queryFn: getCaptureConfig,
    enabled: onboardingOpen,
  });

  const saveMutation = useMutation({
    mutationFn: (config: CaptureConfig) => setCaptureConfig(config),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.captureConfig() });
    },
  });

  useEffect(() => {
    if (onboardingOpen && data) {
      setLocalConfig(withConfigDefaults(data.config));
    }
  }, [onboardingOpen, data]);

  // 다이얼로그를 열 때마다(최초 실행이든 설정에서 재열람이든) 1스텝부터 다시 시작한다.
  useEffect(() => {
    if (!onboardingOpen) return;
    setStep(0);
    setSourceConfigChanged(false);
  }, [onboardingOpen]);

  // 스텝 전환 시 새 스텝의 heading으로 포커스를 옮겨 스크린리더에 진행 상태를 알린다. step은 콜백
  // 본문에서 직접 읽지 않지만, 스텝이 바뀔 때마다 새로 마운트되는 heading에 재포커스하기 위해
  // 의도적으로 의존성에 포함한다.
  // biome-ignore lint/correctness/useExhaustiveDependencies: step 변경 자체가 재포커스 트리거다.
  useEffect(() => {
    if (!onboardingOpen) return;
    headingRef.current?.focus();
  }, [step, onboardingOpen]);

  function handleToggleSource(source: CaptureSource, enabled: boolean) {
    setLocalConfig((prev) => {
      if (!prev) return prev;
      const key = SOURCE_CONFIG_KEYS[source];
      const next: CaptureConfig = { ...prev, [key]: { ...prev[key], enabled } };
      saveMutation.mutate(next);
      return next;
    });
    setSourceConfigChanged(true);
  }

  function goNext() {
    setStep((current) => Math.min(current + 1, TOTAL_STEPS - 1));
  }

  function goPrev() {
    setStep((current) => Math.max(current - 1, 0));
  }

  const isLastStep = step === TOTAL_STEPS - 1;

  return (
    <Dialog.Root open={onboardingOpen} onOpenChange={setOnboardingOpen}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-background/80 backdrop-blur-sm" />
        <Dialog.Content className="fixed top-1/2 left-1/2 z-50 flex h-[32rem] max-h-[90vh] w-full max-w-xl -translate-x-1/2 -translate-y-1/2 flex-col overflow-hidden rounded-lg border border-border bg-popover text-popover-foreground shadow-lg">
          <header className="flex shrink-0 items-center justify-between gap-2 border-b border-border px-6 py-3">
            <p className="text-xs font-semibold tracking-wide text-muted-foreground uppercase">
              {t("onboarding.header")}
            </p>
            {/* 진행 표시기 — fieldset은 group role을 갖는 시맨틱 요소라 그룹화에 적합하다(biome
                useSemanticElements가 role="group" div보다 이 쪽을 권장). 스텝 전환 시 진행 상태를
                스크린리더에 알리도록 아래 카운트에 aria-live 적용(a11y 감사 반영). */}
            <fieldset
              className="flex items-center gap-2 border-0 p-0"
              aria-label={t("onboarding.progressAriaLabel", {
                current: step + 1,
                total: TOTAL_STEPS,
              })}
            >
              <div className="flex items-center gap-1" aria-hidden="true">
                {Array.from({ length: TOTAL_STEPS }, (_, index) => (
                  <span
                    // biome-ignore lint/suspicious/noArrayIndexKey: 스텝 개수가 고정된 정적 인디케이터라 재정렬이 없다.
                    key={index}
                    className={cn(
                      "h-1.5 w-4 rounded-full",
                      index <= step ? "bg-primary" : "bg-muted",
                    )}
                  />
                ))}
              </div>
              <span
                className="text-xs text-muted-foreground tabular-nums"
                aria-live="polite"
                aria-atomic="true"
              >
                {step + 1}/{TOTAL_STEPS}
              </span>
            </fieldset>
          </header>

          <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
            {step === 0 && <WelcomeStep headingRef={headingRef} />}
            {step === 1 && (
              <SourcesStep
                headingRef={headingRef}
                config={localConfig}
                resolvedRoots={data?.resolvedRoots ?? []}
                isLoading={isLoading}
                isError={isError}
                saveError={saveMutation.isError}
                onToggle={handleToggleSource}
              />
            )}
            {step === 2 && <HighlightsStep headingRef={headingRef} />}
            {step === 3 && <SummaryOptInStep headingRef={headingRef} />}
            {step === 4 && <BackfillStep headingRef={headingRef} />}
            {step === 5 && (
              <CompleteStep headingRef={headingRef} sourceConfigChanged={sourceConfigChanged} />
            )}
          </div>

          <footer className="flex shrink-0 items-center justify-between gap-2 border-t border-border px-6 py-3">
            <Button type="button" variant="ghost" size="sm" onClick={goPrev} disabled={step === 0}>
              {t("onboarding.previous")}
            </Button>
            {isLastStep ? (
              <Button
                type="button"
                variant="default"
                size="sm"
                onClick={() => setOnboardingOpen(false)}
              >
                {t("onboarding.startButton")}
              </Button>
            ) : (
              <Button type="button" variant="default" size="sm" onClick={goNext}>
                {t("onboarding.next")}
              </Button>
            )}
          </footer>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
