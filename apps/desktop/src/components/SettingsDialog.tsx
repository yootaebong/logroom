import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Database, Plug, Radar, Settings2, Sparkles, Trash2, X } from "lucide-react";
import { Dialog } from "radix-ui";
import type { ComponentType, KeyboardEvent } from "react";
import { useEffect, useRef, useState } from "react";
import { ReviewProfileForm } from "@/components/ReviewPanel";
import { Button } from "@/components/ui/button";
import type { Translate, UiKey } from "@/i18n";
import { resolveLocale, useT } from "@/i18n";
import {
  checkUpdate,
  countEventsBefore,
  deleteEventsBefore,
  disableAutostart,
  enableAutostart,
  exportData,
  getAppVersion,
  getAutostartEnabled,
  getCaptureConfig,
  getCheckUpdatesEnabled,
  getDbStats,
  getReviewProfile,
  getSummaryConfig,
  installUpdate,
  QUERY_KEY_DIGEST,
  QUERY_KEY_EVENTS_BY_DAY,
  QUERY_KEY_SEARCH,
  QUERY_KEY_STREAM,
  queryKeys,
  restartApp,
  setAppLocale,
  setCaptureConfig,
  setCheckUpdatesEnabled,
  setSummaryConfig,
  vacuumDb,
} from "@/lib/api";
import { openFeedbackMail } from "@/lib/feedback";
import type {
  ApiProvider,
  BodyPolicy,
  CaptureConfig,
  CaptureSource,
  CaptureSourceConfig,
  CliProvider,
  GithubConfig,
  LinearConfig,
  NotionAccount,
  NotionConfig,
  ResolvedCaptureRoot,
  SlackConfig,
  SummaryConfig,
  SummaryEngine,
  UpdateProgress,
} from "@/lib/types";
import { cn } from "@/lib/utils";
import type { AppView, Locale, Theme } from "@/store";
import { useAppStore } from "@/store";

/** 서버 응답에 bodyPolicy가 없을 때(구버전 응답 등) 폴백 값. Rust `BodyPolicy::default()`와 동일. */
const DEFAULT_BODY_POLICY: BodyPolicy = "essential";

/** 서버 응답에 excludeProjects가 없을 때(구버전 응답 등) 폴백 값. Rust `CaptureConfig::default()`와 동일. */
const DEFAULT_EXCLUDE_PROJECTS: string[] = [];

/** 서버 응답에 slack 설정이 없을 때(구버전 응답 등) 폴백 값. Rust `SlackConfig::default()`와 동일. */
const DEFAULT_SLACK_CONFIG: SlackConfig = { token: null, enabled: false, pollMinutes: 5 };

/** 서버 응답에 github 설정이 없을 때(구버전 응답 등) 폴백 값. Rust `GithubConfig::default()`와 동일. */
const DEFAULT_GITHUB_CONFIG: GithubConfig = { accounts: [], enabled: false, pollMinutes: 5 };

/** 서버 응답에 linear 설정이 없을 때(구버전 응답 등) 폴백 값. Rust `LinearConfig::default()`와 동일. */
const DEFAULT_LINEAR_CONFIG: LinearConfig = { accounts: [], enabled: false, pollMinutes: 5 };

/** 서버 응답에 notion 설정이 없을 때(구버전 응답 등) 폴백 값. Rust `NotionConfig::default()`와 동일. */
const DEFAULT_NOTION_CONFIG: NotionConfig = { accounts: [], enabled: false, pollMinutes: 5 };

/** Slack 토큰 발급 가이드 URL(설정 다이얼로그 "커넥터" 섹션 안내 링크, docs/08-connectors.md). */
const SLACK_TOKEN_GUIDE_URL = "https://api.slack.com/apps";

/** GitHub PAT 발급 가이드 URL — fine-grained token 생성 페이지로 바로 연결한다
 * (Settings → Developer settings → Fine-grained personal access tokens, docs/08-connectors.md). */
const GITHUB_TOKEN_GUIDE_URL = "https://github.com/settings/personal-access-tokens/new";

/** Linear Personal API Key 발급 가이드 URL — Settings → Security & access → Personal API keys
 * (docs/08-connectors.md). 실측 미확인 — 완료 보고 "수동 확인 포인트" 참고. */
const LINEAR_TOKEN_GUIDE_URL = "https://linear.app/settings/api";

/** Notion 개인 액세스 토큰(PAT) 발급 화면 — Developer portal의 Personal access tokens 목록. 회사
 * 워크스페이스 구성원도 쓸 수 있는 방식이라(internal integration은 소유자만 발급) 이걸 기본 안내로 둔다. */
const NOTION_TOKEN_GUIDE_URL = "https://www.notion.so/developers/tokens";
/** 이름도 워크스페이스명도 없는 Notion 계정의 표시 이름 — 서버 `DEFAULT_PROJECT`와 같은 값. */
const NOTION_DEFAULT_PROJECT = "Notion";

/** 서버 응답에 소스 설정이 없거나 `enabled`가 빠져 있을 때(구버전 응답 등) 폴백. Rust
 * `SourceConfig::default()`와 동일 — bodyPolicy 유실 버그(#7) 재발 방지 폴백 패턴. */
function withSourceDefaults(config: CaptureSourceConfig | undefined): CaptureSourceConfig {
  return {
    extraRoots: config?.extraRoots ?? [],
    useDefaults: config?.useDefaults ?? true,
    enabled: config?.enabled ?? true,
  };
}

/** 서버 응답에 slack 설정이 없거나 일부 필드가 빠져 있을 때(구버전 응답 등) 폴백. `withSourceDefaults`와
 * 동일한 목적(#7 bodyPolicy 유실 버그 재발 방지 패턴). */
function withSlackDefaults(config: SlackConfig | undefined): SlackConfig {
  return {
    token: config?.token ?? DEFAULT_SLACK_CONFIG.token,
    enabled: config?.enabled ?? DEFAULT_SLACK_CONFIG.enabled,
    pollMinutes: config?.pollMinutes ?? DEFAULT_SLACK_CONFIG.pollMinutes,
  };
}

/** 서버 응답에 github 설정이 없거나 일부 필드가 빠져 있을 때(구버전 응답 등) 폴백. `withSlackDefaults`와
 * 동일한 목적(#7 bodyPolicy 유실 버그 재발 방지 패턴). */
function withGithubDefaults(config: GithubConfig | undefined): GithubConfig {
  return {
    accounts: config?.accounts ?? DEFAULT_GITHUB_CONFIG.accounts,
    enabled: config?.enabled ?? DEFAULT_GITHUB_CONFIG.enabled,
    pollMinutes: config?.pollMinutes ?? DEFAULT_GITHUB_CONFIG.pollMinutes,
  };
}

/** 서버 응답에 linear 설정이 없거나 일부 필드가 빠져 있을 때(구버전 응답 등) 폴백. `withGithubDefaults`와
 * 동일한 목적(#7 bodyPolicy 유실 버그 재발 방지 패턴). */
function withLinearDefaults(config: LinearConfig | undefined): LinearConfig {
  return {
    accounts: config?.accounts ?? DEFAULT_LINEAR_CONFIG.accounts,
    enabled: config?.enabled ?? DEFAULT_LINEAR_CONFIG.enabled,
    pollMinutes: config?.pollMinutes ?? DEFAULT_LINEAR_CONFIG.pollMinutes,
  };
}

/** 서버 응답에 notion 설정이 없거나 일부 필드가 빠져 있을 때(구버전 응답 등) 폴백. `withLinearDefaults`와
 * 동일한 목적(#7 bodyPolicy 유실 버그 재발 방지 패턴). */
function withNotionDefaults(config: NotionConfig | undefined): NotionConfig {
  return {
    accounts: config?.accounts ?? DEFAULT_NOTION_CONFIG.accounts,
    enabled: config?.enabled ?? DEFAULT_NOTION_CONFIG.enabled,
    pollMinutes: config?.pollMinutes ?? DEFAULT_NOTION_CONFIG.pollMinutes,
  };
}

/** `get_capture_config` 응답을 로컬 편집 상태로 옮길 때 구버전 응답의 누락 필드를 폴백값으로
 * 채운다 — 폴백 없이 그대로 저장하면 새 필드가 빈 값으로 덮여써질 위험이 있다(#7 교훈).
 * OnboardingDialog.tsx의 소스 선택 스텝도 이 함수를 재사용한다(동일한 config shape). */
export function withConfigDefaults(config: CaptureConfig): CaptureConfig {
  return {
    ...config,
    claudeCode: withSourceDefaults(config.claudeCode),
    kiroCli: withSourceDefaults(config.kiroCli),
    bodyPolicy: config.bodyPolicy ?? DEFAULT_BODY_POLICY,
    excludeProjects: config.excludeProjects ?? DEFAULT_EXCLUDE_PROJECTS,
    slack: withSlackDefaults(config.slack),
    github: withGithubDefaults(config.github),
    linear: withLinearDefaults(config.linear),
    notion: withNotionDefaults(config.notion),
  };
}

/** 저장 정책 라디오 옵션. value는 CaptureConfig.bodyPolicy와 1:1 대응. */
const BODY_POLICY_OPTIONS: Array<{ value: BodyPolicy; labelKey: UiKey; descriptionKey: UiKey }> = [
  {
    value: "essential",
    labelKey: "settings.bodyPolicy.essential.label",
    descriptionKey: "settings.bodyPolicy.essential.description",
  },
  {
    value: "full",
    labelKey: "settings.bodyPolicy.full.label",
    descriptionKey: "settings.bodyPolicy.full.description",
  },
];

/** CaptureConfig의 소스별 키 — CaptureSource와 1:1 매핑(camelCase 필드명 차이만 흡수).
 * OnboardingDialog.tsx의 소스 선택 스텝도 이 매핑을 재사용한다. */
export const SOURCE_CONFIG_KEYS = {
  claude_code: "claudeCode",
  kiro_cli: "kiroCli",
} as const satisfies Record<CaptureSource, keyof CaptureConfig>;

/** OnboardingDialog.tsx의 소스 선택 스텝도 이 라벨을 재사용한다. */
export const SOURCE_LABELS: Record<CaptureSource, string> = {
  claude_code: "Claude Code",
  kiro_cli: "Kiro CLI",
};

interface DialogOpenProps {
  /** 설정 다이얼로그가 열려있을 때만 조회한다(다른 useQuery들과 동일한 `enabled` 패턴). */
  enabled: boolean;
}

/** 테마 라디오 옵션 — value는 store.Theme과 1:1 대응. 스와치 hex는 index.css의 각 테마
 * `--background`/`--sidebar-bg` 대표값을 미러링한 하드코딩 값이다(다이얼로그는 항상 "현재
 * 활성 테마" 아래에서 렌더되므로 다른 두 테마의 CSS 변수는 이 컨텍스트에서 읽을 수 없다). */
const THEME_OPTIONS: Array<{
  value: Theme;
  labelKey: UiKey;
  backgroundSwatch: string;
  sidebarSwatch: string;
}> = [
  {
    value: "dark",
    labelKey: "settings.theme.dark",
    backgroundSwatch: "#0a0a0a",
    sidebarSwatch: "#141218",
  },
  {
    value: "light",
    labelKey: "settings.theme.light",
    backgroundSwatch: "#ffffff",
    sidebarSwatch: "#f4f4f5",
  },
  {
    value: "warm",
    labelKey: "settings.theme.warm",
    backgroundSwatch: "#faf8f5",
    sidebarSwatch: "#f3efe9",
  },
];

/** 테마 선택 — 저장 버튼과 무관하게 선택 즉시 적용된다(store.setTheme → App.tsx가 `<html>`
 * 클래스 동기화 + localStorage persist). `BodyPolicySection`의 라디오 패턴을 재사용한다.
 * OnboardingDialog.tsx의 완료 스텝도 이 컴포넌트를 그대로 임베드한다. */
export function ThemeSection() {
  const t = useT();
  const theme = useAppStore((state) => state.theme);
  const setTheme = useAppStore((state) => state.setTheme);

  return (
    <fieldset className="space-y-2">
      <legend className="text-sm font-semibold">{t("settings.theme.legend")}</legend>
      <div role="radiogroup" aria-label={t("settings.theme.legend")} className="space-y-2">
        {THEME_OPTIONS.map((option) => {
          const inputId = `theme-${option.value}`;
          return (
            <label key={option.value} htmlFor={inputId} className="flex items-center gap-2 text-sm">
              <input
                id={inputId}
                type="radio"
                name="theme"
                value={option.value}
                checked={theme === option.value}
                onChange={() => setTheme(option.value)}
                className="size-4 shrink-0 accent-primary"
              />
              <span className="flex shrink-0 items-center gap-1" aria-hidden="true">
                <span
                  className="size-3.5 rounded-full border border-border"
                  style={{ backgroundColor: option.backgroundSwatch }}
                />
                <span
                  className="size-3.5 rounded-full border border-border"
                  style={{ backgroundColor: option.sidebarSwatch }}
                />
              </span>
              {t(option.labelKey)}
            </label>
          );
        })}
      </div>
    </fieldset>
  );
}

