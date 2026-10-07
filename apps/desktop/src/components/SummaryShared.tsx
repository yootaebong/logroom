import { useQuery } from "@tanstack/react-query";
import { Dialog } from "radix-ui";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import type { UiKey } from "@/i18n";
import { useT } from "@/i18n";
import {
  previewPerformanceReviewInput,
  previewPeriodSummaryInput,
  previewResumeInput,
  previewSummaryInput,
  queryKeys,
} from "@/lib/api";
import type { PeriodType, ReviewPeriodType } from "@/lib/types";
import { cn } from "@/lib/utils";
import { LOCAL_TIMEZONE } from "@/store";

/** SummaryView.tsx/ReviewPanel.tsx 공용 조각 — 요약 본문 파싱·렌더, 에러 코드 매핑, "전송 내용
 * 미리보기" 다이얼로그. 원래 SummaryView.tsx에 있던 것을 평가서(ReviewPanel.tsx)에서도 그대로
 * 재사용하기 위해 이 파일로 옮겼다(내용은 PreviewTarget의 "review" 케이스 추가를 제외하면 동일). */

/** BE 안정 에러 코드 → i18n 키(리뷰 지적 — "데이터 없음" 같은 정상 상태를 진짜 에러와 구분).
 * daily/period/resume/review 커맨드가 같은 코드 집합을 공유하므로(lib.rs) 매핑 테이블도 그대로
 * 재사용한다. 미지의 코드/런타임 에러(CLI 실패·HTTP 등)는 호출부 fallback 키로 폴백한다. */
export const SUMMARY_ERROR_KEYS: Record<string, UiKey> = {
  summary_no_data: "summary.card.errorNoData",
  summary_disabled: "summary.card.errorDisabled",
  summary_paused: "summary.card.errorPaused",
  summary_no_engine: "summary.card.errorNoEngine",
  summary_api_key_missing: "summary.card.errorApiKeyMissing",
  summary_timeout: "summary.card.errorTimeout",
};

export function summaryErrorKey(error: unknown, fallback: UiKey): UiKey {
  return SUMMARY_ERROR_KEYS[String(error)] ?? fallback;
}

/** 재개 스코프 전용 no_data 안내 — daily/period와 문구가 다르므로("이 프로젝트는 마지막 활동
 * 이후 7일간 활동 없음") SUMMARY_ERROR_KEYS의 summary_no_data만 여기서 오버라이드한다(구
 * ResumeBriefingDialog.tsx의 resumeErrorKey를 이 뷰로 이전). 그 외(disabled/paused/no_engine/
 * api_key_missing)는 공용 매핑을 그대로 쓴다. */
export function resumeErrorKey(
  error: unknown,
  fallback: UiKey = "resume.dialog.generateError",
): UiKey {
  if (isNoDataError(error)) return "resume.dialog.errorNoData";
  return summaryErrorKey(error, fallback);
}

/** "데이터 없음"은 정상적인 빈 상태 — destructive가 아닌 muted 톤으로 표시한다. */
export function isNoDataError(error: unknown): boolean {
  return String(error) === "summary_no_data";
}

/** 요약 본문의 섹션 1개 — `## `(또는 `### `) 제목 라인 아래 `- ` 불릿들이 이어지고, 그 아래
 * `> `로 시작하는 정리·평가 줄(`notes`, 프롬프트 v6, summary/prompts.rs 참고)이 붙을 수 있는
 * 구조. 제목 없는 구버전 캐시(v1·v2)는 heading=null 섹션 하나로 렌더된다. 프롬프트 v6부터는 첫
 * `## ` 이전에 오는 총평도 heading=null 섹션의 notes로 들어온다(items는 비어 있음). */
export interface SummarySection {
  heading: string | null;
  /** `## `면 2, `### `면 3(프롬프트 v7 주제 계층, summary/prompts.rs 참고) — 3은 2보다 한 단계
   * 안쪽 주제다. heading=null인 총평 섹션과 `### `가 없는 구버전 캐시는 전부 2. */
  level: 2 | 3;
  items: string[];
  /** `> `로 시작하는 정리·평가 줄들(프롬프트 v6). 구버전 캐시(v1~v5)에는 없어 빈 배열. 프롬프트
   * v8부터 level 3(`### ` 주제) 섹션의 `notes[0]`는 "상태만" 20자 내외로 압축된 한 줄이라
   * `SummaryContent`가 제목 옆에 인라인으로 렌더한다(`notes[1..]`는 여전히 blockquote). */
  notes: string[];
}

