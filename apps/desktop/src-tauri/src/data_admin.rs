//! 데이터 관리 커맨드(설정 다이얼로그 "데이터" 섹션): 내보내기 · 기간 삭제 · DB 최적화.
//! docs/02-data-model.md "저장 규모·보존·내보내기" 절 참고.
//!
//! 커맨드 자체는 이미 별도 스레드에서 실행되므로(Tauri 동기 커맨드) `spawn_blocking`은 불필요하다.
//! export는 대량(15만+ 행) 테이블을 다루므로 prepared statement로 한 행씩 스트리밍 write하고,
//! 전체를 `Vec`에 모으지 않는다(메모리 적재 금지).

use crate::query::{parse_metadata, total_db_size_bytes};
use rusqlite::{params, Connection};
use serde_json::json;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

/// `checkpoint_truncate_or_err`의 재시도 횟수(최초 시도 제외) — 총 시도 = 1 + 이 값.
const CHECKPOINT_RETRY_COUNT: u32 = 3;
/// 재시도 사이 대기 시간.
const CHECKPOINT_RETRY_DELAY: Duration = Duration::from_millis(200);

/// `export_data` 결과(FE 계약 camelCase, lib.rs가 JSON으로 재포장).
pub struct ExportSummary {
    pub dir: PathBuf,
    pub streams: i64,
    pub events: i64,
}

/// `delete_events_before` 결과.
pub struct DeleteSummary {
    pub deleted_events: i64,
    pub deleted_streams: i64,
}

/// `vacuum_db` 결과.
pub struct VacuumSummary {
    pub before_bytes: u64,
    pub after_bytes: u64,
}

/// `streams` 테이블 전체를 NDJSON(한 줄에 한 행, camelCase)으로 스트리밍 write. 반환값 = 쓴 행 수.
fn export_streams_ndjson(conn: &Connection, path: &Path) -> anyhow::Result<i64> {
    let file = File::create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let mut writer = BufWriter::new(file);

    let mut stmt = conn.prepare(
        "SELECT id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at
         FROM streams ORDER BY started_at ASC",
    )?;
    let mut rows = stmt.query([])?;

    let mut count = 0i64;
    while let Some(row) = rows.next()? {
        let metadata_raw: String = row.get("metadata")?;
        let value = json!({
            "id": row.get::<_, String>("id")?,
            "source": row.get::<_, String>("source")?,
            "kind": row.get::<_, String>("kind")?,
            "title": row.get::<_, Option<String>>("title")?,
            "project": row.get::<_, Option<String>>("project")?,
            "gitBranch": row.get::<_, Option<String>>("git_branch")?,
            "startedAt": row.get::<_, i64>("started_at")?,
            "endedAt": row.get::<_, Option<i64>>("ended_at")?,
            "status": row.get::<_, String>("status")?,
            "metadata": parse_metadata(&metadata_raw),
            "createdAt": row.get::<_, i64>("created_at")?,
        });
        serde_json::to_writer(&mut writer, &value)?;
        writer.write_all(b"\n")?;
        count += 1;
    }
    writer.flush()?;
    Ok(count)
}

/// `events` 테이블 전체를 NDJSON(한 줄에 한 행, camelCase)으로 스트리밍 write. 반환값 = 쓴 행 수.
fn export_events_ndjson(conn: &Connection, path: &Path) -> anyhow::Result<i64> {
    let file = File::create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let mut writer = BufWriter::new(file);

    let mut stmt = conn.prepare(
        "SELECT id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out,
                url, parent_id, external_id, metadata, created_at
         FROM events ORDER BY ts ASC",
    )?;
    let mut rows = stmt.query([])?;

    let mut count = 0i64;
    while let Some(row) = rows.next()? {
        let metadata_raw: String = row.get("metadata")?;
        let value = json!({
            "id": row.get::<_, String>("id")?,
            "streamId": row.get::<_, String>("stream_id")?,
            "ts": row.get::<_, i64>("ts")?,
            "source": row.get::<_, String>("source")?,
            "type": row.get::<_, String>("type")?,
            "title": row.get::<_, Option<String>>("title")?,
            "body": row.get::<_, Option<String>>("body")?,
            "model": row.get::<_, Option<String>>("model")?,
            "tokensIn": row.get::<_, Option<i64>>("tokens_in")?,
            "tokensOut": row.get::<_, Option<i64>>("tokens_out")?,
            "url": row.get::<_, Option<String>>("url")?,
            "parentId": row.get::<_, Option<String>>("parent_id")?,
            "externalId": row.get::<_, Option<String>>("external_id")?,
            "metadata": parse_metadata(&metadata_raw),
            "createdAt": row.get::<_, i64>("created_at")?,
        });
        serde_json::to_writer(&mut writer, &value)?;
        writer.write_all(b"\n")?;
        count += 1;
    }
    writer.flush()?;
    Ok(count)
}

