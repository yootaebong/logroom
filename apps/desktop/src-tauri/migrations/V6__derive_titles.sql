-- LogRoom V6: session 스트림의 "첫 prompt 앞 60자" 하드컷 title을, 신규 캡처와 동일한 휴리스틱
-- (첫 "의미 있는"(15자+) prompt의 첫 줄 앞 60자, capture/policy.rs::derive_stream_title)으로 재계산.
-- 실측(2026-07): session 111개 중 55개가 이 하드컷 title이었고, "ㄱㄱ" 같은 응답성 첫 prompt가
-- 그대로 title이 되거나 60자 하드컷으로 주제가 안 드러나는 문제가 있었다.
--
-- 이 SQL은 Rust의 derive_stream_title과 완전히 동일한 구현일 필요는 없다(근사):
-- - "첫 줄"은 `instr(body, char(10))`로 첫 개행 위치를 찾아 그 앞부분만 사용.
-- - "의미 있음" 판정은 `length(trim(body)) >= 15`(SQLite length()는 TEXT에 대해 문자 수를 센다).
-- - 마크다운 프리픽스 strip은 `ltrim(x, '#>-* ')`(문자셋 반복 strip, 3-인자 ltrim은 SQLite 3.34+).
-- - 60자 절단은 `substr(x, 1, 60)`(V3/V4와 동일하게 '…'는 붙이지 않는다 — 원래 title도 안 붙였음).
--
-- 대상: title이 "그 스트림 첫 prompt의 `substr(replace(body,char(10),' '),1,60)`"과 정확히 일치하는
-- (= normalize.rs의 옛 하드컷 폴백으로 채워진 것으로 보이는) kind='session' 스트림만. ai-title 등으로
-- 채워진 title은 이 값과 다를 것이므로 자연히 제외된다. agent 스트림과 manualTitle(V7 이전엔 항상
-- false지만 방어적으로 가드)이 세팅된 스트림은 건드리지 않는다.
-- V1~V5는 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

UPDATE streams
SET title = (
  SELECT substr(
           trim(
             ltrim(
               CASE
                 WHEN instr(sel.body, char(10)) > 0
                   THEN substr(sel.body, 1, instr(sel.body, char(10)) - 1)
                 ELSE sel.body
               END,
               '#>-* '
             )
           ),
           1, 60
         )
  FROM (
    SELECT e.body AS body
    FROM events e
    WHERE e.stream_id = streams.id
      AND e.type = 'prompt'
      AND e.body IS NOT NULL
      AND length(trim(e.body)) >= 15
    ORDER BY e.ts ASC, e.id ASC
    LIMIT 1
  ) sel
)
WHERE streams.kind = 'session'
  -- json_extract가 키 부재 시 NULL을 반환하면 `NOT(... AND NULL)`도 NULL이 되어(3치 논리) 이
  -- WHERE 절 전체가 매치 실패로 취급된다 — COALESCE로 명시적 falsy(0) 기본값을 줘야 한다.
  AND NOT (json_valid(streams.metadata) AND COALESCE(json_extract(streams.metadata, '$.manualTitle'), 0))
  AND streams.title IS NOT NULL
  AND streams.title = (
    SELECT substr(replace(e.body, char(10), ' '), 1, 60)
    FROM events e
    WHERE e.stream_id = streams.id AND e.type = 'prompt' AND e.body IS NOT NULL
    ORDER BY e.ts ASC, e.id ASC
    LIMIT 1
  )
  AND EXISTS (
    SELECT 1 FROM events e
    WHERE e.stream_id = streams.id
      AND e.type = 'prompt'
      AND e.body IS NOT NULL
      AND length(trim(e.body)) >= 15
  );