/** 최초 뷰 라디오 옵션 — value는 store.AppView와 1:1 대응. 라벨은 사이드바 뷰 항목 키를 재사용한다
 * (동일한 뷰 이름이라 별도 키 불필요). */
const DEFAULT_VIEW_OPTIONS: Array<{ value: AppView; labelKey: UiKey }> = [
  { value: "digest", labelKey: "sidebar.view.digest" },
  { value: "timeline", labelKey: "sidebar.view.timeline" },
  { value: "summary", labelKey: "sidebar.view.summary" },
];

/** 앱 시작 시 열 최초 뷰 선택(사용자 요구) — 테마와 동일한 localStorage persist 라디오. 지금 화면
 * (`view`)을 바꾸지 않고 "다음 실행 시 열 뷰"(`defaultView`)만 저장한다. */
function DefaultViewSection() {
  const t = useT();
  const defaultView = useAppStore((state) => state.defaultView);
  const setDefaultView = useAppStore((state) => state.setDefaultView);

  return (
    <fieldset className="space-y-2">
      <legend className="text-sm font-semibold">{t("settings.defaultView.legend")}</legend>
      <p className="text-xs text-muted-foreground">{t("settings.defaultView.help")}</p>
      <div role="radiogroup" aria-label={t("settings.defaultView.legend")} className="space-y-2">
        {DEFAULT_VIEW_OPTIONS.map((option) => {
          const inputId = `default-view-${option.value}`;
          return (
            <label key={option.value} htmlFor={inputId} className="flex items-center gap-2 text-sm">
              <input
                id={inputId}
                type="radio"
                name="default-view"
                value={option.value}
                checked={defaultView === option.value}
                onChange={() => setDefaultView(option.value)}
                className="size-4 shrink-0 accent-primary"
              />
              {t(option.labelKey)}
            </label>
          );
        })}
      </div>
    </fieldset>
  );
}

/** 언어 라디오 옵션 — value는 store.Locale과 1:1 대응. "system"/"en"/"ko" 3종(ThemeSection과
 * 동일한 라디오 패턴). 언어 자체의 이름("한국어"/"English")은 어느 UI 언어에서 봐도 자기 언어의
 * 자연스러운 표기를 쓰는 게 관례라 번역하지 않는다 — "시스템"만 번역 대상이다. */
const LOCALE_OPTIONS: Array<{ value: Locale; label: string; labelKey?: UiKey }> = [
  { value: "system", label: "", labelKey: "settings.language.system" },
  { value: "ko", label: "한국어" },
  { value: "en", label: "English" },
];

/** 언어 선택 — 테마와 동일하게 선택 즉시 적용된다(store.setLocale → useT 구독 컴포넌트 즉시
 * 리렌더). 트레이 메뉴 라벨은 별도로 Rust config에 동기화되며 재시작 후에만 반영된다(App.tsx의
 * setAppLocale 동기화 effect 참고) — 그 사실을 안내 문구로 고지한다. */
function LanguageSection() {
  const t = useT();
  const locale = useAppStore((state) => state.locale);
  const setLocale = useAppStore((state) => state.setLocale);

  function handleChange(next: Locale) {
    setLocale(next);
    void setAppLocale(resolveLocale(next)).catch(() => {
      // 트레이 라벨 동기화 실패는 부가 효과 — 다음 재시작 전까지는 어차피 반영되지 않는다.
    });
  }

  return (
    <fieldset className="space-y-2">
      <legend className="text-sm font-semibold">{t("settings.language.heading")}</legend>
      <div role="radiogroup" aria-label={t("settings.language.heading")} className="space-y-2">
        {LOCALE_OPTIONS.map((option) => {
          const inputId = `locale-${option.value}`;
          return (
            <label key={option.value} htmlFor={inputId} className="flex items-center gap-2 text-sm">
              <input
                id={inputId}
                type="radio"
                name="locale"
                value={option.value}
                checked={locale === option.value}
                onChange={() => handleChange(option.value)}
                className="size-4 shrink-0 accent-primary"
              />
              {option.labelKey ? t(option.labelKey) : option.label}
            </label>
          );
        })}
      </div>
      <p className="text-xs text-muted-foreground">{t("settings.language.restartNote")}</p>
    </fieldset>
  );
}

/** 로그인 시 자동 실행(macOS LaunchAgent) 토글. `config.json`이 아니라 OS에 직접 등록/해제하므로
 * 저장 버튼과 무관하게 체크 즉시 적용된다. 개발 모드에서는 등록 대상 실행 파일이 없어 항상
 * 비활성으로 남는다(안내 문구로 정직하게 고지). `GeneralSection`의 "일반" 섹션 안에 렌더된다. */
function AutostartToggle({ enabled: dialogOpen }: DialogOpenProps) {
  const t = useT();
  const queryClient = useQueryClient();
  const autostartId = "autostart-enabled";

  const { data: enabled, isError: isQueryError } = useQuery({
    queryKey: queryKeys.autostart(),
    queryFn: getAutostartEnabled,
    enabled: dialogOpen,
  });

  const toggleMutation = useMutation({
    mutationFn: (next: boolean) => (next ? enableAutostart() : disableAutostart()),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.autostart() });
    },
  });

  return (
    <div className="space-y-2">
      <label htmlFor={autostartId} className="flex items-center gap-2 text-sm">
        <input
          id={autostartId}
          type="checkbox"
          checked={enabled ?? false}
          onChange={(event) => toggleMutation.mutate(event.target.checked)}
          disabled={enabled === undefined || toggleMutation.isPending}
          className="size-4 rounded border-border accent-primary"
        />
        {t("settings.autostart.label")}
      </label>
      <p className="text-xs text-muted-foreground">{t("settings.autostart.devNote")}</p>
      <p className="min-h-4 text-xs" aria-live="polite">
        {isQueryError && (
          <span className="text-destructive">{t("settings.autostart.loadError")}</span>
        )}
        {toggleMutation.isError && (
          <span className="text-destructive">{t("settings.autostart.toggleError")}</span>
        )}
      </p>
    </div>
  );
}

/** 업데이트 다운로드 진행률 문구(바이트 → MB, 총량 모르면 다운로드된 양만). */
function formatUpdateProgress(progress: UpdateProgress, t: Translate): string {
  const downloadedMb = formatMegabytes(progress.downloaded);
  if (progress.total === null) {
    return t("settings.update.downloading", { downloaded: downloadedMb });
  }
  return t("settings.update.downloadingWithTotal", {
    downloaded: downloadedMb,
    total: formatMegabytes(progress.total),
  });
}

/** 현재 버전 표시 · 수동/자동 업데이트 확인 · 발견 시 설치 진행 → 재시작(M5, ADR-0013).
 * `GeneralSection`의 "일반" 섹션 안에 렌더된다. 발견된 업데이트는 store(`updateAvailable`)에
 * 저장해 헤더 배지와 같은 값을 공유한다(단일 소스, App.tsx의 `update-available` 리스너와 동기화). */