/** `## `/`### ` 제목 + `- ` 불릿 + `> ` 정리 줄 구조를 섹션으로 파싱한다(라인 기반). `- `/`> `
 * 어느 쪽도 아닌 일반 텍스트 라인은 직전에 추가된 줄이 속했던 쪽(불릿이면 items, 정리 줄이면
 * notes)에 이어붙인다. 프롬프트 v4(프로젝트별 재편, summary/prompts.rs 참고)는 각 프로젝트를
 * `## {프로젝트명}` 섹션으로 출력하고, v6(정리·평가 줄 신설)은 그 아래에 `> ` 정리 줄을, 첫
 * `## ` 이전에는 총평을 `> ` 줄로 둔다 — 이 총평 섹션은 heading=null이고 items가 비어 있으므로
 * 그런 섹션도 결과에 포함시킨다. v7(주제 계층)부터는 `### {주제명}`도 새 섹션을 시작하되
 * level: 3으로 표시한다(같은 프로젝트 `## ` 섹션 아래 한 단계 안쪽 주제). 구버전 캐시(v1~v6,
 * `## 한눈에`/`## 자세히` 포함, `### ` 없음)는 전부 level 2라 기존과 동일하게 동작한다. 재개
 * 브리핑(M7-②)의 `## 요약`/`## 진행 중·미완결`/`## 다음 할 일` 3섹션도 마찬가지다
 * (ResumeBriefingDialog.tsx). 프롬프트 v8이 출력 맨 아래에 추가하는 `## 정리`/`## Summary`
 * 섹션도 그냥 `## ` heading + `- ` 불릿의 평범한 level 2 섹션이라 별도 파싱 분기가 없다. */
export function parseSummaryContent(content: string): SummarySection[] {
  const sections: SummarySection[] = [];
  let current: SummarySection = { heading: null, level: 2, items: [], notes: [] };
  // 직전 일반 텍스트 라인이 이어붙을 대상 — 불릿(`- `) 다음이면 "items", 정리 줄(`> `) 다음이면
  // "notes". 새 섹션(`## `/`### `)을 만나면 초기화한다.
  let lastTarget: "items" | "notes" | null = null;
  for (const line of content.split("\n")) {
    if (line.startsWith("### ") || line.startsWith("## ")) {
      if (current.heading !== null || current.items.length > 0 || current.notes.length > 0) {
        sections.push(current);
      }
      const level: 2 | 3 = line.startsWith("### ") ? 3 : 2;
      current = { heading: line.slice(level === 3 ? 4 : 3).trim(), level, items: [], notes: [] };
      lastTarget = null;
      continue;
    }
    if (line.startsWith("- ")) {
      current.items.push(line.slice(2).trim());
      lastTarget = "items";
      continue;
    }
    if (line.startsWith("> ")) {
      current.notes.push(line.slice(2).trim());
      lastTarget = "notes";
      continue;
    }
    const trimmed = line.trim();
    if (trimmed && lastTarget === "notes") {
      current.notes[current.notes.length - 1] += ` ${trimmed}`;
    } else if (trimmed && lastTarget === "items") {
      current.items[current.items.length - 1] += ` ${trimmed}`;
    } else if (trimmed) {
      current.items.push(trimmed);
      lastTarget = "items";
    }
  }
  if (current.heading !== null || current.items.length > 0 || current.notes.length > 0) {
    sections.push(current);
  }
  return sections;
}

/** 구버전(v3) 캐시의 "한눈에" 목차 섹션 heading — 이 문자열일 때만 목차 톤(촘촘·중간 강조)으로
 * 렌더한다. 프롬프트 v4(프로젝트별 재편)부터는 섹션 heading이 프로젝트명(`## logroom` 등)이라
 * "첫 섹션"이라는 위치 기반 판정을 쓰면 첫 프로젝트가 잘못 목차 톤이 된다 — heading 문자열 자체로
 * 판정해야 v3 구버전 캐시 호환과 v4 신규 캐시 둘 다 올바르게 렌더된다(리뷰 지적). */
const TOC_HEADINGS = new Set(["한눈에", "At a glance"]);

