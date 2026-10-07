-- LogRoom V9: 주간/월간 AI 요약 롤업 캐시(M7-③). period_type('week'|'month') + period_key + locale
-- 조합당 최신 요약 1건만 보관한다(daily_summaries V8과 동일한 upsert 원칙).
-- period_key: week=그 주 월요일 로컬 날짜 'YYYY-MM-DD' / month='YYYY-MM'.
-- V1~V8은 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

CREATE TABLE period_summaries (
  period_type TEXT NOT NULL,
  period_key  TEXT NOT NULL,
  locale      TEXT NOT NULL,
  tz          TEXT NOT NULL,
  engine      TEXT NOT NULL,
  model       TEXT NOT NULL,
  content     TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  PRIMARY KEY (period_type, period_key, locale)
);