function UpdateSection({ enabled: dialogOpen }: DialogOpenProps) {
  const t = useT();
  const queryClient = useQueryClient();
  const updateAvailable = useAppStore((state) => state.updateAvailable);
  const setUpdateAvailable = useAppStore((state) => state.setUpdateAvailable);
  const [progress, setProgress] = useState<UpdateProgress | null>(null);
  const [installed, setInstalled] = useState(false);
  const autoCheckId = "auto-check-updates-enabled";

  const { data: version } = useQuery({
    queryKey: queryKeys.appVersion(),
    queryFn: getAppVersion,
    enabled: dialogOpen,
  });

  const { data: autoCheckEnabled, isError: isAutoCheckQueryError } = useQuery({
    queryKey: queryKeys.checkUpdatesEnabled(),
    queryFn: getCheckUpdatesEnabled,
    enabled: dialogOpen,
  });

  const toggleAutoCheckMutation = useMutation({
    mutationFn: (next: boolean) => setCheckUpdatesEnabled(next),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.checkUpdatesEnabled() });
    },
  });

  const checkMutation = useMutation({
    mutationFn: checkUpdate,
    onSuccess: (result) => setUpdateAvailable(result),
  });

  const installMutation = useMutation({
    mutationFn: installUpdate,
    onMutate: () => {
      setProgress(null);
      setInstalled(false);
    },
    onSuccess: () => setInstalled(true),
  });

  useEffect(() => {
    if (!dialogOpen) return;
    const unlistenPromise = listen<UpdateProgress>("update-progress", (event) => {
      setProgress(event.payload);
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [dialogOpen]);

  return (
    <div className="space-y-3 border-t border-border pt-3">
      <div className="flex items-center justify-between text-xs">
        <span className="text-muted-foreground">{t("settings.update.currentVersion")}</span>
        <span className="font-mono">{version ? `v${version}` : t("common.checking")}</span>
      </div>

      <label htmlFor={autoCheckId} className="flex items-center gap-2 text-sm">
        <input
          id={autoCheckId}
          type="checkbox"
          checked={autoCheckEnabled ?? true}
          onChange={(event) => toggleAutoCheckMutation.mutate(event.target.checked)}
          disabled={autoCheckEnabled === undefined || toggleAutoCheckMutation.isPending}
          className="size-4 rounded border-border accent-primary"
        />
        {t("settings.update.autoCheckLabel")}
      </label>

      <div className="flex items-center gap-2">
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => checkMutation.mutate()}
          disabled={checkMutation.isPending || installMutation.isPending}
        >
          {checkMutation.isPending ? t("common.checking") : t("settings.update.checkButton")}
        </Button>
        {updateAvailable && !installed && (
          <Button
            type="button"
            variant="default"
            size="sm"
            onClick={() => installMutation.mutate()}
            disabled={installMutation.isPending}
          >
            {installMutation.isPending
              ? t("settings.update.installing")
              : t("settings.update.installButton", { version: updateAvailable.version })}
          </Button>
        )}
        {installed && (
          <Button type="button" variant="outline" size="sm" onClick={() => restartApp()}>
            {t("common.restartNow")}
          </Button>
        )}
      </div>

      <p className="min-h-4 text-xs" aria-live="polite">
        {isAutoCheckQueryError && (
          <span className="text-destructive">{t("settings.update.autoCheckLoadError")}</span>
        )}
        {toggleAutoCheckMutation.isError && (
          <span className="text-destructive">{t("settings.update.autoCheckToggleError")}</span>
        )}
        {checkMutation.isError && (
          <span className="text-destructive">{t("settings.update.checkError")}</span>
        )}
        {checkMutation.isSuccess && !updateAvailable && (
          <span className="text-muted-foreground">{t("settings.update.upToDate")}</span>
        )}
        {checkMutation.isSuccess && updateAvailable && !installed && (
          <span className="text-muted-foreground">
            {t("settings.update.newVersionAvailable", { version: updateAvailable.version })}
            {updateAvailable.notes ? ` — ${updateAvailable.notes}` : ""}
          </span>
        )}
        {installMutation.isPending && progress && (
          <span className="text-muted-foreground">{formatUpdateProgress(progress, t)}</span>
        )}
        {installMutation.isError && (
          <span className="text-destructive">{t("settings.update.installError")}</span>
        )}
        {installed && (
          <span className="text-muted-foreground">{t("settings.update.installedNotice")}</span>
        )}
      </p>
    </div>
  );
}

/** 첫 실행 온보딩 다이얼로그를 다시 연다(M5 온보딩). 설정 다이얼로그를 먼저 닫아 두 모달이
 * 겹치지 않게 한다 — 온보딩 완료 플래그(localStorage)는 그대로 유지되고 재열람만 트리거한다. */
function ReplayOnboardingAction() {
  const t = useT();
  const setSettingsOpen = useAppStore((state) => state.setSettingsOpen);
  const setOnboardingOpen = useAppStore((state) => state.setOnboardingOpen);

  function handleClick() {
    setSettingsOpen(false);
    setOnboardingOpen(true);
  }

  return (
    <div className="flex items-center justify-between gap-2 border-t border-border pt-3">
      <div className="min-w-0">
        <h4 className="text-sm font-medium">{t("settings.onboarding.heading")}</h4>
        <p className="text-xs text-muted-foreground">{t("settings.onboarding.description")}</p>
      </div>
      <Button type="button" variant="outline" size="sm" onClick={handleClick}>
        {t("settings.onboarding.replayButton")}
      </Button>
    </div>
  );
}

/** 피드백 보내기(설정 일반) — 앱은 서버로 직접 전송하지 않고 mailto로 기본 메일 앱을 연다(아웃바운드
 * 0 유지, lib/feedback.ts 참고). "우리는 당신 데이터를 서버에 쌓지 않는다, 피드백조차"라는
 * 프라이버시 포지션의 일부. */
function FeedbackAction() {
  const t = useT();
  return (
    <div className="flex items-center justify-between gap-2 border-t border-border pt-3">
      <div className="min-w-0">
        <h4 className="text-sm font-medium">{t("settings.feedback.heading")}</h4>
        <p className="text-xs text-muted-foreground">{t("settings.feedback.description")}</p>
      </div>
      <Button
        type="button"
        variant="outline"
        size="sm"
        onClick={() => void openFeedbackMail(t("feedback.mailSubject"), t("feedback.mailBodyHint"))}
      >
        {t("settings.feedback.button")}
      </Button>
    </div>
  );
}

/** 요약 모드 라디오 옵션 — value는 SummaryConfig.engine과 1:1 대응. 실제 CLI/API 프로바이더는
 * 이 값과 별개(아래 CLI_PROVIDER_OPTIONS/API_PROVIDER_OPTIONS)라 문구를 프로바이더 이름이 아니라
 * 모드 자체로 일반화한다("Claude CLI" → "CLI"). */
const SUMMARY_ENGINE_OPTIONS: Array<{ value: SummaryEngine; labelKey: UiKey }> = [
  { value: "auto", labelKey: "settings.summary.engine.auto" },
  { value: "cli", labelKey: "settings.summary.engine.cli" },
  { value: "api", labelKey: "settings.summary.engine.api" },
];

/** CLI 프로바이더 select 옵션. 브랜드명이라 번역하지 않는다(i18n 정책 — 고유명사 하드코딩 허용). */
const CLI_PROVIDER_OPTIONS: Array<{ value: CliProvider; label: string }> = [
  { value: "claude", label: "Claude" },
  { value: "gemini", label: "Gemini" },
  { value: "codex", label: "Codex" },
];

/** API 프로바이더 select 옵션. 브랜드명이라 번역하지 않는다. */
const API_PROVIDER_OPTIONS: Array<{ value: ApiProvider; label: string }> = [
  { value: "anthropic", label: "Anthropic" },
  { value: "openai", label: "OpenAI" },
  { value: "gemini", label: "Gemini" },
];

/** `value`가 CLI_PROVIDER_OPTIONS의 값 중 하나인지(select onChange에서 `as` 캐스팅 없이 좁히기 위함). */
function isCliProvider(value: string): value is CliProvider {
  return CLI_PROVIDER_OPTIONS.some((option) => option.value === value);
}

/** `value`가 API_PROVIDER_OPTIONS의 값 중 하나인지. */
function isApiProvider(value: string): value is ApiProvider {
  return API_PROVIDER_OPTIONS.some((option) => option.value === value);
}

/** CLI 모델 드롭다운 후보 — 프로바이더별 CLI가 해석하는 안정 alias(자유 텍스트였을 때의 오타/플래그
 * 주입 여지를 UI 단에서 제거, BE validate_cli_model과 이중 방어). 새 모델 출시를 따라가도록 프로바이더
 * 공통으로 "직접 입력" 옵션도 제공한다. 튜플 타입(`[string, ...string[]]`)은 최소 1개 원소를 보장해
 * `[0]` 인덱싱이 `noUncheckedIndexedAccess`에서도 `string | undefined`가 아닌 `string`이 되게 한다. */
const CLI_MODEL_OPTIONS_BY_PROVIDER: Record<CliProvider, readonly [string, ...string[]]> = {
  claude: ["haiku", "sonnet", "opus"],
  gemini: ["gemini-2.5-flash", "gemini-2.5-pro"],
  codex: ["gpt-5", "gpt-5-mini"],
};

/** API 모델 드롭다운 후보(프로바이더별) — 새 모델 출시를 따라가도록 "직접 입력" 옵션을 함께 제공한다.
 * 튜플 타입에 대한 설명은 [`CLI_MODEL_OPTIONS_BY_PROVIDER`] 참고. */
const API_MODEL_OPTIONS_BY_PROVIDER: Record<ApiProvider, readonly [string, ...string[]]> = {
  anthropic: ["claude-haiku-4-5", "claude-sonnet-4-5"],
  openai: ["gpt-5-mini", "gpt-5"],
  gemini: ["gemini-2.5-flash", "gemini-2.5-pro"],
};

/** 모델 select에서 "직접 입력"을 나타내는 센티널 값(실제 모델명과 충돌하지 않는 값). */
const CUSTOM_MODEL_VALUE = "__custom__";

/** "직접 입력" 텍스트 입력의 placeholder(프로바이더별 모델명 형태 힌트). */
const CLI_MODEL_CUSTOM_PLACEHOLDER: Record<CliProvider, string> = {
  claude: "sonnet-...",
  gemini: "gemini-...",
  codex: "gpt-...",
};
const API_MODEL_CUSTOM_PLACEHOLDER: Record<ApiProvider, string> = {
  anthropic: "claude-...",
  openai: "gpt-...",
  gemini: "gemini-...",
};

/** apiProvider → SummaryConfig의 실제 키 필드 매핑(3개 키 중 현재 선택된 프로바이더의 것만 노출). */
const API_KEY_FIELD_BY_PROVIDER = {
  anthropic: "anthropicApiKey",
  openai: "openaiApiKey",
  gemini: "geminiApiKey",
} as const satisfies Record<ApiProvider, keyof SummaryConfig>;

/** API 키 입력의 placeholder(프로바이더별 키 형태 힌트, 아직 키가 없을 때만 표시). */
const API_KEY_PLACEHOLDER_BY_PROVIDER: Record<ApiProvider, string> = {
  anthropic: "sk-ant-...",
  openai: "sk-proj-...",
  gemini: "AIzaSy...",
};

interface ModelPickerProps {
  id: string;
  labelKey: UiKey;
  value: string;
  options: readonly string[];
  disabled: boolean;
  customPlaceholder: string;
  onChange: (value: string) => void;
}

/** 모델 드롭다운 + "직접 입력" 텍스트 입력(선택 시에만 노출) — CLI/API 모델 선택에서 공유하는 UI
 * (기존에는 apiModel에만 있던 "직접 입력"을 cliModel에도 동일하게 적용하며 중복 방지를 위해 추출). */
function ModelPicker({
  id,
  labelKey,
  value,
  options,
  disabled,
  customPlaceholder,
  onChange,
}: ModelPickerProps) {
  const t = useT();
  const isCustom = !options.includes(value);
  return (
    <div className="flex-1">
      <label htmlFor={id} className="text-xs font-medium text-muted-foreground">
        {t(labelKey)}
      </label>
      <select
        id={id}
        value={isCustom ? CUSTOM_MODEL_VALUE : value}
        disabled={disabled}
        onChange={(event) =>
          // "직접 입력" 선택 시 기존 값을 비워 아래 텍스트 입력을 연다(빈 값으로 초기화하면 저장 전
          // 상태가 invalid해짐 — 실제로는 텍스트 입력에 바로 값을 채우게 유도).
          onChange(event.target.value === CUSTOM_MODEL_VALUE ? "" : event.target.value)
        }
        className="mt-1 w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
      >
        {options.map((model) => (
          <option key={model} value={model}>
            {model}
          </option>
        ))}
        <option value={CUSTOM_MODEL_VALUE}>{t("settings.summary.modelCustomOption")}</option>
      </select>
      {isCustom && (
        <input
          type="text"
          aria-label={t("settings.summary.modelCustomAriaLabel")}
          value={value}
          disabled={disabled}
          placeholder={customPlaceholder}
          onChange={(event) => onChange(event.target.value)}
          className="mt-1 w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
        />
      )}
    </div>
  );
}

/** 설정 다이얼로그 "AI 요약" 카테고리(M7-①, ADR-0016, 멀티 프로바이더) — enabled 토글 + 자동 생성
 * 토글 + 모드 라디오(자동/CLI/API 키) + 모드별 프로바이더 select(CLI: Claude/Gemini/Codex, API:
 * Anthropic/OpenAI/Gemini) + 프로바이더에 맞는 모델/키 입력 + 프라이버시 안내. 원래 "일반" 하위
 * 섹션이었지만 항목이 많아져 별도 카테고리로 분리(실사용 요구). 하단 전역 footer 없이 이 섹션
 * 자체가 로컬 편집 상태 + 저장 버튼을 갖는다(자체 완결형 — `ExportDataAction` 등과 동일한 패턴). */
function AISummarySection({ enabled: dialogOpen }: DialogOpenProps) {
  const t = useT();
  const queryClient = useQueryClient();
  const { data, isLoading, isError } = useQuery({
    queryKey: queryKeys.summaryConfig(),
    queryFn: getSummaryConfig,
    enabled: dialogOpen,
  });

  const [localConfig, setLocalConfig] = useState<SummaryConfig | null>(null);
  const [savedMessageVisible, setSavedMessageVisible] = useState(false);

  useEffect(() => {
    if (dialogOpen && data) {
      setLocalConfig(data);
    }
  }, [dialogOpen, data]);

  const saveMutation = useMutation({
    mutationFn: (config: SummaryConfig) => setSummaryConfig(config),
    onSuccess: () => {
      setSavedMessageVisible(true);
      queryClient.invalidateQueries({ queryKey: queryKeys.summaryConfig() });
    },
  });

  function handleChange(next: SummaryConfig) {
    setLocalConfig(next);
    setSavedMessageVisible(false);
  }

  /** 프로바이더별 마지막 선택 모델 기억 — 프로바이더를 전환했다가 되돌아와도 직접 입력(custom)한
   * 모델이 소실되지 않게 한다(리뷰 Warning — API 키가 프로바이더별로 유지되는 것과 일관성). 저장
   * 대상은 아니고 이 다이얼로그 세션 한정 UI 상태다. */
  const lastCliModelRef = useRef<Partial<Record<CliProvider, string>>>({});
  const lastApiModelRef = useRef<Partial<Record<ApiProvider, string>>>({});

  /** CLI 프로바이더 변경: 떠나는 프로바이더의 현재 모델을 기억해 두고, 새 프로바이더는 기억된
   * 값(있으면) 또는 그 프로바이더의 첫 옵션으로 설정한다(다른 프로바이더의 모델명은 유효하지
   * 않을 수 있으므로 그대로 두지는 않는다). */
  function handleCliProviderChange(config: SummaryConfig, provider: CliProvider) {
    lastCliModelRef.current[config.cliProvider] = config.cliModel;
    handleChange({
      ...config,
      cliProvider: provider,
      cliModel: lastCliModelRef.current[provider] ?? CLI_MODEL_OPTIONS_BY_PROVIDER[provider][0],
    });
  }

  /** API 프로바이더 변경 — 위 CLI와 동일한 기억/복원 규칙. */
  function handleApiProviderChange(config: SummaryConfig, provider: ApiProvider) {
    lastApiModelRef.current[config.apiProvider] = config.apiModel;
    handleChange({
      ...config,
      apiProvider: provider,
      apiModel: lastApiModelRef.current[provider] ?? API_MODEL_OPTIONS_BY_PROVIDER[provider][0],
    });
  }

  return (
    <section aria-label={t("settings.summary.heading")} className="space-y-3">
      <h3 className="text-sm font-semibold">{t("settings.summary.heading")}</h3>

      {isLoading && <p className="text-sm text-muted-foreground">{t("settings.loading")}</p>}
      {isError && <p className="text-sm text-destructive">{t("settings.loadError")}</p>}

      {localConfig && (
        <>
          <label htmlFor="summary-enabled" className="flex items-center gap-2 text-sm">
            <input
              id="summary-enabled"
              type="checkbox"
              checked={localConfig.enabled}
              onChange={(event) => handleChange({ ...localConfig, enabled: event.target.checked })}
              className="size-4 rounded border-border accent-primary"
            />
            {t("settings.summary.enableLabel")}
          </label>

          <div className={cn("space-y-3", !localConfig.enabled && "opacity-50")}>
            <div>
              <label htmlFor="summary-auto-generate" className="flex items-center gap-2 text-sm">
                <input
                  id="summary-auto-generate"
                  type="checkbox"
                  checked={localConfig.autoGenerate}
                  disabled={!localConfig.enabled}
                  onChange={(event) =>
                    handleChange({ ...localConfig, autoGenerate: event.target.checked })
                  }
                  className="size-4 rounded border-border accent-primary"
                />
                {t("settings.summary.autoGenerateLabel")}
              </label>
              <p className="mt-1 pl-6 text-xs text-muted-foreground">
                {t("settings.summary.autoGenerateHelp")}
              </p>
            </div>

            <fieldset className="space-y-1.5">
              <legend className="text-xs font-medium text-muted-foreground">
                {t("settings.summary.engineLegend")}
              </legend>
              <div
                role="radiogroup"
                aria-label={t("settings.summary.engineLegend")}
                className="space-y-1.5"
              >
                {SUMMARY_ENGINE_OPTIONS.map((option) => {
                  const inputId = `summary-engine-${option.value}`;
                  return (
                    <label
                      key={option.value}
                      htmlFor={inputId}
                      className="flex items-center gap-2 text-sm"
                    >
                      <input
                        id={inputId}
                        type="radio"
                        name="summary-engine"
                        value={option.value}
                        checked={localConfig.engine === option.value}
                        disabled={!localConfig.enabled}
                        onChange={() => handleChange({ ...localConfig, engine: option.value })}
                        className="size-4 shrink-0 accent-primary"
                      />
                      {t(option.labelKey)}
                    </label>
                  );
                })}
              </div>
            </fieldset>

            {localConfig.engine === "auto" && (
              <p className="text-xs text-muted-foreground">
                {t("settings.summary.autoDetectionOrderNote")}
              </p>
            )}

            {localConfig.engine === "cli" && (
              <div className="space-y-3">
                <div>
                  <label
                    htmlFor="summary-cli-provider"
                    className="text-xs font-medium text-muted-foreground"
                  >
                    {t("settings.summary.cliProviderLabel")}
                  </label>
                  <select
                    id="summary-cli-provider"
                    value={localConfig.cliProvider}
                    disabled={!localConfig.enabled}
                    onChange={(event) => {
                      const { value } = event.target;
                      if (isCliProvider(value)) handleCliProviderChange(localConfig, value);
                    }}
                    className="mt-1 w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
                  >
                    {CLI_PROVIDER_OPTIONS.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                </div>

                <ModelPicker
                  id="summary-cli-model"
                  labelKey="settings.summary.cliModelLabel"
                  value={localConfig.cliModel}
                  options={CLI_MODEL_OPTIONS_BY_PROVIDER[localConfig.cliProvider]}
                  disabled={!localConfig.enabled}
                  customPlaceholder={CLI_MODEL_CUSTOM_PLACEHOLDER[localConfig.cliProvider]}
                  onChange={(cliModel) => handleChange({ ...localConfig, cliModel })}
                />

                {localConfig.cliProvider === "claude" && (
                  <div>
                    <label
                      htmlFor="summary-cli-config-dir"
                      className="text-xs font-medium text-muted-foreground"
                    >
                      {t("settings.summary.cliConfigDirLabel")}
                    </label>
                    <input
                      id="summary-cli-config-dir"
                      type="text"
                      value={localConfig.cliConfigDir ?? ""}
                      disabled={!localConfig.enabled}
                      placeholder="~/.claude-b"
                      autoComplete="off"
                      onChange={(event) =>
                        handleChange({
                          ...localConfig,
                          cliConfigDir: event.target.value.trim() || null,
                        })
                      }
                      className="mt-1 w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
                    />
                    <p className="mt-1 text-xs text-muted-foreground">
                      {t("settings.summary.cliConfigDirHelp")}
                    </p>
                  </div>
                )}
              </div>
            )}

            {localConfig.engine === "api" && (
              <div className="space-y-3">
                <div>
                  <label
                    htmlFor="summary-api-provider"
                    className="text-xs font-medium text-muted-foreground"
                  >
                    {t("settings.summary.apiProviderLabel")}
                  </label>
                  <select
                    id="summary-api-provider"
                    value={localConfig.apiProvider}
                    disabled={!localConfig.enabled}
                    onChange={(event) => {
                      const { value } = event.target;
                      if (isApiProvider(value)) handleApiProviderChange(localConfig, value);
                    }}
                    className="mt-1 w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
                  >
                    {API_PROVIDER_OPTIONS.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                </div>

                {(() => {
                  const apiKeyField = API_KEY_FIELD_BY_PROVIDER[localConfig.apiProvider];
                  const currentKey = localConfig[apiKeyField];
                  const hadKeyOnLoad = data?.[apiKeyField];
                  return (
                    <div>
                      <label
                        htmlFor="summary-api-key"
                        className="text-xs font-medium text-muted-foreground"
                      >
                        {t("settings.summary.apiKeyLabel")}
                      </label>
                      <input
                        id="summary-api-key"
                        type="password"
                        value={currentKey ?? ""}
                        onChange={(event) =>
                          handleChange({
                            ...localConfig,
                            [apiKeyField]: event.target.value.trim() || null,
                          })
                        }
                        placeholder={
                          hadKeyOnLoad
                            ? t("settings.summary.apiKeyPlaceholderChange")
                            : API_KEY_PLACEHOLDER_BY_PROVIDER[localConfig.apiProvider]
                        }
                        autoComplete="off"
                        disabled={!localConfig.enabled}
                        className="mt-1 w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
                      />
                    </div>
                  );
                })()}

                <ModelPicker
                  id="summary-api-model"
                  labelKey="settings.summary.apiModelLabel"
                  value={localConfig.apiModel}
                  options={API_MODEL_OPTIONS_BY_PROVIDER[localConfig.apiProvider]}
                  disabled={!localConfig.enabled}
                  customPlaceholder={API_MODEL_CUSTOM_PLACEHOLDER[localConfig.apiProvider]}
                  onChange={(apiModel) => handleChange({ ...localConfig, apiModel })}
                />
              </div>
            )}

            <p className="text-xs text-muted-foreground">{t("settings.summary.privacyNote")}</p>
          </div>

          <div className="flex items-center gap-2">
            <Button
              type="button"
              variant="default"
              size="sm"
              onClick={() => saveMutation.mutate(localConfig)}
              disabled={saveMutation.isPending}
            >
              {saveMutation.isPending ? t("common.saving") : t("common.save")}
            </Button>
          </div>
          <p className="min-h-4 text-xs" aria-live="polite">
            {saveMutation.isError && (
              <span className="text-destructive">{t("common.saveError")}</span>
            )}
            {savedMessageVisible && !saveMutation.isError && (
              <span className="text-muted-foreground">{t("settings.summary.savedNotice")}</span>
            )}
          </p>
        </>
      )}
    </section>
  );
}

/** 설정 다이얼로그 "AI 요약" 카테고리 — 평가 프로필 섹션(ADR-0017). `AISummarySection`과 달리 로컬
 * 편집 상태·저장 버튼은 재사용 컴포넌트인 `ReviewProfileForm`이 자체적으로 관리한다 — 이 섹션은
 * 제목·설명 + 로딩/에러 표시만 감싼다(같은 `queryKeys.reviewProfile()`을 ReviewPanel.tsx와
 * 공유하므로 저장 즉시 두 화면이 함께 갱신된다). */
function ReviewProfileSection({ enabled: dialogOpen }: DialogOpenProps) {
  const t = useT();
  const { data, isLoading, isError } = useQuery({
    queryKey: queryKeys.reviewProfile(),
    queryFn: getReviewProfile,
    enabled: dialogOpen,
  });

  return (
    <section aria-label={t("review.profile.heading")} className="space-y-3">
      <div>
        <h3 className="text-sm font-semibold">{t("review.profile.heading")}</h3>
        <p className="mt-1 text-xs text-muted-foreground">{t("review.profile.description")}</p>
      </div>
      {isLoading && <p className="text-sm text-muted-foreground">{t("settings.loading")}</p>}
      {isError && <p className="text-sm text-destructive">{t("settings.loadError")}</p>}
      {data && <ReviewProfileForm profile={data} />}
    </section>
  );
}

/** 설정 다이얼로그 "일반" 섹션 — 언어 + 테마 + 자동 실행 토글 + 버전 표시/업데이트 확인·설치
 * (M5, ADR-0013) + 온보딩 다시 보기(M5 온보딩). AI 요약은 별도 "AI 요약" 카테고리로 분리됨. */
function GeneralSection({ enabled: dialogOpen }: DialogOpenProps) {
  const t = useT();
  return (
    <section aria-label={t("settings.general.heading")} className="space-y-3">
      <h3 className="text-sm font-semibold">{t("settings.general.heading")}</h3>
      <LanguageSection />
      <div className="border-t border-border pt-3">
        <ThemeSection />
      </div>
      <div className="border-t border-border pt-3">
        <DefaultViewSection />
      </div>
      <div className="border-t border-border pt-3">
        <AutostartToggle enabled={dialogOpen} />
      </div>
      <UpdateSection enabled={dialogOpen} />
      <ReplayOnboardingAction />
      <FeedbackAction />
    </section>
  );
}

function findResolvedRoot(
  resolvedRoots: ResolvedCaptureRoot[],
  source: CaptureSource,
  path: string,
): ResolvedCaptureRoot | undefined {
  return resolvedRoots.find((root) => root.source === source && root.path === path);
}

interface CaptureSourceSectionProps {
  source: CaptureSource;
  config: CaptureSourceConfig;
  resolvedRoots: ResolvedCaptureRoot[];
  onChange: (config: CaptureSourceConfig) => void;
}

function CaptureSourceSection({
  source,
  config,
  resolvedRoots,
  onChange,
}: CaptureSourceSectionProps) {
  const t = useT();
  const [newPath, setNewPath] = useState("");
  const [pathError, setPathError] = useState<string | null>(null);
  const label = SOURCE_LABELS[source];
  const defaultRoots = resolvedRoots.filter(
    (root) => root.source === source && root.origin === "default",
  );
  const enabledId = `capture-enabled-${source}`;
  const useDefaultsId = `capture-use-defaults-${source}`;
  const newPathId = `capture-new-path-${source}`;
  const trimmedNewPath = newPath.trim();

  function handleAddPath() {
    if (trimmedNewPath.length === 0 || config.extraRoots.includes(trimmedNewPath)) return;
    if (!trimmedNewPath.startsWith("/")) {
      setPathError(t("settings.absolutePathError"));
      return;
    }
    onChange({ ...config, extraRoots: [...config.extraRoots, trimmedNewPath] });
    setNewPath("");
    setPathError(null);
  }

  function handleRemovePath(path: string) {
    onChange({ ...config, extraRoots: config.extraRoots.filter((root) => root !== path) });
  }

  return (
    <section
      aria-label={t("settings.captureSource.sectionAriaLabel", { label })}
      className="space-y-3"
    >
      <h3 className="text-sm font-semibold">{label}</h3>

      <label htmlFor={enabledId} className="flex items-center gap-2 text-sm">
        <input
          id={enabledId}
          type="checkbox"
          checked={config.enabled}
          onChange={(event) => onChange({ ...config, enabled: event.target.checked })}
          className="size-4 rounded border-border accent-primary"
        />
        {t("settings.captureSource.enableLabel")}
      </label>
      {!config.enabled && (
        <p className="text-xs text-muted-foreground">{t("settings.captureSource.disabledNote")}</p>
      )}

      <div className={cn("space-y-3", !config.enabled && "opacity-50")}>
        <label htmlFor={useDefaultsId} className="flex items-center gap-2 text-sm">
          <input
            id={useDefaultsId}
            type="checkbox"
            checked={config.useDefaults}
            onChange={(event) => onChange({ ...config, useDefaults: event.target.checked })}
            className="size-4 rounded border-border accent-primary"
          />
          {t("settings.captureSource.useDefaultsLabel")}
        </label>

        <div className={cn(!config.useDefaults && "opacity-50")}>
          <p className="text-xs font-medium text-muted-foreground">
            {t("settings.captureSource.defaultRootsHeading")}
          </p>
          {!config.useDefaults && (
            <p className="mt-1 text-xs text-muted-foreground">
              {t("settings.captureSource.defaultRootsExcludedNote")}
            </p>
          )}
          {defaultRoots.length === 0 ? (
            <p className="mt-1 text-xs text-muted-foreground">
              {t("settings.captureSource.noDefaultRoots")}
            </p>
          ) : (
            <ul className="mt-1 space-y-1">
              {defaultRoots.map((root) => (
                <li
                  key={root.path}
                  className="flex items-center justify-between gap-2 rounded-md border border-border px-2 py-1 text-xs"
                >
                  <span className="truncate font-mono">{root.path}</span>
                  {!root.exists && (
                    <span className="shrink-0 rounded-sm bg-destructive/10 px-1.5 py-0.5 text-[10px] font-medium text-destructive">
                      {t("settings.captureSource.missingBadge")}
                    </span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>

        <div>
          <p className="text-xs font-medium text-muted-foreground">
            {t("settings.captureSource.customRootsHeading")}
          </p>
          {config.extraRoots.length === 0 ? (
            <p className="mt-1 text-xs text-muted-foreground">
              {t("settings.captureSource.noCustomRoots")}
            </p>
          ) : (
            <ul className="mt-1 space-y-1">
              {config.extraRoots.map((path) => {
                const resolved = findResolvedRoot(resolvedRoots, source, path);
                return (
                  <li
                    key={path}
                    className="flex items-center justify-between gap-2 rounded-md border border-border px-2 py-1 text-xs"
                  >
                    <span className="truncate font-mono">{path}</span>
                    <div className="flex shrink-0 items-center gap-1">
                      {resolved && !resolved.exists && (
                        <span className="rounded-sm bg-destructive/10 px-1.5 py-0.5 text-[10px] font-medium text-destructive">
                          {t("settings.captureSource.missingBadge")}
                        </span>
                      )}
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-xs"
                        aria-label={t("settings.captureSource.deleteCustomRootAriaLabel", { path })}
                        onClick={() => handleRemovePath(path)}
                      >
                        <Trash2 className="size-3.5" />
                      </Button>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}

          <div className="mt-2 flex items-center gap-2">
            <label htmlFor={newPathId} className="sr-only">
              {t("settings.captureSource.addCustomRootLabel", { label })}
            </label>
            <input
              id={newPathId}
              type="text"
              value={newPath}
              onChange={(event) => {
                setNewPath(event.target.value);
                setPathError(null);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  handleAddPath();
                }
              }}
              placeholder="/path/to/logs"
              aria-invalid={pathError !== null}
              className="w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring"
            />
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={handleAddPath}
              disabled={trimmedNewPath.length === 0 || config.extraRoots.includes(trimmedNewPath)}
            >
              {t("common.add")}
            </Button>
          </div>
          <p className="mt-1 min-h-4 text-xs" aria-live="polite">
            {pathError && <span className="text-destructive">{pathError}</span>}
          </p>
        </div>
      </div>
    </section>
  );
}

interface BodyPolicySectionProps {
  value: BodyPolicy;
  onChange: (value: BodyPolicy) => void;
}

function BodyPolicySection({ value, onChange }: BodyPolicySectionProps) {
  const t = useT();
  return (
    <fieldset className="space-y-3">
      <legend className="text-sm font-semibold">{t("settings.bodyPolicy.legend")}</legend>
      <div role="radiogroup" aria-label={t("settings.bodyPolicy.legend")} className="space-y-2">
        {BODY_POLICY_OPTIONS.map((option) => {
          const inputId = `body-policy-${option.value}`;
          return (
            <label key={option.value} htmlFor={inputId} className="flex items-start gap-2 text-sm">
              <input
                id={inputId}
                type="radio"
                name="body-policy"
                value={option.value}
                checked={value === option.value}
                onChange={() => onChange(option.value)}
                className="mt-0.5 size-4 shrink-0 accent-primary"
              />
              <span>
                <span className="font-medium">{t(option.labelKey)}</span>
                <span className="block text-xs text-muted-foreground">
                  {t(option.descriptionKey)}
                </span>
              </span>
            </label>
          );
        })}
      </div>
    </fieldset>
  );
}

interface ExcludeProjectsSectionProps {
  value: string[];
  onChange: (value: string[]) => void;
}

/** 제외할 프로젝트 절대경로 목록 편집. `CaptureSourceSection`의 커스텀 경로 추가/삭제 패턴을
 * 그대로 재사용한다(절대경로 검증 + 인라인 에러). */
function ExcludeProjectsSection({ value, onChange }: ExcludeProjectsSectionProps) {
  const t = useT();
  const [newPath, setNewPath] = useState("");
  const [pathError, setPathError] = useState<string | null>(null);
  const newPathId = "exclude-project-new-path";
  const trimmedNewPath = newPath.trim();

  function handleAddPath() {
    if (trimmedNewPath.length === 0 || value.includes(trimmedNewPath)) return;
    if (!trimmedNewPath.startsWith("/")) {
      setPathError(t("settings.absolutePathError"));
      return;
    }
    onChange([...value, trimmedNewPath]);
    setNewPath("");
    setPathError(null);
  }

  function handleRemovePath(path: string) {
    onChange(value.filter((p) => p !== path));
  }

  return (
    <section aria-label={t("settings.excludeProjects.heading")} className="space-y-3">
      <div>
        <h3 className="text-sm font-semibold">{t("settings.excludeProjects.heading")}</h3>
        <p className="mt-1 text-xs text-muted-foreground">
          {t("settings.excludeProjects.description")}
        </p>
      </div>

      {value.length === 0 ? (
        <p className="text-xs text-muted-foreground">{t("settings.excludeProjects.empty")}</p>
      ) : (
        <ul className="space-y-1">
          {value.map((path) => (
            <li
              key={path}
              className="flex items-center justify-between gap-2 rounded-md border border-border px-2 py-1 text-xs"
            >
              <span className="truncate font-mono">{path}</span>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label={t("settings.excludeProjects.deleteAriaLabel", { path })}
                onClick={() => handleRemovePath(path)}
              >
                <Trash2 className="size-3.5" />
              </Button>
            </li>
          ))}
        </ul>
      )}

      <div className="flex items-center gap-2">
        <label htmlFor={newPathId} className="sr-only">
          {t("settings.excludeProjects.addLabel")}
        </label>
        <input
          id={newPathId}
          type="text"
          value={newPath}
          onChange={(event) => {
            setNewPath(event.target.value);
            setPathError(null);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              handleAddPath();
            }
          }}
          placeholder="/path/to/project"
          aria-invalid={pathError !== null}
          className="w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring"
        />
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={handleAddPath}
          disabled={trimmedNewPath.length === 0 || value.includes(trimmedNewPath)}
        >
          {t("common.add")}
        </Button>
      </div>
      <p className="min-h-4 text-xs" aria-live="polite">
        {pathError && <span className="text-destructive">{pathError}</span>}
      </p>
    </section>
  );
}

interface ConnectorsSectionProps {
  value: SlackConfig;
  onChange: (value: SlackConfig) => void;
  /** 서버가 이미 slack 토큰을 저장하고 있는지(다이얼로그를 연 시점 기준 — 편집 중인 `value.token`은
   * 마스킹 값을 그대로 보여주므로 placeholder 문구 결정에는 이 값을 대신 쓴다). */
  hasExistingToken: boolean;
}

/** 설정 다이얼로그 "커넥터" 섹션 — Slack v1(docs/08-connectors.md, ADR-0015): enabled 토글 + 수동
 * user token 입력(password) + 폴링 주기 + 토큰 발급 가이드. Pro 게이팅(M6)은 아직 구현되지 않아
 * "Pro 예정" 뱃지만 표시한다. 기존 토큰이 있으면 서버가 마스킹된 값(`"••••" + 마지막 4자`)을
 * `value.token`으로 내려주므로 입력창에 그대로 표시된다 — 그 값을 변경 없이 그대로 저장해도
 * BE(`set_capture_config`)가 원본 토큰을 보존하므로 안전하다. */
function ConnectorsSection({ value, onChange, hasExistingToken }: ConnectorsSectionProps) {
  const t = useT();
  const enabledId = "connector-slack-enabled";
  const tokenId = "connector-slack-token";
  const pollMinutesId = "connector-slack-poll-minutes";

  function handleOpenGuide() {
    void openUrl(SLACK_TOKEN_GUIDE_URL);
  }

  return (
    <section aria-label={t("settings.connectors.heading")} className="space-y-3">
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-sm font-semibold">{t("settings.connectors.heading")}</h3>
        <span className="rounded-full border border-border px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
          {t("settings.connectors.proBadge")}
        </span>
      </div>

      <div className="space-y-3 rounded-md border border-border p-3">
        <label htmlFor={enabledId} className="flex items-center gap-2 text-sm">
          <input
            id={enabledId}
            type="checkbox"
            checked={value.enabled}
            onChange={(event) => onChange({ ...value, enabled: event.target.checked })}
            className="size-4 rounded border-border accent-primary"
          />
          {t("settings.connectors.slackLabel")}
        </label>

        <div className={cn("space-y-3", !value.enabled && "opacity-50")}>
          <div>
            <label htmlFor={tokenId} className="text-xs font-medium text-muted-foreground">
              User Token
            </label>
            <input
              id={tokenId}
              type="password"
              value={value.token ?? ""}
              onChange={(event) => onChange({ ...value, token: event.target.value.trim() || null })}
              placeholder={
                hasExistingToken ? t("settings.connectors.tokenPlaceholderChange") : "xoxp-..."
              }
              autoComplete="off"
              disabled={!value.enabled}
              className="mt-1 w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
            />
          </div>

          <div>
            <label htmlFor={pollMinutesId} className="text-xs font-medium text-muted-foreground">
              {t("settings.connectors.pollMinutesLabel")}
            </label>
            <input
              id={pollMinutesId}
              type="number"
              min={1}
              value={value.pollMinutes}
              disabled={!value.enabled}
              onChange={(event) =>
                onChange({ ...value, pollMinutes: Math.max(1, Number(event.target.value) || 1) })
              }
              className="mt-1 w-20 rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
            />
          </div>

          <p className="text-xs text-muted-foreground">
            {t("settings.connectors.pollDescriptionPrefix")}{" "}
            <button
              type="button"
              onClick={handleOpenGuide}
              className="underline hover:text-foreground"
            >
              api.slack.com/apps
            </button>{" "}
            {t("settings.connectors.guideStep1")} <code>search:read</code>{" "}
            {t("settings.connectors.guideStep2")} <code>xoxp-</code>
            {t("settings.connectors.guideStep3")}
          </p>
          <p className="text-xs text-muted-foreground">
            {t("settings.connectors.appliesAfterRestart")}
          </p>
        </div>
      </div>
    </section>
  );
}

/** 마스킹되지 않은(사용자가 방금 입력한) 토큰을 화면에 노출하지 않기 위한 클라이언트 표시용 마스킹.
 * 서버가 이미 마스킹해 내려준 값(`"••••"` 접두)은 그대로 두고, 아직 저장 전이라 원문 그대로인
 * 값만 여기서 마스킹한다(GitHub는 계정마다 별도 행에 토큰을 나열하므로 Slack의 단일
 * password input과 달리 평문 노출 위험이 있다 — 보안 리뷰 원칙 준수). */
function maskTokenForDisplay(token: string): string {
  if (token.startsWith("••••")) return token;
  const visibleLen = Math.min(token.length, 4);
  return `••••${token.slice(token.length - visibleLen)}`;
}

interface GithubConnectorSectionProps {
  value: GithubConfig;
  onChange: (value: GithubConfig) => void;
}

/** 설정 다이얼로그 "커넥터" 섹션 — GitHub v1(docs/08-connectors.md): enabled 토글 + 다중 계정
 * 목록(username 표시 + 토큰 마스킹 + 삭제) + PAT 추가 입력 + 폴링 주기 + 발급 가이드. Slack
 * `ConnectorsSection`과 달리 계정이 여러 개일 수 있어 "추가/삭제" 방식으로 편집한다(개별 토큰
 * 인라인 수정은 지원하지 않음 — 교체하려면 삭제 후 재추가). `username`이 아직 `null`이면 폴러가
 * `GET /user` 검증에 아직 성공하지 못한 상태(다음 재시작 후 폴링 성공 시 채워진다). */
function GithubConnectorSection({ value, onChange }: GithubConnectorSectionProps) {
  const t = useT();
  const [newToken, setNewToken] = useState("");
  const enabledId = "connector-github-enabled";
  const pollMinutesId = "connector-github-poll-minutes";
  const newTokenId = "connector-github-new-token";
  const trimmedNewToken = newToken.trim();

  function handleOpenGuide() {
    void openUrl(GITHUB_TOKEN_GUIDE_URL);
  }

  function handleAddAccount() {
    if (trimmedNewToken.length === 0) return;
    onChange({
      ...value,
      accounts: [...value.accounts, { token: trimmedNewToken, username: null }],
    });
    setNewToken("");
  }

  function handleRemoveAccount(index: number) {
    onChange({ ...value, accounts: value.accounts.filter((_, i) => i !== index) });
  }

  return (
    <section aria-label={t("settings.connectors.githubHeading")} className="space-y-3">
      <h3 className="text-sm font-semibold">{t("settings.connectors.githubHeading")}</h3>

      <div className="space-y-3 rounded-md border border-border p-3">
        <label htmlFor={enabledId} className="flex items-center gap-2 text-sm">
          <input
            id={enabledId}
            type="checkbox"
            checked={value.enabled}
            onChange={(event) => onChange({ ...value, enabled: event.target.checked })}
            className="size-4 rounded border-border accent-primary"
          />
          {t("settings.connectors.githubLabel")}
        </label>

        <div className={cn("space-y-3", !value.enabled && "opacity-50")}>
          <div>
            <p className="text-xs font-medium text-muted-foreground">
              {t("settings.connectors.githubAccountsHeading")}
            </p>
            {value.accounts.length === 0 ? (
              <p className="mt-1 text-xs text-muted-foreground">
                {t("settings.connectors.githubNoAccounts")}
              </p>
            ) : (
              <ul className="mt-1 space-y-1">
                {value.accounts.map((account, index) => (
                  <li
                    key={account.token}
                    className="flex items-center justify-between gap-2 rounded-md border border-border px-2 py-1 text-xs"
                  >
                    <span className="min-w-0 truncate font-mono">
                      {account.username ?? t("settings.connectors.githubUsernamePending")}
                      {" · "}
                      {maskTokenForDisplay(account.token)}
                    </span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      disabled={!value.enabled}
                      aria-label={t("settings.connectors.githubDeleteAccountAriaLabel", {
                        account: account.username ?? maskTokenForDisplay(account.token),
                      })}
                      onClick={() => handleRemoveAccount(index)}
                    >
                      <Trash2 className="size-3.5" />
                    </Button>
                  </li>
                ))}
              </ul>
            )}

            <div className="mt-2 flex items-center gap-2">
              <label htmlFor={newTokenId} className="sr-only">
                {t("settings.connectors.githubAddAccountLabel")}
              </label>
              <input
                id={newTokenId}
                type="password"
                value={newToken}
                onChange={(event) => setNewToken(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    handleAddAccount();
                  }
                }}
                placeholder="github_pat_..."
                autoComplete="off"
                disabled={!value.enabled}
                className="w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
              />
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={handleAddAccount}
                disabled={!value.enabled || trimmedNewToken.length === 0}
              >
                {t("common.add")}
              </Button>
            </div>
          </div>

          <div>
            <label htmlFor={pollMinutesId} className="text-xs font-medium text-muted-foreground">
              {t("settings.connectors.pollMinutesLabel")}
            </label>
            <input
              id={pollMinutesId}
              type="number"
              min={1}
              value={value.pollMinutes}
              disabled={!value.enabled}
              onChange={(event) =>
                onChange({ ...value, pollMinutes: Math.max(1, Number(event.target.value) || 1) })
              }
              className="mt-1 w-20 rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
            />
          </div>

          <p className="text-xs text-muted-foreground">
            {t("settings.connectors.githubGuidePrefix")}{" "}
            <button
              type="button"
              onClick={handleOpenGuide}
              className="underline hover:text-foreground"
            >
              github.com/settings → Developer settings
            </button>{" "}
            {t("settings.connectors.githubGuideSuffix")}
          </p>
          <p className="text-xs text-muted-foreground">
            {t("settings.connectors.appliesAfterRestart")}
          </p>
        </div>
      </div>
    </section>
  );
}

interface LinearConnectorSectionProps {
  value: LinearConfig;
  onChange: (value: LinearConfig) => void;
}

/** 설정 다이얼로그 "커넥터" 섹션 — Linear v1(docs/08-connectors.md): enabled 토글 + 다중 워크스페이스
 * 목록(viewerName 표시 + 토큰 마스킹 + 삭제) + Personal API Key 추가 입력 + 폴링 주기 + 발급 가이드.
 * `GithubConnectorSection`과 동일한 다중 계정(워크스페이스) 편집 패턴(추가/삭제, 인라인 수정 불가 —
 * 교체하려면 삭제 후 재추가). `viewerName`이 아직 `null`이면 폴러가 `viewer` 검증에 아직 성공하지
 * 못한 상태(다음 재시작 후 폴링 성공 시 채워진다). */
function LinearConnectorSection({ value, onChange }: LinearConnectorSectionProps) {
  const t = useT();
  const [newToken, setNewToken] = useState("");
  const enabledId = "connector-linear-enabled";
  const pollMinutesId = "connector-linear-poll-minutes";
  const newTokenId = "connector-linear-new-token";
  const trimmedNewToken = newToken.trim();

  function handleOpenGuide() {
    void openUrl(LINEAR_TOKEN_GUIDE_URL);
  }

  function handleAddAccount() {
    if (trimmedNewToken.length === 0) return;
    onChange({
      ...value,
      accounts: [...value.accounts, { token: trimmedNewToken, viewerId: null, viewerName: null }],
    });
    setNewToken("");
  }

  function handleRemoveAccount(index: number) {
    onChange({ ...value, accounts: value.accounts.filter((_, i) => i !== index) });
  }

  return (
    <section aria-label={t("settings.connectors.linearHeading")} className="space-y-3">
      <h3 className="text-sm font-semibold">{t("settings.connectors.linearHeading")}</h3>

      <div className="space-y-3 rounded-md border border-border p-3">
        <label htmlFor={enabledId} className="flex items-center gap-2 text-sm">
          <input
            id={enabledId}
            type="checkbox"
            checked={value.enabled}
            onChange={(event) => onChange({ ...value, enabled: event.target.checked })}
            className="size-4 rounded border-border accent-primary"
          />
          {t("settings.connectors.linearLabel")}
        </label>

        <div className={cn("space-y-3", !value.enabled && "opacity-50")}>
          <div>
            <p className="text-xs font-medium text-muted-foreground">
              {t("settings.connectors.linearAccountsHeading")}
            </p>
            {value.accounts.length === 0 ? (
              <p className="mt-1 text-xs text-muted-foreground">
                {t("settings.connectors.linearNoAccounts")}
              </p>
            ) : (
              <ul className="mt-1 space-y-1">
                {value.accounts.map((account, index) => (
                  <li
                    key={account.token}
                    className="flex items-center justify-between gap-2 rounded-md border border-border px-2 py-1 text-xs"
                  >
                    <span className="min-w-0 truncate font-mono">
                      {account.viewerName ?? t("settings.connectors.linearUsernamePending")}
                      {" · "}
                      {maskTokenForDisplay(account.token)}
                    </span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      disabled={!value.enabled}
                      aria-label={t("settings.connectors.linearDeleteAccountAriaLabel", {
                        account: account.viewerName ?? maskTokenForDisplay(account.token),
                      })}
                      onClick={() => handleRemoveAccount(index)}
                    >
                      <Trash2 className="size-3.5" />
                    </Button>
                  </li>
                ))}
              </ul>
            )}

            <div className="mt-2 flex items-center gap-2">
              <label htmlFor={newTokenId} className="sr-only">
                {t("settings.connectors.linearAddAccountLabel")}
              </label>
              <input
                id={newTokenId}
                type="password"
                value={newToken}
                onChange={(event) => setNewToken(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    handleAddAccount();
                  }
                }}
                placeholder="lin_api_..."
                autoComplete="off"
                disabled={!value.enabled}
                className="w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
              />
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={handleAddAccount}
                disabled={!value.enabled || trimmedNewToken.length === 0}
              >
                {t("common.add")}
              </Button>
            </div>
          </div>

          <div>
            <label htmlFor={pollMinutesId} className="text-xs font-medium text-muted-foreground">
              {t("settings.connectors.pollMinutesLabel")}
            </label>
            <input
              id={pollMinutesId}
              type="number"
              min={1}
              value={value.pollMinutes}
              disabled={!value.enabled}
              onChange={(event) =>
                onChange({ ...value, pollMinutes: Math.max(1, Number(event.target.value) || 1) })
              }
              className="mt-1 w-20 rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
            />
          </div>

          <p className="text-xs text-muted-foreground">
            {t("settings.connectors.linearGuidePrefix")}{" "}
            <button
              type="button"
              onClick={handleOpenGuide}
              className="underline hover:text-foreground"
            >
              linear.app → Settings → Security & access → Personal API keys
            </button>{" "}
            {t("settings.connectors.linearGuideSuffix")}
          </p>
          <p className="text-xs text-muted-foreground">
            {t("settings.connectors.appliesAfterRestart")}
          </p>
        </div>
      </div>
    </section>
  );
}

interface NotionConnectorSectionProps {
  value: NotionConfig;
  onChange: (value: NotionConfig) => void;
}

/** 설정 다이얼로그 "커넥터" 섹션 — Notion v1(docs/08-connectors.md): enabled 토글 + 다중 계정
 * 목록(이름 + 토큰 종류 + 토큰 마스킹 + 삭제) + 이름·토큰 추가 입력 + 폴링 주기 + 발급 가이드.
 * `LinearConnectorSection`과 동일한 다중 계정 편집 패턴(추가/삭제, 인라인 수정 불가).
 *
 * **이름 입력이 다른 커넥터에 없는 이유** — PAT는 워크스페이스 이름을 알려 주지 않고, 같은 사람이
 * 개인·회사 워크스페이스에 PAT를 하나씩 만들면 서버가 받는 신원도 같다. 사용자가 붙인 이름이 목록과
 * 타임라인(`project`)에서 두 계정을 구분하는 유일한 값이다.
 *
 * 토큰 종류(PAT/internal integration)는 사용자가 고르지 않는다 — 서버가 검증하면서 `kind`를 채우고,
 * 목록이 그 결과("내 편집만" / "연결한 페이지의 모든 편집")를 보여 준다. integration은 페이지마다
 * 연결을 붙여야 하고 연결이 없으면 오류 없이 0건이라(헬스도 ok) 그 안내를 항상 노출한다. */
function NotionConnectorSection({ value, onChange }: NotionConnectorSectionProps) {
  const t = useT();
  const [newLabel, setNewLabel] = useState("");
  const [newToken, setNewToken] = useState("");
  const enabledId = "connector-notion-enabled";
  const pollMinutesId = "connector-notion-poll-minutes";
  const newLabelId = "connector-notion-new-label";
  const newTokenId = "connector-notion-new-token";
  const trimmedNewLabel = newLabel.trim();
  const trimmedNewToken = newToken.trim();

  function handleOpenGuide() {
    void openUrl(NOTION_TOKEN_GUIDE_URL);
  }

  function handleAddAccount() {
    if (trimmedNewToken.length === 0) return;
    onChange({
      ...value,
      accounts: [
        ...value.accounts,
        {
          token: trimmedNewToken,
          id: null,
          label: trimmedNewLabel.length > 0 ? trimmedNewLabel : null,
          kind: null,
          userName: null,
          workspaceId: null,
          workspaceName: null,
        },
      ],
    });
    setNewLabel("");
    setNewToken("");
  }

  function handleRemoveAccount(index: number) {
    onChange({ ...value, accounts: value.accounts.filter((_, i) => i !== index) });
  }

  /** 서버 `capture/notion.rs::project_name`과 같은 순서(이름 → integration 워크스페이스명 →
   * "Notion") — 여기 보이는 이름이 곧 타임라인의 project다. 검증 전이면 `null`. */
  function accountName(account: NotionAccount): string | null {
    const name = account.label ?? account.workspaceName;
    if (name) return name;
    return account.kind ? NOTION_DEFAULT_PROJECT : null;
  }

  /** 토큰 종류 표시. PAT면 누구의 편집을 거르는지 확인할 수 있게 Notion 사용자 이름을 붙인다. */
  function kindLabel(account: NotionAccount): string | null {
    if (account.kind === "personal") {
      const label = t("settings.connectors.notionKindPersonal");
      return account.userName ? `${label} (${account.userName})` : label;
    }
    if (account.kind === "integration") return t("settings.connectors.notionKindIntegration");
    return null;
  }

  function handleEnterToAdd(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Enter") {
      event.preventDefault();
      handleAddAccount();
    }
  }

  return (
    <section aria-label={t("settings.connectors.notionHeading")} className="space-y-3">
      <h3 className="text-sm font-semibold">{t("settings.connectors.notionHeading")}</h3>

      <div className="space-y-3 rounded-md border border-border p-3">
        <label htmlFor={enabledId} className="flex items-center gap-2 text-sm">
          <input
            id={enabledId}
            type="checkbox"
            checked={value.enabled}
            onChange={(event) => onChange({ ...value, enabled: event.target.checked })}
            className="size-4 rounded border-border accent-primary"
          />
          {t("settings.connectors.notionLabel")}
        </label>

        <div className={cn("space-y-3", !value.enabled && "opacity-50")}>
          <div>
            <p className="text-xs font-medium text-muted-foreground">
              {t("settings.connectors.notionAccountsHeading")}
            </p>
            {value.accounts.length === 0 ? (
              <p className="mt-1 text-xs text-muted-foreground">
                {t("settings.connectors.notionNoAccounts")}
              </p>
            ) : (
              <ul className="mt-1 space-y-1">
                {value.accounts.map((account, index) => {
                  const name = accountName(account);
                  const kind = kindLabel(account);
                  return (
                    <li
                      key={account.id ?? account.token}
                      className="flex items-center justify-between gap-2 rounded-md border border-border px-2 py-1 text-xs"
                    >
                      <span className="min-w-0 truncate">
                        <span className="font-medium">
                          {name ?? t("settings.connectors.notionUsernamePending")}
                        </span>
                        {kind ? (
                          <span className="text-muted-foreground">{` · ${kind}`}</span>
                        ) : null}
                        <span className="font-mono text-muted-foreground">
                          {` · ${maskTokenForDisplay(account.token)}`}
                        </span>
                      </span>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-xs"
                        disabled={!value.enabled}
                        aria-label={t("settings.connectors.notionDeleteAccountAriaLabel", {
                          account: name ?? maskTokenForDisplay(account.token),
                        })}
                        onClick={() => handleRemoveAccount(index)}
                      >
                        <Trash2 className="size-3.5" />
                      </Button>
                    </li>
                  );
                })}
              </ul>
            )}

            <div className="mt-2 flex items-center gap-2">
              <label htmlFor={newLabelId} className="sr-only">
                {t("settings.connectors.notionAccountLabelInput")}
              </label>
              <input
                id={newLabelId}
                type="text"
                value={newLabel}
                onChange={(event) => setNewLabel(event.target.value)}
                onKeyDown={handleEnterToAdd}
                placeholder={t("settings.connectors.notionAccountLabelPlaceholder")}
                autoComplete="off"
                disabled={!value.enabled}
                className="w-32 shrink-0 rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
              />
              <label htmlFor={newTokenId} className="sr-only">
                {t("settings.connectors.notionAddAccountLabel")}
              </label>
              <input
                id={newTokenId}
                type="password"
                value={newToken}
                onChange={(event) => setNewToken(event.target.value)}
                onKeyDown={handleEnterToAdd}
                placeholder="ntn_..."
                autoComplete="off"
                disabled={!value.enabled}
                className="w-full rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
              />
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={handleAddAccount}
                disabled={!value.enabled || trimmedNewToken.length === 0}
              >
                {t("common.add")}
              </Button>
            </div>
          </div>

          <div>
            <label htmlFor={pollMinutesId} className="text-xs font-medium text-muted-foreground">
              {t("settings.connectors.pollMinutesLabel")}
            </label>
            <input
              id={pollMinutesId}
              type="number"
              min={1}
              value={value.pollMinutes}
              disabled={!value.enabled}
              onChange={(event) =>
                onChange({ ...value, pollMinutes: Math.max(1, Number(event.target.value) || 1) })
              }
              className="mt-1 w-20 rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed"
            />
          </div>

          <p className="text-xs text-muted-foreground">
            {t("settings.connectors.notionGuidePrefix")}{" "}
            <button
              type="button"
              onClick={handleOpenGuide}
              className="underline hover:text-foreground"
            >
              notion.so/developers/tokens
            </button>{" "}
            {t("settings.connectors.notionGuideSuffix")}
          </p>
          <p className="rounded-md border border-amber-500/40 bg-amber-500/10 p-2 text-xs text-foreground">
            {t("settings.connectors.notionCaveat")}
          </p>
          <p className="text-xs text-muted-foreground">
            {t("settings.connectors.notionIntegrationNote")}
          </p>
          <p className="text-xs text-muted-foreground">
            {t("settings.connectors.appliesAfterRestart")}
          </p>
        </div>
      </div>
    </section>
  );
}

/** 커넥터 카테고리 하단의 향후 지원 예정 커넥터 미리보기 행(Figma·Jira) — 아직 구현되지 않은
 * 안내용 UI라 muted 처리 + Pro 뱃지만 보여준다(클릭 불가, 폼 상태 없음). */
function ConnectorsComingSoon() {
  const t = useT();
  return (
    <div className="flex items-center justify-between gap-2 rounded-md border border-border p-3 opacity-50">
      <span className="text-sm">{t("settings.connectors.comingSoonLabel")}</span>
      <span className="rounded-full border border-border px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">
        {t("settings.connectors.proBadge")}
      </span>
    </div>
  );
}

/** bytes를 소수점 1자리 MB 문자열로 표시(데이터 섹션 DB 크기). */
function formatMegabytes(bytes: number): string {
  return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}

/** 기간 삭제 입력 기본값(일). 자동 삭제 정책은 없음 — 수동 실행 시 초깃값으로만 쓰인다. */
const DEFAULT_RETENTION_DAYS = 90;
const MIN_RETENTION_DAYS = 1;
const MS_PER_DAY = 24 * 60 * 60 * 1000;

/** 내보내기 버튼 — 완료/실패를 aria-live로 알린다. 결과는 다이얼로그를 닫아도 리셋되지 않고
 * mutation 상태 그대로 유지한다(재조회 없는 1회성 동작이라 재차 확인할 수 있게). */
function ExportDataAction() {
  const t = useT();
  const exportMutation = useMutation({ mutationFn: exportData });

  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between gap-2">
        <div className="min-w-0">
          <h4 className="text-sm font-medium">{t("settings.export.heading")}</h4>
          <p className="text-xs text-muted-foreground">{t("settings.export.description")}</p>
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => exportMutation.mutate()}
          disabled={exportMutation.isPending}
        >
          {exportMutation.isPending ? t("settings.export.exporting") : t("settings.export.button")}
        </Button>
      </div>
      <p className="min-h-4 text-xs" aria-live="polite">
        {exportMutation.isError && (
          <span className="text-destructive">{t("settings.export.error")}</span>
        )}
        {exportMutation.isSuccess && (
          <span className="text-muted-foreground">
            {t("settings.export.successMessage", {
              dir: exportMutation.data.dir,
              streams: exportMutation.data.streams.toLocaleString(),
              events: exportMutation.data.events.toLocaleString(),
            })}
          </span>
        )}
      </p>
    </div>
  );
}

/** 기간 삭제 — 파괴적 동작이라 2단계(입력 → 인라인 확인) 없이 바로 삭제되지 않게 한다.
 * 자동 보존 정책은 이번 범위 밖이며 항상 사용자가 직접 눌러야만 실행된다. */
function DeleteOldEventsAction() {
  const t = useT();
  const queryClient = useQueryClient();
  const [days, setDays] = useState(DEFAULT_RETENTION_DAYS);
  const [confirming, setConfirming] = useState(false);
  // 미리보기(count) 조회 시점에 계산한 기준 ts. 실제 삭제는 이 값을 그대로 재사용한다 —
  // 확인 문구를 본 뒤 시간이 흘러도 재계산하지 않아야 "이벤트 X건" 문구와 실제 삭제 결과가 일치한다.
  const [confirmedThresholdTs, setConfirmedThresholdTs] = useState<number | null>(null);
  const daysInputId = "delete-old-events-days";

  const previewMutation = useMutation({
    mutationFn: (ts: number) => countEventsBefore(ts),
    onSuccess: () => setConfirming(true),
  });

  const deleteMutation = useMutation({
    mutationFn: (ts: number) => deleteEventsBefore(ts),
    onSuccess: () => {
      setConfirming(false);
      setConfirmedThresholdTs(null);
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_EVENTS_BY_DAY] });
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_DIGEST] });
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_SEARCH] });
      queryClient.invalidateQueries({ queryKey: [QUERY_KEY_STREAM] });
      queryClient.invalidateQueries({ queryKey: queryKeys.dbStats() });
    },
  });

  function thresholdTs(): number {
    return Date.now() - days * MS_PER_DAY;
  }

  function handleRequestDelete() {
    deleteMutation.reset();
    const ts = thresholdTs();
    setConfirmedThresholdTs(ts);
    previewMutation.mutate(ts);
  }

  function handleCancel() {
    setConfirming(false);
    setConfirmedThresholdTs(null);
    previewMutation.reset();
  }

  function handleConfirmDelete() {
    if (confirmedThresholdTs === null) return;
    deleteMutation.mutate(confirmedThresholdTs);
  }

  function handleDaysChange(next: number) {
    setDays(next);
    // input은 confirming 중엔 disabled라 실제로는 여기 도달하지 않지만, 방어적으로 확인
    // 단계를 리셋해 둔다(재계산 없이 오래된 ts로 삭제되는 사고를 막기 위함).
    if (confirming) {
      setConfirming(false);
      setConfirmedThresholdTs(null);
      previewMutation.reset();
    }
  }

  const busy = previewMutation.isPending || deleteMutation.isPending;

  return (
    <div className="space-y-2">
      <div>
        <h4 className="text-sm font-medium">{t("settings.deleteOld.heading")}</h4>
        <p className="text-xs text-muted-foreground">{t("settings.deleteOld.description")}</p>
      </div>

      <div className="flex items-center gap-2">
        <label htmlFor={daysInputId} className="text-xs text-muted-foreground">
          {t("settings.deleteOld.retentionDaysLabel")}
        </label>
        <input
          id={daysInputId}
          type="number"
          min={MIN_RETENTION_DAYS}
          value={days}
          onChange={(event) =>
            handleDaysChange(
              Math.max(MIN_RETENTION_DAYS, Number(event.target.value) || MIN_RETENTION_DAYS),
            )
          }
          disabled={confirming || busy}
          className="w-20 rounded-md border border-border bg-transparent px-2 py-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
        />
        {!confirming && (
          <Button
            type="button"
            variant="destructive"
            size="sm"
            onClick={handleRequestDelete}
            disabled={busy}
          >
            {previewMutation.isPending
              ? t("common.checking")
              : t("settings.deleteOld.deleteButton", { days })}
          </Button>
        )}
      </div>

      {confirming && (
        <div className="space-y-2 rounded-md border border-destructive/40 bg-destructive/10 p-2">
          <p className="text-xs text-destructive">
            {t("settings.deleteOld.confirmMessage", {
              n: (previewMutation.data ?? 0).toLocaleString(),
            })}
          </p>
          <div className="flex items-center gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={handleCancel}
              disabled={deleteMutation.isPending}
            >
              {t("common.cancel")}
            </Button>
            <Button
              type="button"
              variant="destructive"
              size="sm"
              onClick={handleConfirmDelete}
              disabled={deleteMutation.isPending}
            >
              {deleteMutation.isPending
                ? t("settings.deleteOld.deleting")
                : t("settings.deleteOld.confirmDeleteButton")}
            </Button>
          </div>
        </div>
      )}

      <p className="min-h-4 text-xs" aria-live="polite">
        {previewMutation.isError && (
          <span className="text-destructive">{t("settings.deleteOld.previewError")}</span>
        )}
        {deleteMutation.isError && (
          <span className="text-destructive">{t("settings.deleteOld.deleteError")}</span>
        )}
        {deleteMutation.isSuccess && (
          <span className="text-muted-foreground">
            {t("settings.deleteOld.successMessage", {
              events: deleteMutation.data.deletedEvents.toLocaleString(),
              streams: deleteMutation.data.deletedStreams.toLocaleString(),
            })}
          </span>
        )}
      </p>
    </div>
  );
}

