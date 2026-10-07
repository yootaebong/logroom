-- LogRoom V11: 프롬프트 버전 컬럼 추가(캐시 갱신 판정용). 주간/월간 롤업이 daily_summaries 캐시를
-- 이어붙여 만드는데, 캐시가 있으면 프롬프트 버전과 무관하게 무조건 재사용해 구버전 일간 캐시가
-- 신형 프롬프트로 재생성한 주간/월간에 섞여 들어가는 문제가 있었다(prompts.rs::PROMPT_VERSION
-- 참고). 이 컬럼으로 각 캐시 행이 어느 프롬프트 버전으로 생성됐는지 기록하고, 현재 버전과 다르면
-- "갱신 대상"으로 분류한다(summary::is_stale_prompt_version). 기존 행은 기본값 ''(버전 미상)이 되어
-- 자연히 갱신 대상으로 분류된다. daily_summaries/period_summaries뿐 아니라 summary_details(V10)도
-- 함께 ALTER해 세 캐시 테이블의 스키마를 일관되게 유지한다.
-- V1~V10은 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

ALTER TABLE daily_summaries ADD COLUMN prompt_version TEXT NOT NULL DEFAULT '';
ALTER TABLE period_summaries ADD COLUMN prompt_version TEXT NOT NULL DEFAULT '';
ALTER TABLE summary_details ADD COLUMN prompt_version TEXT NOT NULL DEFAULT '';
