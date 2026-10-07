-- LogRoom V10: "상세 보기" 온디맨드 AI 요약 캐시. 기본 요약(daily_summaries/period_summaries)은
-- 스캔용으로 짧게 유지하는 대신, 사용자가 "상세 보기"를 눌렀을 때만 같은 발췌를 상세용 프롬프트로
-- 재호출해 이 테이블에 별도로 캐시한다(비용은 누를 때만 발생). scope('day'|'week'|'month')로 일간·
-- 주간·월간을 한 테이블에서 커버한다 — scope_key는 day면 로컬 날짜 'YYYY-MM-DD', week·month면
-- 각각의 period_key(V9의 period_key와 동일한 형식)를 그대로 쓴다. (scope, scope_key, locale)
-- 조합당 최신 요약 1건만 보관한다(daily_summaries/period_summaries와 동일한 upsert 원칙).
-- daily_summaries(V8)/period_summaries(V9)는 이 마이그레이션에서 건드리지 않는다.
-- V1~V9는 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

CREATE TABLE summary_details (
  scope      TEXT NOT NULL,
  scope_key  TEXT NOT NULL,
  locale     TEXT NOT NULL,
  tz         TEXT NOT NULL,
  engine     TEXT NOT NULL,
  model      TEXT NOT NULL,
  content    TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (scope, scope_key, locale)
);