/** DB 최적화(VACUUM) — 완료 시 다이얼로그 "데이터 통계"도 함께 최신화한다. */
function VacuumDbAction() {
  const t = useT();
  const queryClient = useQueryClient();
  const vacuumMutation = useMutation({
    mutationFn: vacuumDb,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.dbStats() });
    },
  });

  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between gap-2">
        <div className="min-w-0">
          <h4 className="text-sm font-medium">{t("settings.vacuum.heading")}</h4>
          <p className="text-xs text-muted-foreground">{t("settings.vacuum.description")}</p>
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => vacuumMutation.mutate()}
          disabled={vacuumMutation.isPending}
        >
          {vacuumMutation.isPending ? t("settings.vacuum.running") : t("settings.vacuum.button")}
        </Button>
      </div>
      <p className="min-h-4 text-xs" aria-live="polite">
        {vacuumMutation.isError && (
          <span className="text-destructive">{t("settings.vacuum.error")}</span>
        )}
        {vacuumMutation.isSuccess && (
          <span className="text-muted-foreground">
            {formatMegabytes(vacuumMutation.data.beforeBytes)} →{" "}
            {formatMegabytes(vacuumMutation.data.afterBytes)}
          </span>
        )}
      </p>
    </div>
  );
}

interface DataStatsSectionProps {
  /** 설정 다이얼로그가 열려있을 때만 조회한다(다른 useQuery들과 동일한 `enabled` 패턴). */
  enabled: boolean;
}

