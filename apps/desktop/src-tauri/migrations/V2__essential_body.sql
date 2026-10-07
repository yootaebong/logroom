-- LogRoom V2: 회고 중심 저장(essential body policy, ADR-0012) 소급 적용 + FTS 축소.
-- prompt/response(핵심 가치)만 전문 유지·검색하고, tool 계열은 요약만 남겨 DB 용량을 줄인다.
-- V1__init.sql은 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

-- 1) 기존 tool 계열 데이터 다이어트.
--    tool_result body: 256자(코드포인트, substr은 char 단위라 UTF-8/CJK 안전) 초과분 절단.
UPDATE events
SET body = substr(body, 1, 256) || '…'
WHERE type = 'tool_result' AND body IS NOT NULL AND length(body) > 256;

--    tool_use metadata: input 전문 제거(신규 캡처부터는 절단된 inputPreview만 남기지만,
--    소급 데이터는 즉시 용량을 줄이기 위해 완전 삭제한다).
--    metadata 컬럼은 스키마상 TEXT일 뿐 JSON 유효성이 DB 레벨에서 강제되지 않으므로
--    (예: 과거 버그·수동 조작으로 비JSON 값이 들어간 행) json_valid로 먼저 걸러
--    json_remove/json_type 호출이 그런 행에서 에러를 내며 마이그레이션 전체를 실패시키지 않게 한다.
UPDATE events
SET metadata = json_remove(metadata, '$.input')
WHERE json_valid(metadata)
  AND type = 'tool_use' AND json_type(metadata, '$.input') IS NOT NULL;

-- 2) FTS: prompt/response만 인덱싱하도록 전체 재구축.
INSERT INTO events_fts(events_fts) VALUES('delete-all');
INSERT INTO events_fts(rowid, title, body)
  SELECT rowid, title, body FROM events WHERE type IN ('prompt', 'response');

-- 3) 트리거 재정의: 이제부터 prompt/response 타입만 FTS에 반영한다(그 외 타입은 인덱싱 안 함).
DROP TRIGGER events_ai;
DROP TRIGGER events_ad;
DROP TRIGGER events_au;

CREATE TRIGGER events_ai AFTER INSERT ON events WHEN new.type IN ('prompt', 'response') BEGIN
  INSERT INTO events_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;
CREATE TRIGGER events_ad AFTER DELETE ON events WHEN old.type IN ('prompt', 'response') BEGIN
  INSERT INTO events_fts(events_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
END;
-- UPDATE는 하나의 트리거 WHEN 절에서 old/new 타입을 동시에 분기할 수 없어 delete/insert 트리거로 분리한다.
CREATE TRIGGER events_au_del AFTER UPDATE ON events WHEN old.type IN ('prompt', 'response') BEGIN
  INSERT INTO events_fts(events_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
END;
CREATE TRIGGER events_au_ins AFTER UPDATE ON events WHEN new.type IN ('prompt', 'response') BEGIN
  INSERT INTO events_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;
