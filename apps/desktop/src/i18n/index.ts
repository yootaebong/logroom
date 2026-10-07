// LogRoom 데스크톱 i18n 훅 — store의 `locale`(사용자 선택값, "system" 포함)을 구독해 실제
// 표시 언어를 해석하고, 파라미터 치환을 지원하는 `t()`를 반환한다(랜딩 `i18n/utils.ts`의
// `useTranslations` 패턴을 zustand store 구독형으로 이식).

import { useAppStore } from "@/store";
import type { ResolvedLocale, UiKey } from "./ui";
import { ui } from "./ui";

export type { ResolvedLocale, UiKey } from "./ui";

/** `t(key, params)` 시그니처 — 컴포넌트가 아닌 순수 헬퍼 함수(포맷터 등)에 그대로 넘겨 재사용한다. */
export type Translate = (key: UiKey, params?: Record<string, string | number>) => string;

/** store의 `locale`("system"/"en"/"ko")을 실제 표시 언어("en"/"ko")로 해석한다. "system"이면
 * OS 언어(`navigator.language`)가 한국어 계열이면 "ko", 그 외에는 "en"으로 폴백한다. */
export function resolveLocale(locale: "system" | "en" | "ko"): ResolvedLocale {
  if (locale === "en" || locale === "ko") return locale;
  return navigator.language.startsWith("ko") ? "ko" : "en";
}

/** `{name}` 플레이스홀더를 params 값으로 치환한다. 매치되는 param이 없으면 원문 그대로 둔다. */
function interpolate(template: string, params?: Record<string, string | number>): string {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (match, key: string) =>
    key in params ? String(params[key]) : match,
  );
}

/** 현재 store locale이 해석하는 실제 표시 언어("en"/"ko"). 날짜 포맷 등 `t()` 없이 로케일
 * 분기만 필요한 곳(예: Sidebar.tsx `formatSidebarDate`)에서 쓴다. */
export function useResolvedLocale(): ResolvedLocale {
  const locale = useAppStore((state) => state.locale);
  return resolveLocale(locale);
}

/** 번역 함수를 반환하는 훅. store의 locale이 바뀌면 구독 중인 컴포넌트가 리렌더된다. */
export function useT(): Translate {
  const resolved = useResolvedLocale();
  return (key, params) => interpolate(ui[resolved][key], params);
}