/// WAL 체크포인트를 실행하고 결과를 검증한다. `PRAGMA wal_checkpoint(TRUNCATE)`는
/// `(busy, log, checkpointed)`를 반환하는데, `busy != 0`이면 다른 커넥션이 WAL을 쓰고 있어
/// 체크포인트가 완전히 끝나지 않았다는 뜻이다 — 이 상태로 DB 파일을 복사하면 사본이 최신
/// 변경분을 누락할 수 있다. 짧게 재시도하고 그래도 busy면 export를 중단시킨다.
fn checkpoint_truncate_or_err(conn: &Connection) -> anyhow::Result<()> {
    for attempt in 0..=CHECKPOINT_RETRY_COUNT {
        let (busy, _log, _checkpointed): (i64, i64, i64) =
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        if busy == 0 {
            return Ok(());
        }
        if attempt < CHECKPOINT_RETRY_COUNT {
            thread::sleep(CHECKPOINT_RETRY_DELAY);
        }
    }
    anyhow::bail!("다른 프로세스가 DB 사용 중 — 내보내기 중단")
}

/// 전체 데이터를 `export_dir`(이미 타임스탬프까지 포함된 최종 디렉터리 경로)에 내보낸다:
/// `streams.jsonl` + `events.jsonl`(NDJSON) + `logroom.db` 사본. 대량 테이블을 다루므로
/// 각 파일은 prepared statement로 한 행씩 스트리밍 write한다(전체 메모리 적재 금지).
/// `export_dir`을 인자로 받는 이유는 테스트에서 tempdir을 주입하기 위함 — 실제 경로 계산
/// (`~/Downloads/logroom-export-<timestamp>/`)은 lib.rs 커맨드 핸들러가 담당한다.
/// 실패 시(중간에 에러 발생) 부분적으로 생성된 `export_dir`을 정리한 뒤 에러를 전파한다
/// (정리 자체의 실패는 무시 — 원래 에러를 가리지 않기 위함).
pub fn export_data(conn: &Connection, db_path: &Path, export_dir: &Path) -> anyhow::Result<ExportSummary> {
    export_data_inner(conn, db_path, export_dir).inspect_err(|_| {
        let _ = fs::remove_dir_all(export_dir);
    })
}

fn export_data_inner(conn: &Connection, db_path: &Path, export_dir: &Path) -> anyhow::Result<ExportSummary> {
    fs::create_dir_all(export_dir)?;
    fs::set_permissions(export_dir, fs::Permissions::from_mode(0o700))?;

    let streams = export_streams_ndjson(conn, &export_dir.join("streams.jsonl"))?;
    let events = export_events_ndjson(conn, &export_dir.join("events.jsonl"))?;

    // WAL의 최신 변경분까지 반영된 사본을 만들기 위해 복사 전 체크포인트(db.rs와 동일 이유로
    // 트랜잭션 밖에서 실행 — VACUUM/checkpoint는 SQLite 제약상 트랜잭션 안에서 불가). 사본
    // 무결성이 목적이므로 busy로 끝나면(다른 프로세스가 WAL을 쓰는 중) 복사하지 않고 중단한다.
    checkpoint_truncate_or_err(conn)?;
    let db_copy_path = export_dir.join("logroom.db");
    fs::copy(db_path, &db_copy_path)?;
    fs::set_permissions(&db_copy_path, fs::Permissions::from_mode(0o600))?;

    Ok(ExportSummary {
        dir: export_dir.to_path_buf(),
        streams,
        events,
    })
}

/// 삭제 미리보기: `ts` 이전(배타적 상한, `< ts`) 이벤트 수. FE가 "이벤트 X건이 영구 삭제됩니다"
/// 확인 문구에 사용한다.
pub fn count_events_before(conn: &Connection, ts: i64) -> anyhow::Result<i64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE ts < ?1", params![ts], |r| r.get(0))?;
    Ok(count)
}