/** 설정 다이얼로그 "데이터" 섹션 — DB 크기/이벤트·스트림 수/기록 범위(읽기 전용) +
 * 내보내기/기간 삭제/DB 최적화(쓰기 동작) 관리. */
function DataStatsSection({ enabled }: DataStatsSectionProps) {
  const t = useT();
  const { data, isLoading, isError } = useQuery({
    queryKey: queryKeys.dbStats(),
    queryFn: getDbStats,
    enabled,
  });

  return (
    <section aria-label={t("settings.data.heading")} className="space-y-4">
      <div className="space-y-2">
        <h3 className="text-sm font-semibold">{t("settings.data.heading")}</h3>
        {isLoading && <p className="text-xs text-muted-foreground">{t("common.loading")}</p>}
        {isError && <p className="text-xs text-destructive">{t("settings.data.loadError")}</p>}
        {data && (
          <dl className="grid grid-cols-2 gap-y-1 text-xs">
            <dt className="text-muted-foreground">{t("settings.data.dbSize")}</dt>
            <dd className="text-right font-mono">
              {formatMegabytes(data.dbSizeBytes + data.walSizeBytes)}
            </dd>
            <dt className="text-muted-foreground">{t("settings.data.eventsStreams")}</dt>
            <dd className="text-right font-mono">
              {data.events.toLocaleString()} / {data.streams.toLocaleString()}
            </dd>
            <dt className="text-muted-foreground">{t("settings.data.recordRange")}</dt>
            <dd className="text-right font-mono">
              {data.oldestTs !== null && data.newestTs !== null
                ? `${new Date(data.oldestTs).toLocaleDateString()} ~ ${new Date(data.newestTs).toLocaleDateString()}`
                : t("settings.data.noRecords")}
            </dd>
          </dl>
        )}
      </div>

      <div className="space-y-4 border-t border-border pt-3">
        <ExportDataAction />
        <DeleteOldEventsAction />
        <VacuumDbAction />
      </div>
    </section>
  );
}

