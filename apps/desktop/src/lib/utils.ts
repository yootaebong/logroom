import type { Source } from "@logroom/core";
import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";
import type { Translate } from "@/i18n";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** source별 표시 라벨 — 브랜드명(Claude Code/Kiro CLI/Kiro IDE/Gemini/Slack/GitHub/Linear)은
 * 로케일과 무관하게 그대로 쓰고, "manual"(수동 기록)만 번역한다(Timeline/DigestView/CaptureHealthBadge 공용). */
const SOURCE_BRAND_LABELS: Partial<Record<Source, string>> = {
  claude_code: "Claude Code",
  kiro_cli: "Kiro CLI",
  kiro_ide: "Kiro IDE",
  gemini_web: "Gemini",
  slack: "Slack",
  github: "GitHub",
  linear: "Linear",
  notion: "Notion",
};

export function sourceLabel(source: Source, t: Translate): string {
  return SOURCE_BRAND_LABELS[source] ?? t("common.source.manual");
}

/** 메시지형 커넥터 소스(발화·편집 등 점 이벤트 중심) — 세션형(claude/kiro)과 렌더 규칙이 다르다:
 * 프롬프트 카운트가 무의미하고, 타임라인은 이벤트 구간을 바로 그린다(Timeline/DigestView 공용).
 * Notion은 발화가 아니라 페이지 편집이지만 렌더 규칙은 같다 — started_at~ended_at이 "지속 작업"이
 * 아니라 첫/마지막 편집 시각이라, 세션형으로 그리면 한 페이지를 몇 달간 붙잡고 있는 것처럼 보인다. */
export const MESSAGE_SOURCES: readonly Source[] = ["slack", "github", "linear", "notion"];

/** streams.metadata.parentStream(문자열)을 안전하게 추출한다 — agent 스트림의 부모 세션 id
 * (docs/02-data-model.md "서브에이전트" 참고). */
export function getParentStreamId(metadata: Record<string, unknown>): string | null {
  const value = metadata.parentStream;
  return typeof value === "string" && value.length > 0 ? value : null;
}

/** streams.metadata.hub === true 인지 — 중간 에이전트(agent hub 등)가 레포 밖 폴더에서 띄운 Claude 세션(hub 세션)과,
 * 거기서 턴마다 실제 레포로 나눠 옮긴 자식 스트림에 붙는다(src-tauri capture/hub.rs). */
export function isHubStream(metadata: Record<string, unknown>): boolean {
  return metadata.hub === true;
}

/** project 문자열에서 마지막 `/` 세그먼트(표시 이름)를 뽑는다 — 소스별로 project 키 형식이 달라도
 * (Claude 절대경로 `/Users/…/logroom` vs GitHub `owner/repo`) 표시 이름 단위로 묶기 위한 공용 로직
 * (projectDisplayName/uniqueProjectNames/DigestView 섹션 병합 공용). */
export function lastPathSegment(project: string): string {
  const segments = project.split("/").filter(Boolean);
  return segments[segments.length - 1] ?? project;
}

/** project 문자열 목록에서 null/빈 값을 제외하고 표시 이름(마지막 세그먼트) 기준으로 중복 제거 +
 * 정렬한다(뷰 필터 select 옵션용). 필터 단위는 project 원본 키가 아니라 표시 이름이다 — 같은
 * 저장소가 소스별로 다른 project 키를 갖더라도(위 lastPathSegment 참고) 하나의 필터 항목으로
 * 묶인다. */
export function uniqueProjectNames(projects: Array<string | null>): string[] {
  const set = new Set<string>();
  for (const project of projects) {
    if (project) set.add(lastPathSegment(project));
  }
  return [...set].sort();
}

/** 프로젝트 경로에서 마지막 세그먼트만 강조 표시하기 위해 분리한다. 전체 경로는 title 속성으로 노출.
 * `unknownLabel`은 project가 null(프로젝트 미상)일 때 표시할 문구 — 호출부(컴포넌트)가
 * `t("common.unknownProject")`로 번역해 넘긴다(이 함수는 컴포넌트가 아니라 hooks를 쓸 수 없다). */
export function projectDisplayName(
  project: string | null,
  unknownLabel: string,
): { label: string; full: string } {
  if (!project) return { label: unknownLabel, full: unknownLabel };
  return { label: lastPathSegment(project), full: project };
}

/** stream/digest의 project 원본 키가 뷰 필터의 표시 이름(`ViewFilters.project`)과 일치하는지
 * 판정한다(Timeline `matchesViewFilters`/DigestView 필터 공용). project가 null이면 표시 이름이
 * 없으므로 항상 불일치로 본다 — `filters.project`는 항상 실제 표시 이름(null 아님, 호출부에서
 * 이미 null 분기 처리)이라 이 경우 false가 맞다. */
export function projectMatchesFilter(project: string | null, filterDisplayName: string): boolean {
  if (!project) return false;
  return lastPathSegment(project) === filterDisplayName;
}
