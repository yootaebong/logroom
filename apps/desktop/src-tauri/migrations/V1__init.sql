-- LogRoom 초기 스키마 (docs/02-data-model.md DDL)
-- 통합 Event/Stream 모델. 모든 소스를 이 스키마로 정규화한다.

-- 스트림: 세션/대화 = 병렬 레인 1개
CREATE TABLE streams (
  id          TEXT PRIMARY KEY,               -- "<source>:<native session id>"
  source      TEXT NOT NULL,                  -- claude_code | kiro_cli | kiro_ide | gemini_web | manual
  kind        TEXT NOT NULL DEFAULT 'session',-- session | task | thread | agent
  title       TEXT,                           -- 소스 제공 제목(예: Claude ai-title) 또는 자동 생성
  project     TEXT,                           -- cwd/repo 경로
  git_branch  TEXT,
  started_at  INTEGER NOT NULL,               -- epoch ms (첫 이벤트)
  ended_at    INTEGER,                        -- epoch ms (마지막 이벤트; 진행중이면 갱신)
  status      TEXT NOT NULL DEFAULT 'active', -- active | done
  metadata    TEXT NOT NULL DEFAULT '{}',     -- JSON (소스별 부가정보)
  created_at  INTEGER NOT NULL
);
CREATE INDEX idx_streams_started ON streams(started_at DESC);
CREATE INDEX idx_streams_source  ON streams(source, started_at DESC);
CREATE INDEX idx_streams_project ON streams(project);

-- 이벤트: 스트림 내 개별 활동
CREATE TABLE events (
  id          TEXT PRIMARY KEY,               -- uuid v7 (시간정렬 가능)
  stream_id   TEXT REFERENCES streams(id) ON DELETE CASCADE,
  ts          INTEGER NOT NULL,               -- epoch ms
  source      TEXT NOT NULL,
  type        TEXT NOT NULL,                  -- prompt | response | tool_use | tool_result | message | file_edit | note
  title       TEXT,                           -- 리스트 표시용 짧은 요약
  body        TEXT,                           -- 원문(프롬프트/응답/내용)
  model       TEXT,                           -- 예: claude-sonnet-4-5
  tokens_in   INTEGER,
  tokens_out  INTEGER,
  url         TEXT,
  parent_id   TEXT,                           -- 스레드 트리(부모 이벤트 id)
  external_id TEXT,                           -- 소스 native uuid (dedup 키)
  metadata    TEXT NOT NULL DEFAULT '{}',
  created_at  INTEGER NOT NULL,
  UNIQUE(source, external_id)                 -- 재수집 idempotent
);
CREATE INDEX idx_events_ts        ON events(ts DESC);
CREATE INDEX idx_events_stream    ON events(stream_id, ts);
CREATE INDEX idx_events_source_ts ON events(source, ts DESC);
CREATE INDEX idx_events_type      ON events(type);

-- 전문검색: FTS5 external content. CJK 부분검색 위해 trigram 토크나이저.
CREATE VIRTUAL TABLE events_fts USING fts5(
  title, body,
  content='events', content_rowid='rowid',
  tokenize='trigram'
);
CREATE TRIGGER events_ai AFTER INSERT ON events BEGIN
  INSERT INTO events_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;
CREATE TRIGGER events_ad AFTER DELETE ON events BEGIN
  INSERT INTO events_fts(events_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
END;
CREATE TRIGGER events_au AFTER UPDATE ON events BEGIN
  INSERT INTO events_fts(events_fts, rowid, title, body) VALUES('delete', old.rowid, old.title, old.body);
  INSERT INTO events_fts(rowid, title, body) VALUES (new.rowid, new.title, new.body);
END;

-- 캡처 진행상태(파일 감시 오프셋 등, backfill용)
CREATE TABLE capture_cursors (
  source     TEXT NOT NULL,
  resource   TEXT NOT NULL,      -- 파일 경로 등
  offset     INTEGER NOT NULL DEFAULT 0,
  mtime      INTEGER,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (source, resource)
);
