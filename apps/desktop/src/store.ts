import type { Source } from "@logroom/core";
import { create } from "zustand";
import type { UpdateInfo } from "@/lib/types";

/** 로컬 날짜(YYYY-MM-DD) 포맷 — Date -> "하루" 경계 계산의 기준 문자열. */
const DATE_PART_LENGTH = 2;

export function toLocalDateString(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(DATE_PART_LENGTH, "0");
  const day = String(date.getDate()).padStart(DATE_PART_LENGTH, "0");
  return `${year}-${month}-${day}`;
}

/** currentDate 문자열을 days만큼 이동한 새 로컬 날짜 문자열을 반환한다. */
export function shiftLocalDate(localDate: string, days: number): string {
  const [year, month, day] = localDate.split("-").map(Number);
  const shifted = new Date(year ?? 1970, (month ?? 1) - 1, day ?? 1);
  shifted.setDate(shifted.getDate() + days);
  return toLocalDateString(shifted);
}

/** currentDate 문자열을 months만큼 이동한 새 로컬 날짜 문자열을 반환한다(월간 스코프 ◀ ▶ 네비,
 * SummaryView.tsx). 일(day)이 대상 달의 마지막 날보다 크면(예: 1/31 + 1개월) 그 달의 마지막 날로
 * 클램프한다 — `Date.setMonth`의 자동 롤오버(1/31 + 1개월 → 3/3)를 막기 위함. 이동 후 periodKey는
 * `localDateToMonthKey`로 다시 뽑으므로 day 값 자체는 중요하지 않지만, currentDate는 일간 뷰와
 * 공유하는 전역 상태라 예측 가능한 날짜를 유지한다. */
export function shiftLocalMonth(localDate: string, months: number): string {
  const [year, month, day] = localDate.split("-").map(Number);
  const targetFirst = new Date(year ?? 1970, (month ?? 1) - 1 + months, 1);
  const lastDayOfTargetMonth = new Date(
    targetFirst.getFullYear(),
    targetFirst.getMonth() + 1,
    0,
  ).getDate();
  targetFirst.setDate(Math.min(day ?? 1, lastDayOfTargetMonth));
  return toLocalDateString(targetFirst);
}

/** 로컬 날짜(YYYY-MM-DD)가 속한 주의 월요일 날짜 문자열(주간 periodKey, 주 시작=월요일 고정 —
 * BE `summary/period.rs::week_range`와 동일 규칙). `getDay()`는 일요일=0이라 별도 보정한다. */
export function localDateToWeekKey(localDate: string): string {
  const [year, month, day] = localDate.split("-").map(Number);
  const date = new Date(year ?? 1970, (month ?? 1) - 1, day ?? 1);
  const dayOfWeek = date.getDay(); // 0=일 .. 6=토
  const daysSinceMonday = dayOfWeek === 0 ? 6 : dayOfWeek - 1;
  date.setDate(date.getDate() - daysSinceMonday);
  return toLocalDateString(date);
}

/** 로컬 날짜(YYYY-MM-DD)가 속한 달의 periodKey("YYYY-MM", BE `period.rs::month_range`와 동일 형식). */
export function localDateToMonthKey(localDate: string): string {
  return localDate.slice(0, 7);
}

/** 한 분기의 달 수 — 분기 ◀ ▶ 네비는 `shiftLocalMonth(date, ±QUARTER_MONTHS)`로 이동한다. */
export const QUARTER_MONTHS = 3;

/** 로컬 날짜(YYYY-MM-DD)가 속한 분기의 periodKey("YYYY-Qn", 1~3월=Q1 … 10~12월=Q4 — BE
 * `summary/review.rs::quarter_range`와 동일 형식). 회계연도가 아니라 달력 분기다. */
export function localDateToQuarterKey(localDate: string): string {
  const [year, month] = localDate.split("-").map(Number);
  const quarter = Math.floor(((month ?? 1) - 1) / QUARTER_MONTHS) + 1;
  return `${year}-Q${quarter}`;
}

/** OS 로컬 타임존(docs/02 "저장은 절대시각, 표시는 조회 시점 TZ" 기준). */
export const LOCAL_TIMEZONE = Intl.DateTimeFormat().resolvedOptions().timeZone;

/** 메인 화면 뷰 전환(타임라인/다이제스트/AI 요약). */
export type AppView = "timeline" | "digest" | "summary";

/** 3종 테마(다크/라이트/웜 라이트, index.css `.dark`/`:root`/`.theme-warm`). 기본값 = "dark". */
export type Theme = "dark" | "light" | "warm";

/** localStorage 키 — index.html의 FOUC 방지 인라인 스크립트도 동일한 키 문자열을 참조하므로
 * 값을 바꿀 때 index.html도 함께 수정해야 한다. */