/** 파싱된 요약 본문 렌더 — 구버전(v3) "한눈에" 섹션만 목차 톤(촘촘·중간 강조)으로, 그 외 모든
 * 섹션(v4 프로젝트별 섹션·v3 "자세히"·재개 브리핑 3섹션)은 읽기 톤(느슨한 행간)으로 렌더한다.
 * `notes`(프롬프트 v6 정리·평가 줄)는 기본적으로 불릿 목록 아래 인용 블록(`<blockquote>`, 좌측
 * 보더)으로 렌더한다 — heading=null이고 items가 비어 있으면 총평이므로 제목 없이 상단에 그대로
 * 낸다. **level 3(프롬프트 v7 `### ` 주제) 예외(프롬프트 v8)**: 줄 수를 늘리지 않기 위해
 * `notes[0]`(v8부터 "상태만" 20자 내외로 압축된 한 줄)를 제목(`<h4>`) 오른쪽에 인라인
 * (`text-xs text-muted-foreground font-normal`)으로 붙이고, `notes[1..]`가 있으면(드묾) 그것만
 * 기존처럼 blockquote로 렌더한다 — level 2 섹션과 heading 없는 섹션(하루 총평)은 이 예외 대상이
 * 아니라 여전히 notes 전부를 blockquote로 낸다. `## 정리`/`## Summary`(프롬프트 v8) 섹션은
 * level 2라 별도 처리 없이 기존 렌더를 그대로 탄다. level 3은 `pl-3`로 들여쓰고 제목 스타일도
 * level 2(uppercase muted)와 다르게(작게·본문 색) 렌더해 상하 관계를 시각적으로 드러낸다.
 * `export`: ResumeBriefingDialog.tsx도 동일한 `## `/`- ` 라인 구조를 그대로 재사용한다(재개
 * 브리핑 출력에는 `### `/`> ` 줄이 없어 전부 level 2·notes 빈 배열로 기존과 동일하게 렌더된다). */
export function SummaryContent({ content }: { content: string }) {
  const sections = parseSummaryContent(content);
  return (
    <div className="mt-4 space-y-5">
      {sections.map((section, sectionIndex) => {
        const isToc = section.heading !== null && TOC_HEADINGS.has(section.heading);
        // 제목도 불릿도 없이 notes만 있는 섹션 = 총평(프롬프트 v6, 첫 `## ` 이전 `> ` 줄).
        const isOverviewOnly = section.heading === null && section.items.length === 0;
        const isSubsection = section.level === 3;
        // level 3(`### ` 주제)만 첫 note를 제목 옆 인라인 상태로 빼낸다(프롬프트 v8 — "상태만"
        // 짧게 쓰도록 지시된 줄이라 줄 수를 늘리지 않고 제목 옆에 붙일 수 있다). 나머지 note(있으면)
        // 는 그대로 blockquote로 렌더한다.
        const inlineNote = isSubsection ? section.notes[0] : undefined;
        const blockquoteNotes = isSubsection ? section.notes.slice(1) : section.notes;
        return (
          <section
            // 서로 다른 섹션이 같은 heading 문자열을 가질 수 있어(예: 여러 프로젝트에 같은 이름의
            // `### ` 주제) key에 sectionIndex를 함께 조합한다(리뷰 지적). 섹션 배열은 매번
            // content 문자열 전체를 다시 파싱해 생성되므로(재정렬 없음) index 사용이 안전하다.
            // biome-ignore lint/suspicious/noArrayIndexKey: heading이 중복될 수 있어 index를 보조키로 조합, 배열은 매 렌더 전체 재생성이라 재정렬 없음.
            key={`${section.heading ?? "plain"}-${sectionIndex}`}
            className={cn(isSubsection && "pl-3")}
          >
            {section.heading &&
              (isSubsection ? (
                <h4 className="mb-2 flex items-baseline gap-2 text-sm font-medium text-foreground">
                  <span>{section.heading}</span>
                  {inlineNote && (
                    <span className="text-xs font-normal text-muted-foreground">{inlineNote}</span>
                  )}
                </h4>
              ) : (
                <h3 className="mb-2 text-xs font-semibold tracking-wide text-muted-foreground uppercase">
                  {section.heading}
                </h3>
              ))}
            {!isOverviewOnly && (
              <ul className={cn("space-y-1.5", isToc && "space-y-1")}>
                {section.items.map((item) => (
                  <li
                    key={item}
                    className={cn("flex gap-2 text-sm", isToc ? "font-medium" : "leading-relaxed")}
                  >
                    <span aria-hidden="true" className="shrink-0 text-muted-foreground">
                      •
                    </span>
                    <span className="min-w-0">{item}</span>
                  </li>
                ))}
              </ul>
            )}
            {blockquoteNotes.length > 0 && (
              <blockquote
                className={cn(
                  "space-y-1 border-l-2 border-border pl-3 text-sm text-muted-foreground",
                  !isOverviewOnly && "mt-2",
                )}
              >
                {blockquoteNotes.map((note) => (
                  <p key={note}>{note}</p>
                ))}
              </blockquote>
            )}
          </section>
        );
      })}
    </div>
  );
}