/// `ts` 이전 이벤트를 영구 삭제한다(수동 실행 전용 — 자동 보존 정책 없음). 순서: events 삭제
/// (FTS는 V2가 정의한 트리거로 자동 반영) → 이벤트가 0개가 된 빈 스트림 삭제 → 남은 스트림들의
/// 시간범위(started_at/ended_at) 재계산 — migrations/V3__prune_meta_prompts.sql과 동일 패턴을
/// 커맨드에서 재사용. 하나의 트랜잭션으로 원자성을 보장하고, 커밋 후에만 체크포인트한다.
pub fn delete_events_before(conn: &mut Connection, ts: i64) -> anyhow::Result<DeleteSummary> {
    let tx = conn.transaction()?;

    let deleted_events = tx.execute("DELETE FROM events WHERE ts < ?1", params![ts])? as i64;

    let deleted_streams = tx.execute(
        "DELETE FROM streams WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.stream_id = streams.id)",
        [],
    )? as i64;

    tx.execute(
        "UPDATE streams
         SET started_at = COALESCE((SELECT MIN(ts) FROM events e WHERE e.stream_id = streams.id), started_at),
             ended_at   = COALESCE((SELECT MAX(ts) FROM events e WHERE e.stream_id = streams.id), ended_at)",
        [],
    )?;

    tx.commit()?;

    // 커밋 후 체크포인트(트랜잭션 밖 — db.rs::open_and_migrate_at와 동일 이유).
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;

    Ok(DeleteSummary {
        deleted_events,
        deleted_streams,
    })
}

