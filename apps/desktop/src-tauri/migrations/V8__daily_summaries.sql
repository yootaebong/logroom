-- LogRoom V8: 일일 AI 요약 캐시(M7-①, ADR-0016). 날짜(local_date) + 로케일(locale) 조합당 최신
-- 요약 1건만 보관한다(재생성 시 upsert로 덮어씀 — 여러 로케일로 재생성해도 각각 독립 캐시).
-- V1~V7은 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

CREATE TABLE daily_summaries (
  local_date TEXT NOT NULL,
  locale TEXT NOT NULL,
  tz TEXT NOT NULL,
  engine TEXT NOT NULL,
  model TEXT NOT NULL,
  content TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (local_date, locale)
);
