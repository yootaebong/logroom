import { useEffect } from "react";
import { shiftLocalDate, toLocalDateString, useAppStore } from "@/store";

/** 포커스가 이 요소들 위에 있으면 전역 단축키를 무시한다(문자 입력과 충돌 방지). */
const IGNORED_INPUT_SELECTOR = "input, textarea, [contenteditable], [contenteditable='true']";

/** 날짜 이동/오늘 이동 단축키. */
const KEY_PREV_DAY = "ArrowLeft";
const KEY_NEXT_DAY = "ArrowRight";
const KEY_TODAY = "t";
/** 스트림 이동 단축키(다음/이전). */
const KEY_NEXT_STREAM = "j";
const KEY_PREV_STREAM = "k";
const KEY_CLOSE_DETAIL = "Escape";

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.matches(IGNORED_INPUT_SELECTOR);
}

/**
 * 전역 키보드 단축키(docs/05-ui-ux.md "키보드 우선"):
 * ←/→ 날짜 이동, t 오늘, j/k 스트림 이동(순환 없음), Esc 상세 닫기.
 * 입력 요소 포커스 중이거나 검색/설정/온보딩 다이얼로그가 열려 있으면 무시한다(다이얼로그
 * 내부는 radix가 처리 — 온보딩의 Esc 건너뛰기도 이 가드 덕분에 상세 패널 닫힘과 충돌하지 않는다).
 *
 * ←/→는 AI 요약 뷰가 주간/월간 스코프(M7-③)일 때도 항상 **하루 단위**로만 currentDate를 옮긴다 —
 * 일간 네비(App.tsx 헤더)와 동일한 전역 단축키를 공유하는 의도적 설계다. 주간/월간 스코프의
 * "기간 단위" 이동(±7일/±1개월)은 SummaryView 자체의 ◀ ▶ 버튼이 담당한다.
 */
export function useGlobalKeyboardNav(): void {
  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.isComposing) return;
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      if (isEditableTarget(event.target)) return;

      const { searchOpen, settingsOpen, onboardingOpen } = useAppStore.getState();
      if (searchOpen || settingsOpen || onboardingOpen) return;

      switch (event.key) {
        case KEY_PREV_DAY: {
          const { currentDate, setCurrentDate } = useAppStore.getState();
          setCurrentDate(shiftLocalDate(currentDate, -1));
          break;
        }
        case KEY_NEXT_DAY: {
          const { currentDate, setCurrentDate } = useAppStore.getState();
          setCurrentDate(shiftLocalDate(currentDate, 1));
          break;
        }
        case KEY_TODAY: {
          useAppStore.getState().setCurrentDate(toLocalDateString(new Date()));
          break;
        }
        case KEY_NEXT_STREAM: {
          moveSelection(1);
          break;
        }
        case KEY_PREV_STREAM: {
          moveSelection(-1);
          break;
        }
        case KEY_CLOSE_DETAIL: {
          useAppStore.getState().setSelectedStreamId(null);
          break;
        }
        default:
          return;
      }

      event.preventDefault();
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);
}

/** navigableStreamIds에서 현재 선택된 스트림의 다음/이전으로 이동한다. 순환 없음(끝에서 정지). */
function moveSelection(direction: 1 | -1): void {
  const { navigableStreamIds, selectedStreamId, setSelectedStreamId } = useAppStore.getState();
  if (navigableStreamIds.length === 0) return;

  const currentIndex = selectedStreamId ? navigableStreamIds.indexOf(selectedStreamId) : -1;

  if (currentIndex === -1) {
    const fallbackIndex = direction === 1 ? 0 : navigableStreamIds.length - 1;
    setSelectedStreamId(navigableStreamIds[fallbackIndex] ?? null);
    return;
  }

  const nextIndex = currentIndex + direction;
  if (nextIndex < 0 || nextIndex >= navigableStreamIds.length) return;
  setSelectedStreamId(navigableStreamIds[nextIndex] ?? null);
}