/// DB 최적화: `VACUUM` + `wal_checkpoint(TRUNCATE)`(둘 다 트랜잭션 밖에서 실행 — SQLite 제약).
/// 전/후 파일 크기(본체 + WAL)를 재서 FE가 "208MB → 195MB" 형식으로 보여줄 수 있게 한다.
pub fn vacuum_db(conn: &Connection, db_path: &Path) -> anyhow::Result<VacuumSummary> {
    let before_bytes = total_db_size_bytes(db_path);

    conn.execute_batch("VACUUM;")?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;

    let after_bytes = total_db_size_bytes(db_path);

    Ok(VacuumSummary { before_bytes, after_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rusqlite::params;
    use std::io::{BufRead, BufReader};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// `query.rs` 테스트와 동일 패턴 — `tempfile` 크레이트 없이 임시 디렉토리를 만든다.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!(
                "logroom-data-admin-test-{}-{n}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn db_path(&self) -> PathBuf {
            self.path.join("logroom.db")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn seeded_conn() -> (TempDir, Connection) {
        let tmp = TempDir::new();
        let conn = db::open_and_migrate_at(&tmp.db_path()).expect("마이그레이션 성공");
        (tmp, conn)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_stream(conn: &Connection, id: &str, title: Option<&str>, started_at: i64, ended_at: i64) {
        conn.execute(
            "INSERT INTO streams
               (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES (?1, 'claude_code', 'session', ?2, NULL, NULL, ?3, ?4, 'active', '{}', ?3)",
            params![id, title, started_at, ended_at],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_event(conn: &Connection, id: &str, stream_id: &str, ts: i64, event_type: &str, external_id: &str) {
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES (?1, ?2, ?3, 'claude_code', ?4, NULL, '본문', NULL, NULL, NULL, NULL, NULL, ?5, '{}', ?3)",
            params![id, stream_id, ts, event_type, external_id],
        )
        .unwrap();
    }

    fn read_ndjson_lines(path: &Path) -> Vec<serde_json::Value> {
        let file = File::open(path).unwrap();
        BufReader::new(file)
            .lines()
            .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
            .collect()
    }

    // ── export_data ─────────────────────────────────────────────

    #[test]
    fn export_data_writes_ndjson_files_and_db_copy_with_matching_counts() {
        let (tmp, conn) = seeded_conn();
        insert_stream(&conn, "claude_code:sess-1", Some("세션 1"), 1_000, 2_000);
        insert_stream(&conn, "claude_code:sess-2", Some("세션 2"), 3_000, 4_000);
        insert_event(&conn, "ev-1", "claude_code:sess-1", 1_000, "prompt", "ext-1");
        insert_event(&conn, "ev-2", "claude_code:sess-1", 2_000, "response", "ext-2");
        insert_event(&conn, "ev-3", "claude_code:sess-2", 3_000, "prompt", "ext-3");

        let export_dir = tmp.path.join("export-out");
        let summary = export_data(&conn, &tmp.db_path(), &export_dir).expect("export 성공");

        assert_eq!(summary.streams, 2);
        assert_eq!(summary.events, 3);
        assert_eq!(summary.dir, export_dir);

        let streams_lines = read_ndjson_lines(&export_dir.join("streams.jsonl"));
        assert_eq!(streams_lines.len(), 2, "파일 행 수가 반환값과 일치해야 함");
        assert_eq!(streams_lines[0]["id"], json!("claude_code:sess-1"));
        assert_eq!(streams_lines[0]["gitBranch"], serde_json::Value::Null);
        assert_eq!(streams_lines[0]["metadata"], json!({}));

        let events_lines = read_ndjson_lines(&export_dir.join("events.jsonl"));
        assert_eq!(events_lines.len(), 3, "파일 행 수가 반환값과 일치해야 함");
        assert_eq!(events_lines[0]["id"], json!("ev-1"));
        assert_eq!(events_lines[0]["streamId"], json!("claude_code:sess-1"));
        assert_eq!(events_lines[0]["type"], json!("prompt"));

        assert!(export_dir.join("logroom.db").exists(), "logroom.db 사본이 생성돼야 함");
    }

    #[test]
    fn export_data_removes_partial_export_dir_on_failure() {
        let (tmp, conn) = seeded_conn();
        insert_stream(&conn, "claude_code:sess-1", Some("세션 1"), 1_000, 2_000);
        insert_event(&conn, "ev-1", "claude_code:sess-1", 1_000, "prompt", "ext-1");

        // streams.jsonl/events.jsonl까지는 성공하지만, DB 사본 단계(fs::copy)가 존재하지 않는
        // db_path 때문에 실패하도록 만든다 — 중간까지 생성된 export_dir이 정리되는지 검증.
        let bogus_db_path = tmp.path.join("no-such.db");
        let export_dir = tmp.path.join("export-fail");

        let result = export_data(&conn, &bogus_db_path, &export_dir);

        assert!(result.is_err(), "존재하지 않는 db_path면 실패해야 함");
        assert!(!export_dir.exists(), "실패 시 부분 생성된 export_dir이 정리돼야 함");
    }

    #[test]
    fn export_data_empty_db_writes_empty_files_with_zero_counts() {
        let (tmp, conn) = seeded_conn();
        let export_dir = tmp.path.join("export-empty");

        let summary = export_data(&conn, &tmp.db_path(), &export_dir).expect("빈 DB export도 성공해야 함");

        assert_eq!(summary.streams, 0);
        assert_eq!(summary.events, 0);
        assert_eq!(read_ndjson_lines(&export_dir.join("streams.jsonl")).len(), 0);
        assert_eq!(read_ndjson_lines(&export_dir.join("events.jsonl")).len(), 0);
        assert!(export_dir.join("logroom.db").exists());
    }

    // ── count_events_before ─────────────────────────────────────

    #[test]
    fn count_events_before_counts_only_strictly_earlier_events() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(&conn, "claude_code:sess-1", Some("세션 1"), 1_000, 3_000);
        insert_event(&conn, "ev-1", "claude_code:sess-1", 1_000, "prompt", "ext-1");
        insert_event(&conn, "ev-2", "claude_code:sess-1", 2_000, "response", "ext-2");
        insert_event(&conn, "ev-3", "claude_code:sess-1", 3_000, "prompt", "ext-3");

        assert_eq!(count_events_before(&conn, 2_000).unwrap(), 1, "ts=1_000만 2_000보다 이르다");
        assert_eq!(count_events_before(&conn, 3_001).unwrap(), 3, "전부 포함");
        assert_eq!(count_events_before(&conn, 1_000).unwrap(), 0, "경계값은 포함하지 않음(< 비교)");
    }

    #[test]
    fn count_events_before_empty_db_returns_zero() {
        let (_tmp, conn) = seeded_conn();
        assert_eq!(count_events_before(&conn, 999_999).unwrap(), 0);
    }

    // ── delete_events_before ────────────────────────────────────

    #[test]
    fn delete_events_before_deletes_old_events_and_prunes_empty_streams() {
        let (_tmp, mut conn) = seeded_conn();
        // sess-old: 이벤트 전부가 경계 이전 → 삭제 후 스트림 자체도 사라져야 함.
        insert_stream(&conn, "claude_code:sess-old", Some("오래된 세션"), 1_000, 1_500);
        insert_event(&conn, "ev-old-1", "claude_code:sess-old", 1_000, "prompt", "ext-old-1");
        insert_event(&conn, "ev-old-2", "claude_code:sess-old", 1_500, "response", "ext-old-2");

        // sess-mixed: 경계 이전 1건 + 이후 2건 → 이전 것만 삭제되고 경계(started_at)가 재계산돼야 함.
        insert_stream(&conn, "claude_code:sess-mixed", Some("혼재 세션"), 1_000, 5_000);
        insert_event(&conn, "ev-mixed-old", "claude_code:sess-mixed", 1_000, "prompt", "ext-mixed-old");
        insert_event(&conn, "ev-mixed-new1", "claude_code:sess-mixed", 4_000, "prompt", "ext-mixed-new1");
        insert_event(&conn, "ev-mixed-new2", "claude_code:sess-mixed", 5_000, "response", "ext-mixed-new2");

        // sess-clean: 경계 이후만 있는 대조군 → 손대면 안 됨.
        insert_stream(&conn, "claude_code:sess-clean", Some("최근 세션"), 10_000, 10_000);
        insert_event(&conn, "ev-clean", "claude_code:sess-clean", 10_000, "prompt", "ext-clean");

        let summary = delete_events_before(&mut conn, 3_000).expect("삭제 성공");

        assert_eq!(summary.deleted_events, 3, "sess-old 2건 + sess-mixed 1건");
        assert_eq!(summary.deleted_streams, 1, "sess-old만 완전히 비어 삭제돼야 함");

        let old_stream_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM streams WHERE id = 'claude_code:sess-old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_stream_exists, 0, "빈 스트림은 삭제돼야 함");

        let (mixed_started, mixed_ended): (i64, Option<i64>) = conn
            .query_row(
                "SELECT started_at, ended_at FROM streams WHERE id = 'claude_code:sess-mixed'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(mixed_started, 4_000, "삭제된 이벤트 대신 남은 첫 이벤트로 재계산돼야 함");
        assert_eq!(mixed_ended, Some(5_000));

        let (clean_started, clean_ended): (i64, Option<i64>) = conn
            .query_row(
                "SELECT started_at, ended_at FROM streams WHERE id = 'claude_code:sess-clean'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(clean_started, 10_000, "대조군 스트림은 변경되면 안 됨");
        assert_eq!(clean_ended, Some(10_000));

        let remaining_events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining_events, 3, "sess-mixed 2건 + sess-clean 1건만 남아야 함");
    }

    #[test]
    fn delete_events_before_prunes_fts_index_for_deleted_prompt_response() {
        let (_tmp, mut conn) = seeded_conn();
        insert_stream(&conn, "claude_code:sess-fts", Some("FTS 세션"), 1_000, 1_000);
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES ('ev-fts', 'claude_code:sess-fts', 1_000, 'claude_code', 'prompt', NULL, 'UNIQUEFTSMARKER 삭제 대상 검색어', NULL, NULL, NULL, NULL, NULL, 'ext-fts', '{}', 1_000)",
            [],
        )
        .unwrap();

        let before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events_fts WHERE events_fts MATCH 'UNIQUEFTSMARKER'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(before, 1, "삭제 전엔 FTS에 색인돼 있어야 함");

        delete_events_before(&mut conn, 2_000).expect("삭제 성공");

        let after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events_fts WHERE events_fts MATCH 'UNIQUEFTSMARKER'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, 0, "삭제 후엔 트리거로 FTS 색인도 제거돼야 함");
    }

    #[test]
    fn delete_events_before_no_matching_events_is_noop() {
        let (_tmp, mut conn) = seeded_conn();
        insert_stream(&conn, "claude_code:sess-clean", Some("최근 세션"), 10_000, 10_000);
        insert_event(&conn, "ev-clean", "claude_code:sess-clean", 10_000, "prompt", "ext-clean");

        let summary = delete_events_before(&mut conn, 1_000).expect("삭제 성공(대상 없음)");

        assert_eq!(summary.deleted_events, 0);
        assert_eq!(summary.deleted_streams, 0);
        let remaining_events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining_events, 1);
    }

    // ── vacuum_db ───────────────────────────────────────────────

    #[test]
    fn vacuum_db_shrinks_or_keeps_file_size_after_bulk_delete() {
        let (tmp, conn) = seeded_conn();
        insert_stream(&conn, "claude_code:sess-1", Some("세션 1"), 1_000, 1_000);
        // VACUUM 효과를 보려면 삭제로 인한 여유 공간이 있어야 하므로 대량으로 넣었다가 지운다.
        for i in 0..500 {
            let id = format!("ev-{i}");
            let ext = format!("ext-{i}");
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES (?1, 'claude_code:sess-1', 1000, 'claude_code', 'tool_result', NULL, ?2, NULL, NULL, NULL, NULL, NULL, ?3, '{}', 1000)",
                params![id, "x".repeat(500), ext],
            )
            .unwrap();
        }
        conn.execute("DELETE FROM events", []).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();

        let summary = vacuum_db(&conn, &tmp.db_path()).expect("vacuum 성공");

        assert!(summary.before_bytes > 0);
        assert!(
            summary.after_bytes <= summary.before_bytes,
            "삭제 후 VACUUM은 파일을 늘리면 안 됨(before={}, after={})",
            summary.before_bytes,
            summary.after_bytes
        );
    }
}