/** 미리보기 다이얼로그가 조회할 대상 — 일간(localDate)/기간(periodType+periodKey)/재개(project)/
 * 평가서(review, periodType+periodKey+projects+goals) 중 하나. BE 커맨드가 다르므로
 * (preview_summary_input vs preview_period_summary_input vs preview_resume_input vs
 * preview_performance_review_input) 여기서만 분기하고, 다이얼로그 자체(에디터·리셋·생성 버튼)는
 * 최대한 재사용한다. 재개(resume)는 캐시가 없어 "수정본으로 재생성" 개념이 없으므로(구
 * ResumeBriefingDialog.tsx의 PreviewInputDialog와 동일하게) 에디터 대신 읽기 전용 `<pre>`로 표시하고
 * 생성 버튼 자체를 숨긴다. review는 day/period와 동일하게 에디터+생성 버튼을 보여준다 — 입력
 * (프로젝트·목표)이 바뀌면 발췌가 달라지므로 쿼리키에 함께 넣는다(queryKeys.performanceReviewPreview). */
export type PreviewTarget =
  | { kind: "day"; localDate: string }
  | { kind: "period"; periodType: PeriodType; periodKey: string }
  | { kind: "resume"; project: string }
  | {
      kind: "review";
      periodType: ReviewPeriodType;
      periodKey: string;
      /** 포함할 프로젝트 표시명. `null` = 전체. */
      projects: string[] | null;
      /** 기간 목표. `null` = 입력 없음. */
      goals: string | null;
    };

interface PreviewDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  target: PreviewTarget;
  locale: string;
  /** 에디터에서 수정한 발췌로 생성 요청 — 호출부(SummaryView/ReviewPanel)가 mutation을 쏘고
   * 다이얼로그를 닫는다. target이 resume이면 이 다이얼로그는 에디터/생성 버튼을 렌더하지 않으므로
   * 호출되지 않는다. */
  onGenerate: (excerptOverride: string) => void;
  generatePending: boolean;
}

/** "전송 내용 미리보기·수정" 다이얼로그 — 발췌 텍스트를 조회해 **에디터(textarea)**로 보여준다.
 * 사용자가 민감 내용을 지우거나 맥락을 추가한 뒤 "이 내용으로 요약 생성"을 누르면 수정본(1회성,
 * 저장 안 됨)으로 생성한다. 다이얼로그가 열려있을 때만 조회(SettingsDialog.tsx의
 * `enabled: dialogOpen` 패턴), 열 때마다 원본 발췌로 초기화된다. 기간(주간/월간)·평가서(분기/월간)
 * 발췌는 BE가 캐스케이드 생성까지 수행한 뒤 내려주므로("미리보기 = 실제 전송본" 계약) 일간과 조회
 * 시간 체감이 다를 수 있다(isLoading 문구 재사용). 재개(resume) 발췌는 캐시가 없어 수정 후
 * 재생성(override)할 필요가 없다고 판단해 읽기 전용 `<pre>`로만 보여준다. */
