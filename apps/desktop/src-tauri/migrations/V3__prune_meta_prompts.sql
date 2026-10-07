-- LogRoom V3: 슬래시 커맨드 메타/중단 마커가 prompt 이벤트·스트림 title로 저장된 노이즈 소급 정리.
-- 신규 캡처는 src/capture/policy.rs::is_meta_prompt_text + normalize.rs의 isMeta 라인 skip으로 걸러지고,
-- 이 마이그레이션은 그 필터 도입 전에 이미 쌓인 데이터를 같은 패턴 집합으로 정리한다.
-- V1__init.sql/V2__essential_body.sql은 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

-- 1) 노이즈 prompt 이벤트 삭제.
--    FTS는 V2가 정의한 events_ad 트리거(type IN ('prompt','response') WHEN 절)가 자동으로 걷어낸다.
--    LIKE 프리픽스에 SQLite 와일드카드 문자(%, _)가 없어 별도 ESCAPE 처리는 불필요.
DELETE FROM events
WHERE type = 'prompt' AND body IS NOT NULL AND (
      body LIKE '<command-name>%'
   OR body LIKE '<command-message>%'
   OR body LIKE '<command-args>%'
   OR body LIKE '<local-command-stdout>%'
   OR body LIKE '<local-command-caveat>%'
   OR body LIKE '[Request interrupted by user%'
);

-- 2) 노이즈 title(위 패턴으로 시작 — normalize.rs가 첫 prompt 앞 60자를 title로 썼던 값)을
--    1)에서 삭제 후 남은 첫 prompt(ts 기준 최초) 앞 60자로 갱신한다. 남은 prompt가 없으면 NULL.
UPDATE streams
SET title = (
  SELECT substr(e.body, 1, 60)
  FROM events e
  WHERE e.stream_id = streams.id AND e.type = 'prompt' AND e.body IS NOT NULL
  ORDER BY e.ts ASC, e.id ASC
  LIMIT 1
)
WHERE title IS NOT NULL AND (
      title LIKE '<command-name>%'
   OR title LIKE '<command-message>%'
   OR title LIKE '<command-args>%'
   OR title LIKE '<local-command-stdout>%'
   OR title LIKE '<local-command-caveat>%'
   OR title LIKE '[Request interrupted by user%'
);

-- 3) 1)의 삭제로 이벤트가 0개가 된 스트림(노이즈 prompt만 있던 스트림) 제거.
DELETE FROM streams
WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.stream_id = streams.id);

-- 4) 삭제로 경계가 어긋난 남은 스트림들의 시간범위 재계산(db.rs::recalc_stream_bounds와 동일 패턴).
--    3)에서 이벤트 0개 스트림은 이미 삭제됐으므로 COALESCE의 기존값 fallback엔 도달하지 않지만
--    방어적으로 유지한다.
UPDATE streams
SET started_at = COALESCE((SELECT MIN(ts) FROM events e WHERE e.stream_id = streams.id), started_at),
    ended_at   = COALESCE((SELECT MAX(ts) FROM events e WHERE e.stream_id = streams.id), ended_at);
