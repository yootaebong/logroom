-- LogRoom V4: 시스템 주입 prompt 변종(백그라운드 에이전트 완료 알림 등)이 prompt 이벤트·스트림
-- title로 저장된 노이즈 소급 정리. V3(슬래시 커맨드 메타/중단 마커)와 동일한 4단계 구조를
-- 그대로 재사용하되 패턴만 다르다. 신규 캡처는 src/capture/policy.rs::is_meta_prompt_text(V4 추가분)
-- 로 걸러진다. `[Image: ...`로 시작하는 사용자 첨부 이미지 프롬프트는 필터/정리 대상이 아니다.
-- V1~V3는 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

-- 1) 노이즈 prompt 이벤트 삭제.
--    FTS는 V2가 정의한 events_ad 트리거(type IN ('prompt','response') WHEN 절)가 자동으로 걷어낸다.
--    `<ide_opened_file>`은 LIKE 와일드카드 `_`(임의의 단일 문자 매치)를 리터럴 언더스코어 2개로
--    포함하므로 ESCAPE로 리터럴 처리해야 한다(그렇지 않으면 의도치 않게 더 넓게 매치될 수 있음).
--    나머지 프리픽스는 %, _가 없어 별도 ESCAPE가 불필요하다.
DELETE FROM events
WHERE type = 'prompt' AND body IS NOT NULL AND (
      body LIKE '<task-notification>%'
   OR body LIKE '<teammate-message%'
   OR body LIKE '<fork-boilerplate>%'
   OR body LIKE '[structured-output-enforce]%'
   OR body LIKE '[SYSTEM NOTIFICATION%'
   OR body LIKE '<ide\_opened\_file>%' ESCAPE '\'
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
      title LIKE '<task-notification>%'
   OR title LIKE '<teammate-message%'
   OR title LIKE '<fork-boilerplate>%'
   OR title LIKE '[structured-output-enforce]%'
   OR title LIKE '[SYSTEM NOTIFICATION%'
   OR title LIKE '<ide\_opened\_file>%' ESCAPE '\'
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