const THEME_STORAGE_KEY = "logroom-theme";

function isTheme(value: string | null): value is Theme {
  return value === "dark" || value === "light" || value === "warm";
}

/** 저장된 테마 없거나 값이 유효하지 않으면 기본 "dark"로 폴백한다. */
function getInitialTheme(): Theme {
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
  return isTheme(stored) ? stored : "dark";
}

/** 앱 언어 설정(i18n/ui.ts). "system"이면 OS 언어(`navigator.language`)를 따른다 —
 * 실제 표시 언어로의 해석은 `i18n/index.ts::resolveLocale`가 담당한다. */
export type Locale = "system" | "en" | "ko";

/** localStorage 키 — 테마(THEME_STORAGE_KEY)와 동일한 persist 패턴. */
const LOCALE_STORAGE_KEY = "logroom-locale";

function isLocale(value: string | null): value is Locale {
  return value === "system" || value === "en" || value === "ko";
}

/** 저장된 언어 설정 없거나 값이 유효하지 않으면 기본 "system"으로 폴백한다. */
function getInitialLocale(): Locale {
  const stored = window.localStorage.getItem(LOCALE_STORAGE_KEY);
  return isLocale(stored) ? stored : "system";
}

/** localStorage 키 — 앱 시작 시 열 최초 뷰(설정 다이얼로그에서 지정, 테마 persist 패턴과 동일).
 * 미지정이면 "digest"로 폴백한다(기존 기본값 유지). */
const DEFAULT_VIEW_STORAGE_KEY = "logroom-default-view";

function isAppView(value: string | null): value is AppView {
  return value === "timeline" || value === "digest" || value === "summary";
}

/** 저장된 최초 뷰 설정 — 앱 마운트 시 `view` 초기값으로 쓴다. 미저장/무효면 "digest". */
function getInitialView(): AppView {
  const stored = window.localStorage.getItem(DEFAULT_VIEW_STORAGE_KEY);
  return isAppView(stored) ? stored : "digest";
}

/** localStorage 키 — 첫 실행 온보딩 완료 여부(테마 persist 패턴과 동일, config.json 아님).
 * OnboardingDialog.tsx의 완료/건너뛰기(Esc·바깥 클릭 포함) 시 "true"로 기록된다. */
const ONBOARDED_STORAGE_KEY = "logroom-onboarded";

/** 온보딩 완료 기록이 없으면(첫 실행) true — 마운트 시 온보딩 다이얼로그를 자동으로 연다. */
function getInitialOnboardingOpen(): boolean {
  return window.localStorage.getItem(ONBOARDED_STORAGE_KEY) !== "true";
}

/** 타임라인/다이제스트 공용 뷰 필터(docs/05-ui-ux.md ① 상단 "소스/프로젝트/타입 필터" 중 소스+프로젝트).
 * `sources`가 빈 배열이면 전체 소스, `project`가 null이면 전체 프로젝트를 의미한다.
 * `project`는 stream/digest의 project 원본 키가 아니라 **표시 이름**(마지막 `/` 세그먼트,
 * `lib/utils.ts::projectDisplayName`/`uniqueProjectNames` 참고)이다 — 소스마다 project 키 형식이
 * 달라도(Claude 절대경로 vs GitHub `owner/repo`) 표시 이름이 같으면 같은 필터로 묶인다. */
export interface ViewFilters {
  sources: Source[];
  project: string | null;
}

const INITIAL_VIEW_FILTERS: ViewFilters = { sources: [], project: null };