/** 설정 다이얼로그 좌측 카테고리 네비(슬랙 환경설정 패턴) — 카테고리는 저장 대상(캡처·커넥터)과
 * 즉시 적용형(일반·AI 요약·데이터)으로 나뉜다(하단 footer 노출 조건에 반영). AI 요약은 원래 "일반"
 * 하위 섹션이었지만 설정 항목이 많아져(엔진/모델/자동 생성 등) 별도 카테고리로 분리(실사용 요구). */
type SettingsCategory = "general" | "capture" | "connectors" | "ai" | "data";

const SETTINGS_CATEGORIES: Array<{
  value: SettingsCategory;
  labelKey: UiKey;
  icon: ComponentType<{ className?: string }>;
}> = [
  { value: "general", labelKey: "settings.general.heading", icon: Settings2 },
  { value: "capture", labelKey: "settings.category.capture", icon: Radar },
  { value: "connectors", labelKey: "settings.connectors.heading", icon: Plug },
  { value: "ai", labelKey: "settings.summary.heading", icon: Sparkles },
  { value: "data", labelKey: "settings.data.heading", icon: Database },
];

export function SettingsDialog() {
  const t = useT();
  const settingsOpen = useAppStore((state) => state.settingsOpen);
  const setSettingsOpen = useAppStore((state) => state.setSettingsOpen);
  const queryClient = useQueryClient();

  const [localConfig, setLocalConfig] = useState<CaptureConfig | null>(null);
  const [savedMessageVisible, setSavedMessageVisible] = useState(false);
  const [activeCategory, setActiveCategory] = useState<SettingsCategory>("general");
  const contentScrollRef = useRef<HTMLDivElement>(null);

  const { data, isLoading, isError } = useQuery({
    queryKey: queryKeys.captureConfig(),
    queryFn: getCaptureConfig,
    enabled: settingsOpen,
  });

  useEffect(() => {
    if (settingsOpen && data) {
      setLocalConfig(withConfigDefaults(data.config));
    }
  }, [settingsOpen, data]);

  // 다이얼로그를 열 때마다 항상 "일반" 카테고리부터 보여준다(이전 세션의 선택을 기억하지 않음).
  useEffect(() => {
    if (settingsOpen) {
      setActiveCategory("general");
    }
  }, [settingsOpen]);

  // 카테고리 전환 시 우측 컨텐츠 스크롤 위치를 top으로 리셋한다(effect의 exhaustive-deps 경고를
  // 피하기 위해 전환 핸들러에서 직접 리셋한다 — 렌더 이후 값이 아니라 클릭 시점에 리셋하면 충분).
  function handleCategoryChange(next: SettingsCategory) {
    setActiveCategory(next);
    contentScrollRef.current?.scrollTo({ top: 0 });
  }

  const saveMutation = useMutation({
    mutationFn: (config: CaptureConfig) => setCaptureConfig(config),
    onSuccess: () => {
      setSavedMessageVisible(true);
      queryClient.invalidateQueries({ queryKey: queryKeys.captureConfig() });
    },
  });

  const restartMutation = useMutation({
    mutationFn: () => restartApp(),
  });

  function handleSourceChange(source: CaptureSource, next: CaptureSourceConfig) {
    setLocalConfig((prev) => (prev ? { ...prev, [SOURCE_CONFIG_KEYS[source]]: next } : prev));
    setSavedMessageVisible(false);
  }

  function handleBodyPolicyChange(next: BodyPolicy) {
    setLocalConfig((prev) => (prev ? { ...prev, bodyPolicy: next } : prev));
    setSavedMessageVisible(false);
  }

  function handleExcludeProjectsChange(next: string[]) {
    setLocalConfig((prev) => (prev ? { ...prev, excludeProjects: next } : prev));
    setSavedMessageVisible(false);
  }

  function handleSlackChange(next: SlackConfig) {
    setLocalConfig((prev) => (prev ? { ...prev, slack: next } : prev));
    setSavedMessageVisible(false);
  }

  function handleGithubChange(next: GithubConfig) {
    setLocalConfig((prev) => (prev ? { ...prev, github: next } : prev));
    setSavedMessageVisible(false);
  }

  function handleLinearChange(next: LinearConfig) {
    setLocalConfig((prev) => (prev ? { ...prev, linear: next } : prev));
    setSavedMessageVisible(false);
  }

  function handleNotionChange(next: NotionConfig) {
    setLocalConfig((prev) => (prev ? { ...prev, notion: next } : prev));
    setSavedMessageVisible(false);
  }

  function handleOpenChange(open: boolean) {
    setSettingsOpen(open);
    if (!open) {
      setSavedMessageVisible(false);
      saveMutation.reset();
      restartMutation.reset();
    }
  }

  // 저장 버튼/재시작 안내 footer는 config 저장 대상 카테고리(캡처·커넥터)에서만 노출한다 —
  // 일반/데이터는 각 항목이 즉시 적용되거나 자체 액션 버튼을 갖고 있어 별도 저장이 필요 없다.
  const showSaveFooter = activeCategory === "capture" || activeCategory === "connectors";

  return (
    <Dialog.Root open={settingsOpen} onOpenChange={handleOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-background/80 backdrop-blur-sm" />
        <Dialog.Content className="fixed top-1/2 left-1/2 z-50 flex h-[560px] w-full max-w-3xl -translate-x-1/2 -translate-y-1/2 flex-col overflow-hidden rounded-lg border border-border bg-popover text-popover-foreground shadow-lg">
          <header className="flex shrink-0 items-start justify-between gap-2 border-b border-border px-4 py-3">
            <div className="min-w-0">
              <Dialog.Title className="text-sm font-semibold">
                {t("settings.dialogTitle")}
              </Dialog.Title>
              <Dialog.Description className="mt-0.5 text-xs text-muted-foreground">
                {t("settings.dialogDescription")}
              </Dialog.Description>
            </div>
            <Dialog.Close asChild>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label={t("settings.closeAriaLabel")}
              >
                <X className="size-4" />
              </Button>
            </Dialog.Close>
          </header>

          <div className="flex min-h-0 flex-1">
            <nav
              aria-label={t("settings.categoryNavAriaLabel")}
              className="w-[180px] shrink-0 overflow-y-auto border-r border-border bg-[var(--sidebar-bg)] p-2"
            >
              <ul className="space-y-0.5">
                {SETTINGS_CATEGORIES.map(({ value, labelKey, icon: Icon }) => {
                  const isActive = activeCategory === value;
                  return (
                    <li key={value}>
                      <button
                        type="button"
                        aria-current={isActive ? "true" : undefined}
                        onClick={() => handleCategoryChange(value)}
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
            </nav>

            <div ref={contentScrollRef} className="min-h-0 flex-1 space-y-6 overflow-y-auto p-4">
              {activeCategory === "general" && <GeneralSection enabled={settingsOpen} />}

              {activeCategory === "capture" && (
                <>
                  {isLoading && (
                    <p className="text-sm text-muted-foreground">{t("settings.loading")}</p>
                  )}
                  {isError && <p className="text-sm text-destructive">{t("settings.loadError")}</p>}
                  {localConfig && data && (
                    <>
                      <CaptureSourceSection
                        source="claude_code"
                        config={localConfig.claudeCode}
                        resolvedRoots={data.resolvedRoots}
                        onChange={(next) => handleSourceChange("claude_code", next)}
                      />
                      <CaptureSourceSection
                        source="kiro_cli"
                        config={localConfig.kiroCli}
                        resolvedRoots={data.resolvedRoots}
                        onChange={(next) => handleSourceChange("kiro_cli", next)}
                      />
                      <ExcludeProjectsSection
                        value={localConfig.excludeProjects}
                        onChange={handleExcludeProjectsChange}
                      />
                      <BodyPolicySection
                        value={localConfig.bodyPolicy ?? DEFAULT_BODY_POLICY}
                        onChange={handleBodyPolicyChange}
                      />
                    </>
                  )}
                </>
              )}

              {activeCategory === "connectors" && (
                <>
                  {isLoading && (
                    <p className="text-sm text-muted-foreground">{t("settings.loading")}</p>
                  )}
                  {isError && <p className="text-sm text-destructive">{t("settings.loadError")}</p>}
                  {localConfig && data && (
                    <>
                      <ConnectorsSection
                        value={localConfig.slack}
                        onChange={handleSlackChange}
                        hasExistingToken={Boolean(data.config.slack?.token)}
                      />
                      <GithubConnectorSection
                        value={localConfig.github}
                        onChange={handleGithubChange}
                      />
                      <LinearConnectorSection
                        value={localConfig.linear}
                        onChange={handleLinearChange}
                      />
                      <NotionConnectorSection
                        value={localConfig.notion}
                        onChange={handleNotionChange}
                      />
                    </>
                  )}
                  <ConnectorsComingSoon />
                </>
              )}

              {activeCategory === "ai" && (
                <>
                  <AISummarySection enabled={settingsOpen} />
                  <ReviewProfileSection enabled={settingsOpen} />
                </>
              )}
              {activeCategory === "data" && <DataStatsSection enabled={settingsOpen} />}
            </div>
          </div>

          {showSaveFooter && (
            <footer className="flex shrink-0 items-center justify-between gap-2 border-t border-border px-4 py-3">
              <p className="min-h-4 text-xs" aria-live="polite">
                {saveMutation.isError && (
                  <span className="text-destructive">{t("settings.saveError")}</span>
                )}
                {restartMutation.isError && (
                  <span className="text-destructive">{t("settings.restartError")}</span>
                )}
                {savedMessageVisible && !saveMutation.isError && (
                  <span className="text-muted-foreground">{t("settings.savedNotice")}</span>
                )}
              </p>
              <div className="flex items-center gap-2">
                {savedMessageVisible && (
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => restartMutation.mutate()}
                    disabled={restartMutation.isPending}
                  >
                    {restartMutation.isPending ? t("common.restarting") : t("common.restartNow")}
                  </Button>
                )}
                <Button
                  type="button"
                  variant="default"
                  size="sm"
                  onClick={() => localConfig && saveMutation.mutate(localConfig)}
                  disabled={!localConfig || saveMutation.isPending}
                >
                  {saveMutation.isPending ? t("common.saving") : t("common.save")}
                </Button>
              </div>
            </footer>
          )}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
