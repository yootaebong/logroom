-- LogRoom V13: FTS UPDATE 트리거를 title/body 변경으로만 좁힌다.
-- hub 세션 재귀속(capture/hub.rs)은 이벤트를 레포별 자식 스트림으로 옮기면서 `stream_id` 만 바꾸는
-- UPDATE 를 대량으로 실행한다. V7 트리거는 어떤 컬럼이 바뀌든 FTS 행을 지우고 다시 넣어(재색인)
-- 불필요한 비용이 컸다. 검색 대상(title/body)이 바뀔 때만 재색인하도록 `UPDATE OF title, body` 로 제한한다.
-- 본문은 V7(V7__fts_user_content.sql)과 같다 — 컬럼 제한만 추가. V1~V12 는 refinery 규칙상 수정 금지.

DROP TRIGGER events_au_del;
DROP TRIGGER events_au_ins;

-- UPDATE는 하나의 트리거 WHEN 절에서 old/new 타입을 동시에 분기할 수 없어 delete/insert 트리거로 분리(V2 방식 유지).
CREATE TRIGGER events_au_del AFTER UPDATE OF title, body ON events WHEN old.type IN ('prompt', 'response', 'message', 'note') BEGIN
  INSERT INTO events_fts(events_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
END;
CREATE TRIGGER events_au_ins AFTER UPDATE OF title, body ON events WHEN new.type IN ('prompt', 'response', 'message', 'note') BEGIN
  INSERT INTO events_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;
