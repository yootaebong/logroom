import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef } from "react";
import { useResolvedLocale } from "@/i18n";
import {
  generateDailySummary,
  generatePeriodSummary,
  getDailySummary,
  getPeriodSummary,
  getSummaryConfig,
  queryKeys,
} from "@/lib/api";
import {
  LOCAL_TIMEZONE,
  localDateToMonthKey,
  localDateToWeekKey,
  shiftLocalDate,
  shiftLocalMonth,
  toLocalDateString,
} from "@/store";

/** 자동 생성 체크 주기(30분) — 자정 크론이 아니라 캐치업형이다: 앱(트레이 상주라 창을 닫아도
 * webview는 살아있음)이 깨어 있는 동안 주기적으로 "완료된 기간(어제/지난주/지난달) 요약이 없으면
 * 생성"을 확인하므로, 맥이 꺼져 있던 날도 다음 실행 때 따라잡는다(커넥터 백필과 동일 사상). */
const AUTO_SUMMARY_CHECK_MS = 30 * 60 * 1000;

/**
 * 일일/주간/월간 AI 요약 자동 생성 훅(M7-①/③, "데일리 루틴" 요구) — `summary.enabled &&
 * summary.autoGenerate`일 때 다음 3가지를 각각 확인해 캐시에 없으면 생성한다:
 * ① 어제 날짜(daily) ② 직전 완료 주(weekly) ③ 직전 완료 달(monthly).
 *
 * "완료된" 기간만 다루는 이유는 daily와 동일 — 하루/한 주/한 달이 끝나야 완전한 회고가 되고
 * (ADR-0012 회고 중심), 아침에 앱을 열면 어제/지난주/지난달 요약이 준비돼 있는 스탠드업 시나리오와
 * 맞는다. 주는 항상 월요일 시작 고정이라 "지난주"는 이 주의 월요일에서 -7일로 유일하게 정해지고
 * (요일 분기 불필요), "지난달"도 이번 달 1일에서 -1개월로 유일하게 정해진다. 오늘/이번 주/이번 달
 * 요약은 각 화면(SummaryView)에서 수동 생성.
 *
 * 실패(엔진 에러 등)해도 조용히 넘어가되, 같은 기간+로케일은 앱 세션당 1회만 시도해(attemptedRef)
 * 30분마다 아웃바운드를 반복하지 않는다 — "데이터 없음"(summary_no_data)은 엔진 호출 전에 로컬에서
 * 끝나는 정상 케이스라 비용이 없다. 세 시도는 순차 실행하되 서로 독립적으로 실패해도 나머지에
 * 영향을 주지 않는다.
 */
export function useAutoDailySummary(): void {
  const locale = useResolvedLocale();
  const queryClient = useQueryClient();
  const attemptedRef = useRef<Set<string>>(new Set());

  const { data: config } = useQuery({
    queryKey: queryKeys.summaryConfig(),
    queryFn: getSummaryConfig,
    refetchInterval: AUTO_SUMMARY_CHECK_MS,
  });

  const autoEnabled = config?.enabled === true && config?.autoGenerate === true;

  useEffect(() => {
    if (!autoEnabled) return;

    async function generateDailyIfMissing() {
      const yesterday = shiftLocalDate(toLocalDateString(new Date()), -1);
      const attemptKey = `daily:${yesterday}:${locale}`;
      if (attemptedRef.current.has(attemptKey)) return;
      try {
        const cached = await getDailySummary(yesterday, locale);
        attemptedRef.current.add(attemptKey);
        if (cached) return;
        await generateDailySummary(yesterday, LOCAL_TIMEZONE, locale, false);
        queryClient.invalidateQueries({ queryKey: queryKeys.dailySummary(yesterday, locale) });
      } catch {
        // 자동 경로는 조용히 실패(수동 생성 시 에러 코드가 그대로 안내됨). attemptKey는 이미
        // 기록돼 있어 이 앱 세션에서는 재시도하지 않는다.
      }
    }

    async function generateWeeklyIfMissing() {
      const thisWeekMonday = localDateToWeekKey(toLocalDateString(new Date()));
      const lastWeekKey = shiftLocalDate(thisWeekMonday, -7);
      const attemptKey = `week:${lastWeekKey}:${locale}`;
      if (attemptedRef.current.has(attemptKey)) return;
      try {
        const cached = await getPeriodSummary("week", lastWeekKey, locale);
        attemptedRef.current.add(attemptKey);
        if (cached) return;
        // cascade=true: 빠진 일간을 한 단계 생성(≤7회) — 데일리 루틴이 매일 채워두므로 실제로는
        // 0~2회 수준. 월간과 달리 상한이 작아 자동 경로에서도 허용한다.
        await generatePeriodSummary("week", lastWeekKey, LOCAL_TIMEZONE, locale, false, null, true);
        queryClient.invalidateQueries({
          queryKey: queryKeys.periodSummary("week", lastWeekKey, locale),
        });
      } catch {
        // 조용히 실패 — 위 daily와 동일 원칙.
      }
    }

    async function generateMonthlyIfMissing() {
      const thisMonthKey = localDateToMonthKey(toLocalDateString(new Date()));
      const lastMonthKey = localDateToMonthKey(shiftLocalMonth(`${thisMonthKey}-01`, -1));
      const attemptKey = `month:${lastMonthKey}:${locale}`;
      if (attemptedRef.current.has(attemptKey)) return;
      try {
        const cached = await getPeriodSummary("month", lastMonthKey, locale);
        attemptedRef.current.add(attemptKey);
        if (cached) return;
        // cascade=false(롤업 온리): 자동 경로의 월간은 **캐시된 주간만** 합성한다 — 콜드 스타트에서
        // 월간 1건이 수십 회 엔진 호출로 번지는 팬아웃 방지(리뷰 Critical). 주간이 아직 부족해
        // 실패(no_data)하면 조용히 넘어가고, 주간이 쌓인 뒤의 **다음 앱 세션**에서 재시도된다
        // (attemptedRef가 세션당 1회 가드라 같은 세션 내 재시도는 없음 — 의도된 트레이드오프).
        await generatePeriodSummary(
          "month",
          lastMonthKey,
          LOCAL_TIMEZONE,
          locale,
          false,
          null,
          false,
        );
        queryClient.invalidateQueries({
          queryKey: queryKeys.periodSummary("month", lastMonthKey, locale),
        });
      } catch {
        // 조용히 실패 — 위 daily와 동일 원칙.
      }
    }

    async function runAutoGeneration() {
      await generateDailyIfMissing();
      await generateWeeklyIfMissing();
      await generateMonthlyIfMissing();
    }

    void runAutoGeneration();
    const timer = window.setInterval(runAutoGeneration, AUTO_SUMMARY_CHECK_MS);
    return () => window.clearInterval(timer);
  }, [autoEnabled, locale, queryClient]);
}
