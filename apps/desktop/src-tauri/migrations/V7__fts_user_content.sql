-- LogRoom V7: FTS 인덱싱 범위에 message/note(사용자 콘텐츠) 추가(ADR-0012 보강).
-- V2가 FTS를 type IN ('prompt','response')로 좁힌 취지는 tool 계열(tool_use/tool_result) 노이즈
-- 제외였다. 그런데 이 조건이 message(예: Slack 커넥터가 캡처하는 "내가 쓴 메시지")·note(수동 기록,
-- 향후 도입)까지 함께 배제해버려, 사용자가 직접 작성한 콘텐츠임에도 검색이 안 되는 부작용이 있었다.
-- message/note는 tool 계열이 아니라 prompt/response와 동급의 "사용자 콘텐츠"이므로 재분류한다.
-- V1~V6는 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

-- 1) 기존 message/note 행 재인덱싱. delete-all 후 전체 재구축(V2 방식)은 불필요한 비용이 크므로,
--    이미 인덱싱돼 있는 prompt/response는 그대로 두고 새로 인덱싱 대상이 된 두 타입만 삽입한다.
--    (2026-07 현재 실 DB에는 message/note 행이 0건이라 실질적으로 no-op이지만, 과거 데이터가 있는
--    환경에서도 안전하게 동작하도록 정합성을 맞춰둔다.)
INSERT INTO events_fts(rowid, title, body)
  SELECT rowid, title, body FROM events WHERE type IN ('message', 'note');

-- 2) 트리거 재정의: prompt/response에 message/note를 더해 4종 모두 인덱싱한다.
DROP TRIGGER events_ai;
DROP TRIGGER events_ad;
DROP TRIGGER events_au_del;
DROP TRIGGER events_au_ins;

CREATE TRIGGER events_ai AFTER INSERT ON events WHEN new.type IN ('prompt', 'response', 'message', 'note') BEGIN
  INSERT INTO events_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;
CREATE TRIGGER events_ad AFTER DELETE ON events WHEN old.type IN ('prompt', 'response', 'message', 'note') BEGIN
  INSERT INTO events_fts(events_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
END;
-- UPDATE는 하나의 트리거 WHEN 절에서 old/new 타입을 동시에 분기할 수 없어 delete/insert 트리거로 분리(V2 방식 유지).
CREATE TRIGGER events_au_del AFTER UPDATE ON events WHEN old.type IN ('prompt', 'response', 'message', 'note') BEGIN
  INSERT INTO events_fts(events_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
END;
CREATE TRIGGER events_au_ins AFTER UPDATE ON events WHEN new.type IN ('prompt', 'response', 'message', 'note') BEGIN
  INSERT INTO events_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;