export function PreviewDialog({
  open,
  onOpenChange,
  target,
  locale,
  onGenerate,
  generatePending,
}: PreviewDialogProps) {
  const t = useT();
  const isResume = target.kind === "resume";
  const [draft, setDraft] = useState<string | null>(null);
  const queryKey =
    target.kind === "day"
      ? queryKeys.summaryPreview(target.localDate, locale)
      : target.kind === "period"
        ? queryKeys.periodSummaryPreview(target.periodType, target.periodKey, locale)
        : target.kind === "review"
          ? queryKeys.performanceReviewPreview(
              target.periodType,
              target.periodKey,
              locale,
              target.projects,
              target.goals,
            )
          : queryKeys.resumeBriefingPreview(target.project, locale);
  const { data, isLoading, isError, error } = useQuery({
    queryKey,
    queryFn: () => {
      if (target.kind === "day")
        return previewSummaryInput(target.localDate, LOCAL_TIMEZONE, locale);
      if (target.kind === "period")
        return previewPeriodSummaryInput(
          target.periodType,
          target.periodKey,
          LOCAL_TIMEZONE,
          locale,
        );
      if (target.kind === "review")
        return previewPerformanceReviewInput(
          target.periodType,
          target.periodKey,
          LOCAL_TIMEZONE,
          locale,
          target.projects,
          target.goals,
        );
      return previewResumeInput(target.project, LOCAL_TIMEZONE, locale);
    },
    enabled: open,
  });

  // 열 때마다(그리고 대상/데이터가 바뀌어 data가 갱신되면) 원본 발췌로 초기화 — 이전 세션의
  // 수정본이 남아 있으면 "지금 전송될 내용"이라는 미리보기 계약이 깨진다. resume은 에디터 자체가
  // 없어(draft를 쓰지 않음) 이 초기화가 실질적 의미는 없지만, 훅 순서를 스코프별로 분기하지 않기
  // 위해 그대로 둔다.
  useEffect(() => {
    setDraft(open ? (data ?? null) : null);
  }, [open, data]);

  const edited = draft !== null && data !== undefined && draft !== data;

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-background/80 backdrop-blur-sm" />
        <Dialog.Content className="fixed top-1/2 left-1/2 z-50 flex h-[560px] w-full max-w-2xl -translate-x-1/2 -translate-y-1/2 flex-col overflow-hidden rounded-lg border border-border bg-popover text-popover-foreground shadow-lg">
          <header className="flex shrink-0 items-start justify-between gap-2 border-b border-border px-4 py-3">
            <div className="min-w-0">
              <Dialog.Title className="text-sm font-semibold">
                {t(
                  isResume ? "resume.dialog.previewDialogTitle" : "summary.card.previewDialogTitle",
                )}
              </Dialog.Title>
              {!isResume && (
                <Dialog.Description className="mt-0.5 text-xs text-muted-foreground">
                  {t("summary.card.previewEditableHint")}
                </Dialog.Description>
              )}
            </div>
            <Dialog.Close asChild>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label={t("summary.card.closePreviewAriaLabel")}
              >
                ✕
              </Button>
            </Dialog.Close>
          </header>
          <div className="flex min-h-0 flex-1 flex-col overflow-y-auto p-4">
            {isLoading && <p className="text-sm text-muted-foreground">{t("common.loading")}</p>}
            {isError && (
              <p
                className={cn(
                  "text-sm",
                  isNoDataError(error) ? "text-muted-foreground" : "text-destructive",
                )}
              >
                {t(
                  isResume
                    ? resumeErrorKey(error, "resume.dialog.previewLoadError")
                    : summaryErrorKey(error, "summary.card.previewLoadError"),
                )}
              </p>
            )}
            {isResume
              ? data && (
                  <pre className="whitespace-pre-wrap font-mono text-xs leading-relaxed">
                    {data}
                  </pre>
                )
              : draft !== null && (
                  <textarea
                    value={draft}
                    onChange={(event) => setDraft(event.target.value)}
                    aria-label={t("summary.card.previewEditorAriaLabel")}
                    spellCheck={false}
                    className="min-h-0 w-full flex-1 resize-none rounded-md border border-border bg-transparent p-3 font-mono text-xs leading-relaxed outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  />
                )}
          </div>
          {!isResume && (
            <footer className="flex shrink-0 items-center justify-between gap-2 border-t border-border px-4 py-3">
              <p className="text-xs text-muted-foreground">
                {edited ? t("summary.card.previewEditedNotice") : ""}
              </p>
              <div className="flex items-center gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={!edited}
                  onClick={() => setDraft(data ?? null)}
                >
                  {t("summary.card.previewResetButton")}
                </Button>
                <Button
                  type="button"
                  variant="default"
                  size="sm"
                  disabled={draft === null || draft.trim() === "" || generatePending}
                  onClick={() => {
                    if (draft !== null) onGenerate(draft);
                  }}
                >
                  {generatePending
                    ? t("summary.card.generating")
                    : t("summary.card.generateFromPreviewButton")}
                </Button>
              </div>
            </footer>
          )}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