interface AppState {
  /** 우측 상세 패널에 표시할 스트림 id. */
  selectedStreamId: string | null;
  /** ⌘K 검색 다이얼로그 열림 상태. */
  searchOpen: boolean;
  /** 설정(캡처 root) 다이얼로그 열림 상태. */
  settingsOpen: boolean;
  /** 첫 실행 온보딩 다이얼로그 열림 상태. 최초 마운트 시 localStorage 미기록이면 true로 시작하고,
   * 설정 다이얼로그의 "온보딩 다시 보기"로도 재오픈된다(완료 플래그는 유지). */
  onboardingOpen: boolean;
  /** 타임라인이 조회하는 로컬 날짜(YYYY-MM-DD). 기본값 = 오늘. */
  currentDate: string;
  /** 메인 화면에 표시할 뷰. 초기값 = 설정된 최초 뷰(`defaultView`, 없으면 "digest"). */
  view: AppView;
  /** 앱 시작 시 열 최초 뷰(설정 다이얼로그에서 지정) — localStorage에 persist(테마 패턴). `view`는
   * 세션 중 자유롭게 바뀌지만 이 값은 "다음 실행 시 열 뷰"로 고정된다. */
  defaultView: AppView;
  /** 현재 적용 중인 테마(다크/라이트/웜 라이트). localStorage에 persist된다. */
  theme: Theme;
  /** 현재 앱 언어 설정("system"/"en"/"ko"). localStorage에 persist된다(테마와 동일 패턴).
   * 실제 표시 언어(resolved locale)는 `i18n/index.ts::resolveLocale`가 계산한다. */
  locale: Locale;
  /** 현재 뷰에 화면 표시 순서대로 등록된 세션 스트림 id 목록(키보드 j/k 이동용).
   * Timeline/DigestView가 렌더 데이터 확정 시 덮어쓴다(docs/05-ui-ux.md "키보드 우선"). */
  navigableStreamIds: string[];
  /** 현재 뷰가 조회 중인 날짜에 실제 존재하는 고유 프로젝트 **표시 이름** 목록(프로젝트 필터 select
   * 옵션용, `viewFilters.project`와 동일하게 표시 이름 기준). Timeline/DigestView가 렌더 데이터
   * 확정 시 덮어쓴다(navigableStreamIds와 동일 패턴). */
  dayProjects: string[];
  /** 타임라인/다이제스트 공용 소스+프로젝트 필터(App.tsx 헤더에서 조작). */
  viewFilters: ViewFilters;
  /** 백그라운드/수동 업데이트 확인으로 발견된 새 버전(M5, ADR-0013). 없으면 `null` — 헤더 설정
   * 버튼의 작은 배지와 설정 다이얼로그 "업데이트" 섹션이 같은 값을 공유한다(단일 소스). */
  updateAvailable: UpdateInfo | null;
  setSelectedStreamId: (id: string | null) => void;
  setSearchOpen: (open: boolean) => void;
  setSettingsOpen: (open: boolean) => void;
  setOnboardingOpen: (open: boolean) => void;
  setCurrentDate: (date: string) => void;
  setView: (view: AppView) => void;
  setDefaultView: (view: AppView) => void;
  setTheme: (theme: Theme) => void;
  setLocale: (locale: Locale) => void;
  setNavigableStreamIds: (ids: string[]) => void;
  setDayProjects: (projects: string[]) => void;
  setViewFilterSources: (sources: Source[]) => void;
  setViewFilterProject: (project: string | null) => void;
  resetViewFilters: () => void;
  setUpdateAvailable: (info: UpdateInfo | null) => void;
}

export const useAppStore = create<AppState>((set) => ({
  selectedStreamId: null,
  searchOpen: false,
  settingsOpen: false,
  onboardingOpen: getInitialOnboardingOpen(),
  currentDate: toLocalDateString(new Date()),
  view: getInitialView(),
  defaultView: getInitialView(),
  theme: getInitialTheme(),
  locale: getInitialLocale(),
  navigableStreamIds: [],
  dayProjects: [],
  viewFilters: INITIAL_VIEW_FILTERS,
  updateAvailable: null,
  setSelectedStreamId: (id) => set({ selectedStreamId: id }),
  setSearchOpen: (open) => set({ searchOpen: open }),
  setSettingsOpen: (open) => set({ settingsOpen: open }),
  setOnboardingOpen: (open) => {
    // 닫힐 때(완료든 Esc/바깥 클릭으로 건너뛰든)만 완료 플래그를 기록한다 — 재열람으로 다시 열 때는
    // 이미 "true"라 재기록해도 멱등하다.
    if (!open) {
      window.localStorage.setItem(ONBOARDED_STORAGE_KEY, "true");
    }
    set({ onboardingOpen: open });
  },
  setCurrentDate: (date) => set({ currentDate: date }),
  setView: (view) => set({ view }),
  setDefaultView: (view) => {
    window.localStorage.setItem(DEFAULT_VIEW_STORAGE_KEY, view);
    set({ defaultView: view });
  },
  setTheme: (theme) => {
    window.localStorage.setItem(THEME_STORAGE_KEY, theme);
    set({ theme });
  },
  setLocale: (locale) => {
    window.localStorage.setItem(LOCALE_STORAGE_KEY, locale);
    set({ locale });
  },
  setNavigableStreamIds: (ids) => set({ navigableStreamIds: ids }),
  setDayProjects: (projects) => set({ dayProjects: projects }),
  setViewFilterSources: (sources) =>
    set((state) => ({ viewFilters: { ...state.viewFilters, sources } })),
  setViewFilterProject: (project) =>
    set((state) => ({ viewFilters: { ...state.viewFilters, project } })),
  resetViewFilters: () => set({ viewFilters: INITIAL_VIEW_FILTERS }),
  setUpdateAvailable: (info) => set({ updateAvailable: info }),
}));
