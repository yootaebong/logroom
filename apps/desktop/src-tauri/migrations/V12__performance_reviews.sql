-- LogRoom V12: 업무평가서 캐시(ADR-0017). period_type('quarter'|'month') + period_key + locale 조합당
-- 최신 평가서 1건만 보관한다(daily_summaries V8·period_summaries V9와 동일한 upsert 원칙).
-- period_key: quarter='YYYY-Qn'(달력 분기) / month='YYYY-MM'.
-- inputs: 이 평가서를 만든 입력(JSON) — 포함 프로젝트(null=전체)·기간 목표·평가 프로필 스냅샷. 같은 기간을
-- 다른 입력으로 다시 만들면 덮어쓰므로, 화면에 "무엇으로 만든 평가서인지"를 보여 주려면 함께 남겨야 한다.
-- stats: 생성 시점의 DB 집계 스냅샷(JSON) — 숫자 타일·활동 추이·프로젝트 비중 차트가 쓴다. 나중에 다시
-- 열어도 AI가 본 숫자와 화면의 숫자가 어긋나지 않도록 조회 시점이 아니라 생성 시점 값을 저장한다.
-- prompt_version: summary/prompts.rs::REVIEW_PROMPT_VERSION(요약의 PROMPT_VERSION과 별개로 관리).
-- V1~V11은 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

CREATE TABLE performance_reviews (
  period_type    TEXT NOT NULL,
  period_key     TEXT NOT NULL,
  locale         TEXT NOT NULL,
  tz             TEXT NOT NULL,
  engine         TEXT NOT NULL,
  model          TEXT NOT NULL,
  content        TEXT NOT NULL,
  inputs         TEXT NOT NULL DEFAULT '{}',
  stats          TEXT NOT NULL DEFAULT '{}',
  prompt_version TEXT NOT NULL DEFAULT '',
  created_at     INTEGER NOT NULL,
  PRIMARY KEY (period_type, period_key, locale)
);
