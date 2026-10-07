//! SQLite 연결 · PRAGMA · 마이그레이션 · 인제스트 쓰기 경로 (docs/02-data-model.md).
//! 단일 라이터 원칙: 모든 쓰기는 이 모듈(인제스트 경로)로 직렬화된다.

use crate::model::{EventInput, IngestRequest, StreamInput};
use refinery::{Runner, SchemaVersion};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

mod embedded {
    // src-tauri/migrations/ (CARGO_MANIFEST_DIR 기준)
    refinery::embed_migrations!("./migrations");
}

/// ~/Library/Application Support/LogRoom (docs/02, 04)
pub fn data_dir() -> PathBuf {
    dirs::data_dir().expect("data_dir 없음").join("LogRoom")
}

pub fn db_path() -> PathBuf {
    data_dir().join("logroom.db")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("시스템 시간")
        .as_millis() as i64
}

/// DB 열기 → PRAGMA 적용 → refinery 마이그레이션(최신까지). perm 0600.
pub fn open_and_migrate() -> anyhow::Result<Connection> {
    open_and_migrate_at(&db_path())
}

/// pending 마이그레이션 중 최고 버전(적용할 것이 없으면 `None`).
/// `runner.get_applied_migrations`는 마이그레이션 이력 테이블 자체가 없는(한 번도 마이그레이션이
/// 실행된 적 없는) 완전히 새 DB에서 에러를 낸다 — 그 경우엔 "적용된 마이그레이션 없음"으로 취급한다.
fn highest_pending_version(runner: &Runner, conn: &mut Connection) -> Option<SchemaVersion> {
    let applied: HashSet<SchemaVersion> = runner
        .get_applied_migrations(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.version())
        .collect();

    runner
        .get_migrations()
        .iter()
        .map(|m| m.version())
        .filter(|v| !applied.contains(v))
        .max()
}

/// pending 마이그레이션 적용 직전 `path` → `<파일명>.bak-pre-v<version>`으로 백업한다(perm 0600,
/// 기존 동일 이름 백업은 덮어씀). 비가역 마이그레이션 직전 안전장치이므로 백업 실패는 그대로
/// 상위로 전파해(`Err`) 마이그레이션 자체를 중단시킨다 — 백업 없이 비가역 변경 금지.
fn backup_before_migration(path: &Path, version: SchemaVersion) -> anyhow::Result<()> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("logroom.db");
    let backup_path = path.with_file_name(format!("{file_name}.bak-pre-v{version}"));
    eprintln!(
        "[logroom] pending 마이그레이션(v{version}) 적용 전 백업: {}",
        backup_path.display()
    );
    fs::copy(path, &backup_path).map_err(|e| {
        anyhow::anyhow!("마이그레이션 전 백업 실패({}): {e}", backup_path.display())
    })?;
    fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// `path`를 인자로 받는 버전 — 테스트에서 tempdir DB 경로를 주입하기 위함.
/// `pub(crate)`: query.rs 단위테스트도 동일 마이그레이션 경로로 시드 DB를 만들기 위해 사용.
pub(crate) fn open_and_migrate_at(path: &Path) -> anyhow::Result<Connection> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }

    let mut conn = Connection::open(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;

    // foreign_keys 는 연결별 설정이라 매 연결 시 필요.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA busy_timeout=5000;
         PRAGMA foreign_keys=ON;
         PRAGMA synchronous=NORMAL;",
    )?;

    let runner = embedded::migrations::runner();

    // pending 마이그레이션이 있을 때만 백업(docs/02-data-model.md 약속 이행). 백업 실패 시
    // `?`로 즉시 중단 — 마이그레이션(비가역 변경)을 진행하지 않는다.
    if let Some(pending_version) = highest_pending_version(&runner, &mut conn) {
        backup_before_migration(path, pending_version)?;
    }

    let report = runner.run(&mut conn)?;

    // 새로 적용된 마이그레이션이 있을 때만 VACUUM(비용이 커서 매 기동마다 실행하면 낭비 —
    // 특히 V2의 FTS 재구축·body 다이어트처럼 삭제/축소가 큰 마이그레이션 직후 회수 효과가 크다).
    if !report.applied_migrations().is_empty() {
        eprintln!(
            "[logroom] 마이그레이션 적용됨: {}",
            report
                .applied_migrations()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );

        eprintln!("[logroom] VACUUM 시작(수십 초 소요 가능)");
        // VACUUM/checkpoint는 트랜잭션 안에서 실행할 수 없다(SQLite 제약) — 마이그레이션 트랜잭션이
        // 끝난 뒤 별도 실행. 이 시점엔 마이그레이션 자체는 이미 커밋 완료된 상태라 여기서 실패해도
        // 기동을 막을 이유가 없다 — 로그만 남기고 무시한다.
        if let Err(e) = conn.execute_batch("VACUUM;") {
            eprintln!("[logroom] VACUUM 실패(무시하고 계속 진행): {e}");
            return Ok(conn);
        }
        if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
            eprintln!("[logroom] wal_checkpoint 실패(무시하고 계속 진행): {e}");
            return Ok(conn);
        }
    }

    Ok(conn)
}

fn metadata_str(v: &Option<serde_json::Value>) -> String {
    v.as_ref()
        .map(|m| m.to_string())
        .unwrap_or_else(|| "{}".to_string())
}

/// 스트림 upsert 시 title 갱신 규칙: 수동 편집(`rename_stream`이 `metadata.manualTitle`을 세팅)된
/// 스트림은 이후 캡처가 다시 흘러들어와도 사용자가 지정한 title을 덮지 않는다(그 외 컬럼의
/// COALESCE 동작은 불변). `db.rs` 통합테스트(`upsert_stream_keeps_manual_title...`) 참고.
const UPSERT_STREAM_TITLE_SQL: &str = "CASE
             WHEN json_valid(streams.metadata) AND json_extract(streams.metadata, '$.manualTitle')
               THEN streams.title
             ELSE COALESCE(excluded.title, streams.title)
           END";

/// 스트림 upsert 시 metadata 갱신 규칙: **덮어쓰기가 아니라 병합**(RFC 7396 merge patch)이다.
///
/// 커넥터가 재조회한 최신 정보(Linear 이슈의 `state`/`linearProject` 등 — 이슈 상태는 QA→완료처럼
/// 나중에 바뀌므로 반드시 갱신돼야 한다)를 반영하되, 캡처가 모르는 기존 키는 지우면 안 된다.
/// 특히 `manualTitle`(사용자가 `rename_stream`으로 직접 지정한 제목, [`UPSERT_STREAM_TITLE_SQL`]이
/// 근거로 쓴다)이 통째 덮어쓰기로 날아가면 사용자 편집이 조용히 사라진다.
///
/// `json_patch(기존, 신규)`는 신규의 키만 덮고 나머지는 보존한다. 신규가 `{}`(metadata를 보내지
/// 않는 커넥터의 기본값)면 기존이 그대로 남아 무해하다. 어느 한쪽이라도 JSON이 아니면 병합을
/// 포기하고 기존 값을 지키는 쪽으로 폴백한다(데이터 손실 방지).
const UPSERT_STREAM_METADATA_SQL: &str = "CASE
             WHEN json_valid(streams.metadata) AND json_valid(excluded.metadata)
               THEN json_patch(streams.metadata, excluded.metadata)
             ELSE streams.metadata
           END";

pub(crate) fn upsert_stream(conn: &Connection, s: &StreamInput, init_ts: i64) -> anyhow::Result<()> {
    let now = now_ms();
    let sql = format!(
        "INSERT INTO streams
           (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 'active', ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
           title      = {UPSERT_STREAM_TITLE_SQL},
           project    = COALESCE(excluded.project, streams.project),
           git_branch = COALESCE(excluded.git_branch, streams.git_branch),
           metadata   = {UPSERT_STREAM_METADATA_SQL}"
    );
    conn.execute(
        &sql,
        params![
            s.id,
            s.source,
            s.kind.clone().unwrap_or_else(|| "session".to_string()),
            s.title,
            s.project,
            s.git_branch,
            init_ts,
            metadata_str(&s.metadata),
            now,
        ],
    )?;
    Ok(())
}

fn insert_event(conn: &Connection, e: &EventInput) -> anyhow::Result<usize> {
    let id = Uuid::now_v7().to_string();
    let now = now_ms();
    let n = conn.execute(
        "INSERT INTO events
           (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(source, external_id) DO NOTHING",
        params![
            id,
            e.stream_id,
            e.ts,
            e.source,
            e.event_type,
            e.title,
            e.body,
            e.model,
            e.tokens_in,
            e.tokens_out,
            e.url,
            e.parent_id,
            e.external_id,
            metadata_str(&e.metadata),
            now,
        ],
    )?;
    Ok(n)
}

pub(crate) fn recalc_stream_bounds(conn: &Connection, stream_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE streams SET
           started_at = COALESCE((SELECT MIN(ts) FROM events WHERE stream_id = ?1), started_at),
           ended_at   = COALESCE((SELECT MAX(ts) FROM events WHERE stream_id = ?1), ended_at)
         WHERE id = ?1",
        params![stream_id],
    )?;
    Ok(())
}

/// 인제스트: stream upsert → events idempotent 삽입 → 스트림 시간범위 재계산.
/// 반환값 = 실제 삽입된 이벤트 수(중복 제외).
pub fn ingest(conn: &Connection, req: &IngestRequest) -> anyhow::Result<usize> {
    let init_ts = req.events.iter().map(|e| e.ts).min().unwrap_or_else(now_ms);

    match &req.stream {
        Some(s) => upsert_stream(conn, s, init_ts)?,
        None => {
            // stream 미제공 시 첫 이벤트로 최소 스트림 생성(FK 충족).
            if let Some(first) = req.events.first() {
                let placeholder = StreamInput {
                    id: first.stream_id.clone(),
                    source: first.source.clone(),
                    kind: None,
                    title: None,
                    project: None,
                    git_branch: None,
                    started_at: None,
                    ended_at: None,
                    status: None,
                    metadata: None,
                };
                upsert_stream(conn, &placeholder, init_ts)?;
            }
        }
    }

    let mut inserted = 0usize;
    let mut touched: Vec<String> = Vec::new();
    for e in &req.events {
        inserted += insert_event(conn, e)?;
        if !touched.iter().any(|s| s == &e.stream_id) {
            touched.push(e.stream_id.clone());
        }
    }
    for sid in &touched {
        recalc_stream_bounds(conn, sid)?;
    }
    Ok(inserted)
}

/// 스트림 제목 수동 편집(FE 연필 아이콘 → 편집). trim 후 빈 문자열이면 에러(빈 title로
/// 덮어쓰기 방지). `metadata.manualTitle`을 `true`로 세워 이후 캡처가 다시 흘러들어와도
/// `upsert_stream`이 이 title을 보호하도록 표시한다(위 [`UPSERT_STREAM_TITLE_SQL`] 참고).
pub fn rename_stream(conn: &Connection, id: &str, title: &str) -> anyhow::Result<()> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        anyhow::bail!("제목은 빈 문자열일 수 없습니다");
    }
    conn.execute(
        "UPDATE streams SET
           title = ?2,
           metadata = json_set(
             CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
             '$.manualTitle', json('true')
           )
         WHERE id = ?1",
        params![id, trimmed],
    )?;
    Ok(())
}

/// 캡처 커서 조회: `(offset, mtime)`. 없으면 `None`(파일을 처음부터 backfill).
pub fn get_cursor(
    conn: &Connection,
    source: &str,
    resource: &str,
) -> anyhow::Result<Option<(i64, Option<i64>)>> {
    let row = conn
        .query_row(
            "SELECT offset, mtime FROM capture_cursors WHERE source = ?1 AND resource = ?2",
            params![source, resource],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()?;
    Ok(row)
}

/// 스트림의 최신 이벤트 ts(ms). 이벤트가 없으면 `None`(Kiro CLI ts 보간의 backfill 시작점 산출용).
pub fn max_ts_of_stream(conn: &Connection, stream_id: &str) -> anyhow::Result<Option<i64>> {
    let ts: Option<i64> = conn.query_row(
        "SELECT MAX(ts) FROM events WHERE stream_id = ?1",
        params![stream_id],
        |row| row.get(0),
    )?;
    Ok(ts)
}

/// 특정 source의 캡처 커서 중 가장 최근 갱신 시각(ms). 커서가 하나도 없으면 `None`
/// (감시 root는 등록됐지만 아직 파일을 처리한 적 없는 경우) — `get_capture_health`가
/// "감시 실패 의심"(stale) 판정에 사용한다(capture/health.rs).
pub fn max_cursor_updated_at(conn: &Connection, source: &str) -> anyhow::Result<Option<i64>> {
    let updated_at: Option<i64> = conn.query_row(
        "SELECT MAX(updated_at) FROM capture_cursors WHERE source = ?1",
        params![source],
        |row| row.get(0),
    )?;
    Ok(updated_at)
}

/// 캡처 커서 upsert(파일 감시 오프셋/mtime 갱신).
pub fn upsert_cursor(
    conn: &Connection,
    source: &str,
    resource: &str,
    offset: i64,
    mtime: Option<i64>,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO capture_cursors (source, resource, offset, mtime, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(source, resource) DO UPDATE SET
           offset     = excluded.offset,
           mtime      = excluded.mtime,
           updated_at = excluded.updated_at",
        params![source, resource, offset, mtime, now_ms()],
    )?;
    Ok(())
}

/// 캡처 커서 삭제 — 마이그레이션 전용(구 단일 커서를 이중 커서 체계로 전환할 때
/// `capture/slack.rs`의 이중 커서 마이그레이션 로직이 사용한다). 존재하지 않는 행을 지워도
/// 에러 없이 조용히 통과한다.
pub fn delete_cursor(conn: &Connection, source: &str, resource: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM capture_cursors WHERE source = ?1 AND resource = ?2",
        params![source, resource],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// `tempfile` 크레이트 없이 테스트용 임시 디렉토리를 만든다(config.rs `TempHome`과 동일 패턴).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("logroom-db-test-{}-{n}-{nanos}", std::process::id()));
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

    // ── capture_cursors 헬퍼(capture/health.rs가 사용) ──────────

    #[test]
    fn max_cursor_updated_at_none_when_no_cursors() {
        let tmp = TempDir::new();
        let conn = open_and_migrate_at(&tmp.db_path()).unwrap();
        assert_eq!(max_cursor_updated_at(&conn, "claude_code").unwrap(), None);
    }

    #[test]
    fn max_cursor_updated_at_returns_max_across_resources_for_source_only() {
        let tmp = TempDir::new();
        let conn = open_and_migrate_at(&tmp.db_path()).unwrap();
        // updated_at을 직접 제어하기 위해 upsert_cursor(now_ms() 내부 사용) 대신 원본 INSERT.
        conn.execute_batch(
            "INSERT INTO capture_cursors (source, resource, offset, mtime, updated_at) VALUES
               ('claude_code', '/a.jsonl', 10, 1, 1000),
               ('claude_code', '/b.jsonl', 20, 2, 2000),
               ('kiro_cli', '/c.jsonl', 30, 3, 9999);",
        )
        .unwrap();

        // claude_code는 두 resource 중 더 큰 값(2000)만, kiro_cli 값(9999)과 섞이지 않아야 함.
        assert_eq!(max_cursor_updated_at(&conn, "claude_code").unwrap(), Some(2000));
        assert_eq!(max_cursor_updated_at(&conn, "kiro_cli").unwrap(), Some(9999));
    }

    #[test]
    fn delete_cursor_removes_matching_row_and_leaves_others_untouched() {
        let tmp = TempDir::new();
        let conn = open_and_migrate_at(&tmp.db_path()).unwrap();
        upsert_cursor(&conn, "slack", "search", 100, None).unwrap();
        upsert_cursor(&conn, "slack", "search:newest", 200, None).unwrap();

        delete_cursor(&conn, "slack", "search").unwrap();

        assert_eq!(get_cursor(&conn, "slack", "search").unwrap(), None);
        assert_eq!(get_cursor(&conn, "slack", "search:newest").unwrap(), Some((200, None)));
    }

    #[test]
    fn delete_cursor_missing_row_is_a_noop() {
        let tmp = TempDir::new();
        let conn = open_and_migrate_at(&tmp.db_path()).unwrap();
        delete_cursor(&conn, "slack", "search").unwrap();
        assert_eq!(get_cursor(&conn, "slack", "search").unwrap(), None);
    }

    /// `path`에 V1(`V1__init.sql`)까지만 적용한 커넥션을 연다(V2 이관 테스트의 시드 대상 스키마).
    fn open_v1_only(path: &Path) -> Connection {
        let mut conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(1))
            .run(&mut conn)
            .unwrap();
        conn
    }

    /// `path`에 V2(`V2__essential_body.sql`)까지만 적용한 커넥션을 연다(V3 이관 테스트의 시드 대상 스키마).
    fn open_v2_only(path: &Path) -> Connection {
        let mut conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(2))
            .run(&mut conn)
            .unwrap();
        conn
    }

    /// `path`에 V3(`V3__prune_meta_prompts.sql`)까지만 적용한 커넥션을 연다(V4 이관 테스트의 시드 대상 스키마).
    fn open_v3_only(path: &Path) -> Connection {
        let mut conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(3))
            .run(&mut conn)
            .unwrap();
        conn
    }

    /// `path`에 V4(`V4__prune_injected_prompts.sql`)까지만 적용한 커넥션을 연다(V5 이관 테스트의 시드 대상 스키마).
    fn open_v4_only(path: &Path) -> Connection {
        let mut conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(4))
            .run(&mut conn)
            .unwrap();
        conn
    }

    /// `path`에 V5(`V5__worktree_project_remap.sql`)까지만 적용한 커넥션을 연다(V6 이관 테스트의 시드 대상 스키마).
    fn open_v5_only(path: &Path) -> Connection {
        let mut conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(5))
            .run(&mut conn)
            .unwrap();
        conn
    }

    /// `path`에 V6(`V6__derive_titles.sql`)까지만 적용한 커넥션을 연다(V7 이관 테스트의 시드 대상 스키마).
    fn open_v6_only(path: &Path) -> Connection {
        let mut conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(6))
            .run(&mut conn)
            .unwrap();
        conn
    }

    /// `path`에 V7(`V7__fts_user_content.sql`)까지만 적용한 커넥션을 연다(V8 이관 테스트의 시드 대상 스키마).
    fn open_v7_only(path: &Path) -> Connection {
        let mut conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(7))
            .run(&mut conn)
            .unwrap();
        conn
    }

    /// `path`에 V8(`V8__daily_summaries.sql`)까지만 적용한 커넥션을 연다(V9 이관 테스트의 시드 대상 스키마).
    fn open_v8_only(path: &Path) -> Connection {
        let mut conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        embedded::migrations::runner()
            .set_target(refinery::Target::Version(8))
            .run(&mut conn)
            .unwrap();
        conn
    }

    /// V1 스키마 기준으로 V2 이관 검증용 데이터 6종을 심는다(300자 초과/이하 tool_result body,
    /// NULL body, input 있는 tool_use metadata, 비JSON metadata 행, title-only prompt + response 1건).
    fn seed_v1_rows(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO streams
               (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES
               ('claude_code:sess-1', 'claude_code', 'session', '시드 스트림', NULL, NULL, 1000, 1000, 'active', '{}', 1000);",
        )
        .unwrap();

        let long_body = "y".repeat(300);
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-long', 'claude_code:sess-1', 1000, 'claude_code', 'tool_result', NULL, ?1, NULL, NULL, NULL, NULL, NULL, 'ext-long', '{}', 1000)",
            params![long_body],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-short', 'claude_code:sess-1', 1001, 'claude_code', 'tool_result', NULL, '짧은 결과', NULL, NULL, NULL, NULL, NULL, 'ext-short', '{}', 1001)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-null-body', 'claude_code:sess-1', 1002, 'claude_code', 'tool_result', NULL, NULL, NULL, NULL, NULL, NULL, NULL, 'ext-null-body', '{}', 1002)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-tool-use', 'claude_code:sess-1', 1003, 'claude_code', 'tool_use', 'Bash', 'ls -la', NULL, NULL, NULL, NULL, NULL, 'ext-tool-use', '{\"input\":{\"command\":\"ls -la\"}}', 1003)",
            [],
        )
        .unwrap();

        // 비JSON metadata 행 — 과거 버그/수동 조작 등으로 metadata가 유효한 JSON이 아닌 경우 재현.
        // json_valid 가드가 없으면 이 행에서 json_remove/json_type이 에러를 내 마이그레이션 전체가 실패한다.
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-bad-json', 'claude_code:sess-1', 1004, 'claude_code', 'tool_use', 'Bash', 'ls', NULL, NULL, NULL, NULL, NULL, 'ext-bad-json', 'not-json{', 1004)",
            [],
        )
        .unwrap();

        // title만 있고 body는 없는 prompt(FTS 검증용 고유 마커 포함).
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-title-only', 'claude_code:sess-1', 1005, 'claude_code', 'prompt', 'TITLEONLYPROMPT 제목만 있는 프롬프트', NULL, NULL, NULL, NULL, NULL, NULL, 'ext-title-only', '{}', 1005)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-response', 'claude_code:sess-1', 1006, 'claude_code', 'response', NULL, 'RESPONSEBODY 응답 본문', NULL, NULL, NULL, NULL, NULL, 'ext-response', '{}', 1006)",
            [],
        )
        .unwrap();
    }

    /// `events_fts`가 실제로 `needle`을 색인하고 있는지 MATCH 쿼리로 확인한다(external content fts5
    /// 테이블은 MATCH 없는 평범한 SELECT로는 색인 상태와 무관하게 content 테이블을 그대로 반환하므로
    /// 색인 여부 검증에는 반드시 MATCH가 필요하다).
    fn fts_contains(conn: &Connection, needle: &str) -> bool {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events_fts WHERE events_fts MATCH ?1",
                params![needle],
                |r| r.get(0),
            )
            .unwrap();
        count > 0
    }

    #[test]
    fn v2_migration_applies_over_seeded_v1_data_without_failing() {
        let tmp = TempDir::new();
        let path = tmp.db_path();
        {
            let conn = open_v1_only(&path);
            seed_v1_rows(&conn);
        } // conn 드롭 → 파일 잠금 해제 후 재오픈

        let conn = open_and_migrate_at(&path).expect("V1 → V2 마이그레이션이 성공해야 함");

        // 절단: 300자 tool_result body → 256자 + '…'.
        let truncated: String = conn
            .query_row("SELECT body FROM events WHERE id = 'ev-long'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(truncated.chars().count(), 257);
        assert!(truncated.ends_with('…'));

        // 비절단: 256자 이하 body는 그대로.
        let short: String = conn
            .query_row("SELECT body FROM events WHERE id = 'ev-short'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(short, "짧은 결과");

        // NULL body는 무손상(크래시 없이 NULL 유지).
        let null_body: Option<String> = conn
            .query_row("SELECT body FROM events WHERE id = 'ev-null-body'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(null_body, None);

        // tool_use metadata에서 $.input 제거.
        let tool_use_metadata: String = conn
            .query_row("SELECT metadata FROM events WHERE id = 'ev-tool-use'", [], |r| r.get(0))
            .unwrap();
        let tool_use_metadata: serde_json::Value = serde_json::from_str(&tool_use_metadata).unwrap();
        assert_eq!(tool_use_metadata, serde_json::json!({}));

        // 비JSON metadata 행은 json_valid 가드 덕분에 에러 없이 원문 그대로 보존.
        let bad_json_metadata: String = conn
            .query_row("SELECT metadata FROM events WHERE id = 'ev-bad-json'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bad_json_metadata, "not-json{");

        // FTS에는 prompt/response 타입만 존재해야 한다(tool_use/tool_result는 제외).
        // external content fts5 테이블은 MATCH 없는 평범한 SELECT로는 실제 색인 여부와 무관하게
        // content 테이블(events) 전체를 그대로 훑어 반환하므로(검증해봄), 반드시 MATCH로 색인
        // 포함 여부를 확인해야 한다.
        assert!(fts_contains(&conn, "TITLEONLYPROMPT"), "prompt는 FTS에 남아있어야 함");
        assert!(fts_contains(&conn, "RESPONSEBODY"), "response는 FTS에 남아있어야 함");
        assert!(!fts_contains(&conn, "Bash"), "tool_use는 FTS에서 빠져야 함");
        assert!(!fts_contains(&conn, "yyy"), "tool_result는 FTS에서 빠져야 함");
    }

    #[test]
    fn open_and_migrate_at_backs_up_db_before_pending_migration() {
        let tmp = TempDir::new();
        let path = tmp.db_path();
        {
            let conn = open_v1_only(&path);
            seed_v1_rows(&conn);
        }

        // V1 상태에서 pending 중 최고 버전(마이그레이션 추가로 계속 바뀜 — 현재 V3)을 동적으로
        // 계산해 백업 파일명을 검증한다. 버전을 하드코딩하면 새 마이그레이션을 추가할 때마다
        // 이 테스트가 매번 깨진다.
        let pending_version = {
            let mut conn = Connection::open(&path).unwrap();
            let runner = embedded::migrations::runner();
            highest_pending_version(&runner, &mut conn).expect("V1 이후 pending 마이그레이션이 있어야 함")
        };

        let backup_path = path.with_file_name(format!("logroom.db.bak-pre-v{pending_version}"));
        assert!(!backup_path.exists(), "마이그레이션 전에는 백업 파일이 없어야 함");

        open_and_migrate_at(&path).expect("마이그레이션이 성공해야 함");

        assert!(backup_path.exists(), "pending 마이그레이션 적용 전 백업 파일이 생성돼야 함");
        let perm = fs::metadata(&backup_path).unwrap().permissions();
        assert_eq!(perm.mode() & 0o777, 0o600);
    }

    // ── V3: 메타 prompt 소급 정리(migrations/V3__prune_meta_prompts.sql) ────────────

    /// V2 스키마 기준으로 V3 이관 검증용 스트림 3개를 심는다.
    /// - `sess-noise`: 노이즈 prompt 2개만 존재 → V3 이후 이벤트 0개가 되어 스트림 자체가 삭제돼야 함.
    /// - `sess-mixed`: 노이즈 prompt(가장 이른 ts) + 정상 prompt + response 혼재 →
    ///   노이즈만 삭제되고 title/started_at이 남은 첫 prompt 기준으로 재계산돼야 함.
    /// - `sess-clean`: 정상 prompt만 있는 대조군 → V3가 손대지 않아야 함.
    fn seed_v2_meta_noise_rows(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO streams
               (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES
               ('claude_code:sess-noise', 'claude_code', 'session',
                '<local-command-caveat>Caveat: 로컬 커맨드 실행 중 생성된 메시지', NULL, NULL, 1000, 1001, 'active', '{}', 1000),
               ('claude_code:sess-mixed', 'claude_code', 'session',
                '<command-args></command-args>', NULL, NULL, 1000, 2500, 'active', '{}', 1000),
               ('claude_code:sess-clean', 'claude_code', 'session',
                '정상 프롬프트만 있음', NULL, NULL, 500, 500, 'active', '{}', 500);",
        )
        .unwrap();

        // sess-noise: 노이즈 prompt 2개(전체 삭제 대상).
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-noise-1', 'claude_code:sess-noise', 1000, 'claude_code', 'prompt', NULL, ?1, NULL, NULL, NULL, NULL, NULL, 'ext-noise-1', '{}', 1000)",
            params!["<local-command-caveat>Caveat: 로컬 커맨드 실행 중 생성된 메시지입니다 NOISEMARKERONE.</local-command-caveat>"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-noise-2', 'claude_code:sess-noise', 1001, 'claude_code', 'prompt', NULL, '<command-name>/model</command-name>', NULL, NULL, NULL, NULL, NULL, 'ext-noise-2', '{}', 1001)",
            [],
        )
        .unwrap();

        // sess-mixed: 노이즈 prompt(ts=1000, 가장 이름) + 정상 prompt(ts=2000) + response(ts=2500).
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-mixed-noise', 'claude_code:sess-mixed', 1000, 'claude_code', 'prompt', NULL, '<command-args></command-args>', NULL, NULL, NULL, NULL, NULL, 'ext-mixed-noise', '{}', 1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-mixed-normal', 'claude_code:sess-mixed', 2000, 'claude_code', 'prompt', NULL, 'PROMPTKEEP 정상 프롬프트 본문입니다', NULL, NULL, NULL, NULL, NULL, 'ext-mixed-normal', '{}', 2000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-mixed-response', 'claude_code:sess-mixed', 2500, 'claude_code', 'response', NULL, 'RESPONSEKEEP 응답 본문', NULL, NULL, NULL, NULL, NULL, 'ext-mixed-response', '{}', 2500)",
            [],
        )
        .unwrap();

        // sess-clean: 정상 prompt만(대조군, V3가 손대면 안 됨).
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-clean-prompt', 'claude_code:sess-clean', 500, 'claude_code', 'prompt', NULL, '정상 프롬프트만 있음', NULL, NULL, NULL, NULL, NULL, 'ext-clean-prompt', '{}', 500)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn v3_migration_prunes_noise_prompts_and_recalculates_streams() {
        let tmp = TempDir::new();
        let path = tmp.db_path();
        {
            let conn = open_v2_only(&path);
            seed_v2_meta_noise_rows(&conn);
        } // conn 드롭 → 파일 잠금 해제 후 재오픈

        let conn = open_and_migrate_at(&path).expect("V2 → V3 마이그레이션이 성공해야 함");

        // 1) 노이즈 prompt 이벤트가 삭제됐는지.
        let noise_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE id IN ('ev-noise-1', 'ev-noise-2', 'ev-mixed-noise')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(noise_count, 0, "노이즈 prompt 이벤트는 모두 삭제돼야 함");

        // 정상 prompt/response는 그대로 남아야 함.
        let kept_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE id IN ('ev-mixed-normal', 'ev-mixed-response', 'ev-clean-prompt')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept_count, 3, "정상 이벤트는 삭제되면 안 됨");

        // 2) sess-noise는 이벤트가 0개가 되어 스트림 자체가 삭제돼야 함.
        let noise_stream_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM streams WHERE id = 'claude_code:sess-noise'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(noise_stream_exists, 0, "노이즈만 있던 스트림은 삭제돼야 함");

        // 3) sess-mixed: title이 남은 첫 prompt(정상) 앞 60자로 갱신되고, started_at이 재계산돼야 함.
        let (mixed_title, mixed_started, mixed_ended): (Option<String>, i64, Option<i64>) = conn
            .query_row(
                "SELECT title, started_at, ended_at FROM streams WHERE id = 'claude_code:sess-mixed'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(mixed_title.as_deref(), Some("PROMPTKEEP 정상 프롬프트 본문입니다"));
        assert_eq!(mixed_started, 2000, "삭제된 노이즈 이벤트 대신 남은 첫 이벤트 ts로 재계산돼야 함");
        assert_eq!(mixed_ended, Some(2500));

        // 4) sess-clean(대조군)은 title/이벤트 모두 그대로.
        let clean_title: Option<String> = conn
            .query_row(
                "SELECT title FROM streams WHERE id = 'claude_code:sess-clean'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(clean_title.as_deref(), Some("정상 프롬프트만 있음"));
        let clean_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE stream_id = 'claude_code:sess-clean'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(clean_count, 1);

        // 5) FTS: 노이즈 prompt는 색인에서도 빠지고(V2 트리거가 삭제 시 자동 반영), 정상 prompt는 남아야 함.
        assert!(!fts_contains(&conn, "NOISEMARKERONE"), "노이즈 prompt는 FTS에서 빠져야 함");
        assert!(fts_contains(&conn, "PROMPTKEEP"), "정상 prompt는 FTS에 남아있어야 함");
        assert!(fts_contains(&conn, "RESPONSEKEEP"), "response는 FTS에 남아있어야 함");
    }

    // ── V4: 시스템 주입 prompt 소급 정리(migrations/V4__prune_injected_prompts.sql) ──────

    /// V3 스키마 기준으로 V4 이관 검증용 스트림 3개를 심는다.
    /// - `sess-injected-noise`: 6종 주입 패턴(prompt) 전부만 존재 → V4 이후 이벤트 0개가 되어
    ///   스트림 자체가 삭제돼야 함(`<ide_opened_file>`은 LIKE ESCAPE 동작 검증용).
    /// - `sess-mixed-injected`: 주입 노이즈(가장 이른 ts) + 정상 prompt + response 혼재 →
    ///   노이즈만 삭제되고 title/started_at이 남은 첫 prompt 기준으로 재계산돼야 함.
    /// - `sess-image-safe`: `[Image: ...` 로 시작하는 사용자 첨부 이미지 prompt만 있는 대조군 →
    ///   V4가 손대면 안 됨(필터 금지 대상).
    fn seed_v3_injected_noise_rows(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO streams
               (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES
               ('claude_code:sess-injected-noise', 'claude_code', 'session',
                '<task-notification>agent-a1 완료</task-notification>', NULL, NULL, 1000, 1005, 'active', '{}', 1000),
               ('claude_code:sess-mixed-injected', 'claude_code', 'session',
                '<task-notification>agent-a1 완료</task-notification>', NULL, NULL, 1000, 2500, 'active', '{}', 1000),
               ('claude_code:sess-image-safe', 'claude_code', 'session',
                '[Image: 스크린샷.png] 이 화면 좀 봐줘', NULL, NULL, 800, 800, 'active', '{}', 800);",
        )
        .unwrap();

        // sess-injected-noise: 6종 주입 패턴 전부(전체 삭제 대상).
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-notif-1', 'claude_code:sess-injected-noise', 1000, 'claude_code', 'prompt', NULL, ?1, NULL, NULL, NULL, NULL, NULL, 'ext-notif-1', '{}', 1000)",
            params!["<task-notification>agent-a1 완료 NOISETASKNOTIF</task-notification>"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-teammate-1', 'claude_code:sess-injected-noise', 1001, 'claude_code', 'prompt', NULL, '<teammate-message from=\"agent-b\">안녕</teammate-message>', NULL, NULL, NULL, NULL, NULL, 'ext-teammate-1', '{}', 1001)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-fork-1', 'claude_code:sess-injected-noise', 1002, 'claude_code', 'prompt', NULL, '<fork-boilerplate>표준 안내문</fork-boilerplate>', NULL, NULL, NULL, NULL, NULL, 'ext-fork-1', '{}', 1002)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-enforce-1', 'claude_code:sess-injected-noise', 1003, 'claude_code', 'prompt', NULL, '[structured-output-enforce] JSON으로만 응답하세요', NULL, NULL, NULL, NULL, NULL, 'ext-enforce-1', '{}', 1003)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-sysnotif-1', 'claude_code:sess-injected-noise', 1004, 'claude_code', 'prompt', NULL, '[SYSTEM NOTIFICATION - NOT USER INPUT] 백그라운드 작업 완료', NULL, NULL, NULL, NULL, NULL, 'ext-sysnotif-1', '{}', 1004)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-ide-1', 'claude_code:sess-injected-noise', 1005, 'claude_code', 'prompt', NULL, '<ide_opened_file>src/main.rs</ide_opened_file>', NULL, NULL, NULL, NULL, NULL, 'ext-ide-1', '{}', 1005)",
            [],
        )
        .unwrap();

        // sess-mixed-injected: 주입 노이즈(ts=1000, 가장 이름) + 정상 prompt(ts=2000) + response(ts=2500).
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-mixed2-noise', 'claude_code:sess-mixed-injected', 1000, 'claude_code', 'prompt', NULL, '<task-notification>agent-a1 완료</task-notification>', NULL, NULL, NULL, NULL, NULL, 'ext-mixed2-noise', '{}', 1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-mixed2-normal', 'claude_code:sess-mixed-injected', 2000, 'claude_code', 'prompt', NULL, 'PROMPTKEEP2 정상 프롬프트 본문입니다', NULL, NULL, NULL, NULL, NULL, 'ext-mixed2-normal', '{}', 2000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-mixed2-response', 'claude_code:sess-mixed-injected', 2500, 'claude_code', 'response', NULL, 'RESPONSEKEEP2 응답 본문', NULL, NULL, NULL, NULL, NULL, 'ext-mixed2-response', '{}', 2500)",
            [],
        )
        .unwrap();

        // sess-image-safe: `[Image: ...`(사용자 첨부 이미지 prompt, 대조군 — V4가 손대면 안 됨).
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES
               ('ev-image-prompt', 'claude_code:sess-image-safe', 800, 'claude_code', 'prompt', NULL, '[Image: 스크린샷.png] 이 화면 좀 봐줘', NULL, NULL, NULL, NULL, NULL, 'ext-image-prompt', '{}', 800)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn v4_migration_prunes_injected_prompts_and_keeps_user_image_prompts() {
        let tmp = TempDir::new();
        let path = tmp.db_path();
        {
            let conn = open_v3_only(&path);
            seed_v3_injected_noise_rows(&conn);
        } // conn 드롭 → 파일 잠금 해제 후 재오픈

        let conn = open_and_migrate_at(&path).expect("V3 → V4 마이그레이션이 성공해야 함");

        // 1) 6종 주입 노이즈 prompt 이벤트가 모두 삭제됐는지(`<ide_opened_file>` ESCAPE 포함).
        let noise_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE id IN (
                   'ev-notif-1', 'ev-teammate-1', 'ev-fork-1', 'ev-enforce-1', 'ev-sysnotif-1', 'ev-ide-1',
                   'ev-mixed2-noise'
                 )",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(noise_count, 0, "6종 주입 노이즈 prompt 이벤트는 모두 삭제돼야 함");

        // 정상 prompt/response와 `[Image: ...` 사용자 프롬프트는 그대로 남아야 함.
        let kept_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE id IN ('ev-mixed2-normal', 'ev-mixed2-response', 'ev-image-prompt')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept_count, 3, "정상 이벤트와 [Image: 프롬프트는 삭제되면 안 됨");

        // 2) sess-injected-noise는 이벤트가 0개가 되어 스트림 자체가 삭제돼야 함.
        let noise_stream_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM streams WHERE id = 'claude_code:sess-injected-noise'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(noise_stream_exists, 0, "주입 노이즈만 있던 스트림은 삭제돼야 함");

        // 3) sess-mixed-injected: title이 남은 첫 prompt(정상) 앞 60자로 갱신되고, started_at이 재계산돼야 함.
        let (mixed_title, mixed_started, mixed_ended): (Option<String>, i64, Option<i64>) = conn
            .query_row(
                "SELECT title, started_at, ended_at FROM streams WHERE id = 'claude_code:sess-mixed-injected'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(mixed_title.as_deref(), Some("PROMPTKEEP2 정상 프롬프트 본문입니다"));
        assert_eq!(mixed_started, 2000, "삭제된 노이즈 이벤트 대신 남은 첫 이벤트 ts로 재계산돼야 함");
        assert_eq!(mixed_ended, Some(2500));

        // 4) sess-image-safe(대조군)는 title/이벤트 모두 그대로(`[Image:`는 필터 대상 아님).
        let image_title: Option<String> = conn
            .query_row(
                "SELECT title FROM streams WHERE id = 'claude_code:sess-image-safe'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(image_title.as_deref(), Some("[Image: 스크린샷.png] 이 화면 좀 봐줘"));
        let image_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE stream_id = 'claude_code:sess-image-safe'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(image_count, 1);

        // 5) FTS: 주입 노이즈 prompt는 색인에서도 빠지고(V2 트리거가 삭제 시 자동 반영),
        //    정상 prompt/response·[Image: 프롬프트는 남아야 함.
        assert!(!fts_contains(&conn, "NOISETASKNOTIF"), "주입 노이즈 prompt는 FTS에서 빠져야 함");
        assert!(fts_contains(&conn, "PROMPTKEEP2"), "정상 prompt는 FTS에 남아있어야 함");
        assert!(fts_contains(&conn, "RESPONSEKEEP2"), "response는 FTS에 남아있어야 함");
        assert!(fts_contains(&conn, "스크린샷"), "[Image: 프롬프트는 FTS에 남아있어야 함");
    }

    // ── V5: worktree project 소급 remap(migrations/V5__worktree_project_remap.sql) ──────

    /// V4 스키마 기준으로 V5 이관 검증용 스트림 2개를 심는다.
    /// - `claude_code:sess-worktree`: project가 worktree 경로(`.claude/worktrees/agent-xxx` 포함)
    ///   → V5 이후 본 레포 루트로 remap돼야 함.
    /// - `claude_code:sess-normal`: project가 일반 레포 경로(worktree 패턴 없음, 대조군)
    ///   → V5가 손대면 안 됨.
    fn seed_v4_worktree_project_rows(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO streams
               (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES
               ('claude_code:sess-worktree', 'claude_code', 'session', '워크트리 세션',
                '/Users/me/src/logroom/.claude/worktrees/agent-xxx', 'main', 1000, 1000, 'active', '{}', 1000),
               ('claude_code:sess-normal', 'claude_code', 'session', '일반 세션',
                '/Users/me/src/logroom', 'main', 2000, 2000, 'active', '{}', 2000);",
        )
        .unwrap();
    }

    #[test]
    fn v5_migration_remaps_worktree_projects_and_keeps_normal_projects() {
        let tmp = TempDir::new();
        let path = tmp.db_path();
        {
            let conn = open_v4_only(&path);
            seed_v4_worktree_project_rows(&conn);
        } // conn 드롭 → 파일 잠금 해제 후 재오픈

        let conn = open_and_migrate_at(&path).expect("V4 → V5 마이그레이션이 성공해야 함");

        let worktree_project: Option<String> = conn
            .query_row(
                "SELECT project FROM streams WHERE id = 'claude_code:sess-worktree'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            worktree_project.as_deref(),
            Some("/Users/me/src/logroom"),
            "worktree 경로는 본 레포 루트로 remap돼야 함"
        );

        let normal_project: Option<String> = conn
            .query_row(
                "SELECT project FROM streams WHERE id = 'claude_code:sess-normal'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            normal_project.as_deref(),
            Some("/Users/me/src/logroom"),
            "worktree 패턴이 없는 일반 project는 그대로여야 함"
        );
    }

    // ── V6: prompt-유래 title 소급 재계산(migrations/V6__derive_titles.sql) ──────

    /// V5 스키마 기준으로 V6 이관 검증용 스트림 4개 + 이벤트를 심고 마이그레이션을 검증한다.
    /// - `claude_code:sess-short-first`: title이 짧은 첫 prompt("ㄱㄱ")의 raw substr과 일치 →
    ///   V6 이후 15자+ 인 둘째 prompt의 title로 재계산돼야 함.
    /// - `claude_code:sess-ai-title`: title이 ai-title 스타일(첫 prompt의 raw substr과 불일치) →
    ///   V6가 손대면 안 됨.
    /// - `claude_code:sess-agent-control`: title 패턴은 일치하지만 kind='agent' → V6가 손대면 안 됨.
    /// - `claude_code:sess-manual-control`: title 패턴은 일치하지만 metadata.manualTitle=true →
    ///   V6가 손대면 안 됨(rename_stream 보호 방어 가드 확인).
    #[test]
    fn v6_migration_recalculates_prompt_derived_titles_and_keeps_others() {
        let tmp = TempDir::new();
        let path = tmp.db_path();
        {
            let conn = open_v5_only(&path);
            conn.execute_batch(
                "INSERT INTO streams
                   (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
                 VALUES
                   ('claude_code:sess-short-first', 'claude_code', 'session', 'ㄱㄱ', NULL, NULL, 1000, 2000, 'active', '{}', 1000),
                   ('claude_code:sess-ai-title', 'claude_code', 'session', 'AI가 생성한 특별한 제목', NULL, NULL, 1000, 1000, 'active', '{}', 1000),
                   ('claude_code:sess-agent-control', 'claude_code', 'agent', 'ㄴㄴ', NULL, NULL, 1000, 1000, 'active', '{}', 1000),
                   ('claude_code:sess-manual-control', 'claude_code', 'session', 'ㄷㄷ', NULL, NULL, 1000, 1000, 'active', '{}', 1000);

                 UPDATE streams SET metadata = '{\"manualTitle\":true}' WHERE id = 'claude_code:sess-manual-control';",
            )
            .unwrap();

            // sess-short-first: 짧은 첫 prompt("ㄱㄱ") + 15자+ 둘째 prompt.
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-short-1', 'claude_code:sess-short-first', 1000, 'claude_code', 'prompt', NULL, 'ㄱㄱ', NULL, NULL, NULL, NULL, NULL, 'ext-short-1', '{}', 1000)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-short-2', 'claude_code:sess-short-first', 2000, 'claude_code', 'prompt', NULL, '이 프로젝트 구조를 좀 설명해 주실 수 있을까요', NULL, NULL, NULL, NULL, NULL, 'ext-short-2', '{}', 2000)",
                [],
            )
            .unwrap();

            // sess-ai-title: title이 ai-title 스타일이라 첫 prompt의 raw substr과 불일치(대조군).
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-ai-title-1', 'claude_code:sess-ai-title', 1000, 'claude_code', 'prompt', NULL, '짧은 프롬프트 본문입니다', NULL, NULL, NULL, NULL, NULL, 'ext-ai-title-1', '{}', 1000)",
                [],
            )
            .unwrap();

            // sess-agent-control: title 패턴은 일치하지만 kind='agent'(대조군).
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-agent-1', 'claude_code:sess-agent-control', 1000, 'claude_code', 'prompt', NULL, 'ㄴㄴ', NULL, NULL, NULL, NULL, NULL, 'ext-agent-1', '{}', 1000)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-agent-2', 'claude_code:sess-agent-control', 2000, 'claude_code', 'prompt', NULL, '둘째 프롬프트는 충분히 길어야 채택 대상이 됩니다', NULL, NULL, NULL, NULL, NULL, 'ext-agent-2', '{}', 2000)",
                [],
            )
            .unwrap();

            // sess-manual-control: title 패턴은 일치하지만 manualTitle=true(대조군).
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-manual-1', 'claude_code:sess-manual-control', 1000, 'claude_code', 'prompt', NULL, 'ㄷㄷ', NULL, NULL, NULL, NULL, NULL, 'ext-manual-1', '{}', 1000)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-manual-2', 'claude_code:sess-manual-control', 2000, 'claude_code', 'prompt', NULL, '둘째 프롬프트는 충분히 길어야 채택 대상이 됩니다', NULL, NULL, NULL, NULL, NULL, 'ext-manual-2', '{}', 2000)",
                [],
            )
            .unwrap();
        } // conn 드롭 → 파일 잠금 해제 후 재오픈

        let conn = open_and_migrate_at(&path).expect("V5 → V6 마이그레이션이 성공해야 함");

        // 1) 짧은 첫 prompt 스트림: 둘째(15자+) prompt의 정리된 첫 줄로 title이 재계산돼야 함.
        let short_first_title: Option<String> = conn
            .query_row(
                "SELECT title FROM streams WHERE id = 'claude_code:sess-short-first'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            short_first_title.as_deref(),
            Some("이 프로젝트 구조를 좀 설명해 주실 수 있을까요"),
            "짧은 첫 prompt는 건너뛰고 의미 있는 둘째 prompt로 title이 갱신돼야 함"
        );

        // 2) ai-title 스타일(불일치) title은 불변.
        let ai_title: Option<String> = conn
            .query_row(
                "SELECT title FROM streams WHERE id = 'claude_code:sess-ai-title'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            ai_title.as_deref(),
            Some("AI가 생성한 특별한 제목"),
            "raw substr과 불일치하는 title(ai-title 등)은 V6가 손대면 안 됨"
        );

        // 3) agent 스트림은 패턴이 일치해도 kind 필터로 제외돼야 함.
        let agent_title: Option<String> = conn
            .query_row(
                "SELECT title FROM streams WHERE id = 'claude_code:sess-agent-control'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(agent_title.as_deref(), Some("ㄴㄴ"), "agent 스트림은 V6가 손대면 안 됨");

        // 4) manualTitle 스트림은 방어 가드로 제외돼야 함.
        let manual_title: Option<String> = conn
            .query_row(
                "SELECT title FROM streams WHERE id = 'claude_code:sess-manual-control'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            manual_title.as_deref(),
            Some("ㄷㄷ"),
            "manualTitle 스트림은 V6가 손대면 안 됨"
        );
    }

    // ── V7: FTS 인덱싱 범위에 message/note 추가(migrations/V7__fts_user_content.sql, ADR-0012 보강) ──

    #[test]
    fn v7_migration_indexes_message_and_note_but_still_excludes_tool_types() {
        let tmp = TempDir::new();
        let path = tmp.db_path();
        {
            let conn = open_v6_only(&path);
            conn.execute_batch(
                "INSERT INTO streams
                   (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
                 VALUES
                   ('slack:chan-1', 'slack', 'session', '#general', NULL, NULL, 1000, 1004, 'active', '{}', 1000);",
            )
            .unwrap();

            // message: V6까지는 FTS 트리거 조건에 없어 인덱싱되지 않았어야 할 타입 — V7 이후 검색돼야 함.
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-message', 'slack:chan-1', 1000, 'slack', 'message', NULL, 'MESSAGEMARKER 사용자가 슬랙에 남긴 메시지', NULL, NULL, NULL, NULL, NULL, 'ext-message', '{}', 1000)",
                [],
            )
            .unwrap();

            // note: message와 동급의 사용자 콘텐츠 — 마찬가지로 V7 이후 검색돼야 함.
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-note', 'slack:chan-1', 1001, 'slack', 'note', NULL, 'NOTEMARKER 수동으로 남긴 기록', NULL, NULL, NULL, NULL, NULL, 'ext-note', '{}', 1001)",
                [],
            )
            .unwrap();

            // tool_use/tool_result는 V2 이후와 마찬가지로 계속 인덱싱에서 빠져야 한다(대조군).
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-tool-use', 'slack:chan-1', 1002, 'slack', 'tool_use', 'Bash', 'TOOLUSEMARKER ls -la', NULL, NULL, NULL, NULL, NULL, 'ext-tool-use', '{}', 1002)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-tool-result', 'slack:chan-1', 1003, 'slack', 'tool_result', NULL, 'TOOLRESULTMARKER 결과', NULL, NULL, NULL, NULL, NULL, 'ext-tool-result', '{}', 1003)",
                [],
            )
            .unwrap();

            // prompt/response는 V2부터 이미 인덱싱 대상 — V7 이후에도 계속 검색돼야 함(회귀 확인).
            conn.execute(
                "INSERT INTO events
                   (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
                 VALUES
                   ('ev-prompt', 'slack:chan-1', 1004, 'slack', 'prompt', NULL, 'PROMPTMARKER 프롬프트', NULL, NULL, NULL, NULL, NULL, 'ext-prompt', '{}', 1004)",
                [],
            )
            .unwrap();
        } // conn 드롭 → 파일 잠금 해제 후 재오픈

        let conn = open_and_migrate_at(&path).expect("V6 → V7 마이그레이션이 성공해야 함");

        assert!(fts_contains(&conn, "MESSAGEMARKER"), "message는 V7 이후 FTS에 인덱싱돼야 함");
        assert!(fts_contains(&conn, "NOTEMARKER"), "note는 V7 이후 FTS에 인덱싱돼야 함");
        assert!(fts_contains(&conn, "PROMPTMARKER"), "prompt는 V7 이후에도 계속 인덱싱돼야 함");
        assert!(!fts_contains(&conn, "TOOLUSEMARKER"), "tool_use는 V7 이후에도 인덱싱에서 빠져야 함");
        assert!(!fts_contains(&conn, "TOOLRESULTMARKER"), "tool_result는 V7 이후에도 인덱싱에서 빠져야 함");
    }

    // ── V8: 일일 AI 요약 캐시 테이블(migrations/V8__daily_summaries.sql, M7-①, ADR-0016) ──

    #[test]
    fn v8_migration_creates_daily_summaries_table_with_composite_primary_key() {
        let tmp = TempDir::new();
        let path = tmp.db_path();
        {
            open_v7_only(&path);
        } // conn 드롭 → 파일 잠금 해제 후 재오픈

        let conn = open_and_migrate_at(&path).expect("V7 → V8 마이그레이션이 성공해야 함");

        conn.execute(
            "INSERT INTO daily_summaries (local_date, locale, tz, engine, model, content, created_at)
             VALUES ('2026-07-16', 'ko', 'Asia/Seoul', 'cli', 'sonnet', '- 첫 요약', 1000)",
            [],
        )
        .expect("daily_summaries insert 성공해야 함");

        let content: String = conn
            .query_row(
                "SELECT content FROM daily_summaries WHERE local_date = '2026-07-16' AND locale = 'ko'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "- 첫 요약");

        // 같은 (local_date, locale) 조합은 PRIMARY KEY 위반으로 재삽입이 거부돼야 함(upsert는
        // summary::upsert가 ON CONFLICT로 별도 처리 — 여기서는 스키마 제약 자체만 검증).
        let dup_result = conn.execute(
            "INSERT INTO daily_summaries (local_date, locale, tz, engine, model, content, created_at)
             VALUES ('2026-07-16', 'ko', 'Asia/Seoul', 'api', 'claude-haiku-4-5', '- 다른 요약', 2000)",
            [],
        );
        assert!(dup_result.is_err(), "동일한 (local_date, locale) 재삽입은 실패해야 함");

        // 다른 locale은 별도 행으로 공존해야 함.
        conn.execute(
            "INSERT INTO daily_summaries (local_date, locale, tz, engine, model, content, created_at)
             VALUES ('2026-07-16', 'en', 'Asia/Seoul', 'cli', 'sonnet', '- first summary', 1500)",
            [],
        )
        .expect("다른 locale 조합은 별도 행으로 insert 성공해야 함");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM daily_summaries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    // ── V9: 주간/월간 요약 롤업 캐시 테이블(migrations/V9__period_summaries.sql, M7-③) ──

    #[test]
    fn v9_migration_creates_period_summaries_table_with_composite_primary_key() {
        let tmp = TempDir::new();
        let path = tmp.db_path();
        {
            open_v8_only(&path);
        } // conn 드롭 → 파일 잠금 해제 후 재오픈

        let conn = open_and_migrate_at(&path).expect("V8 → V9 마이그레이션이 성공해야 함");

        conn.execute(
            "INSERT INTO period_summaries (period_type, period_key, locale, tz, engine, model, content, created_at)
             VALUES ('week', '2026-07-13', 'ko', 'Asia/Seoul', 'cli', 'sonnet', '- 첫 주간 요약', 1000)",
            [],
        )
        .expect("period_summaries insert 성공해야 함");

        let content: String = conn
            .query_row(
                "SELECT content FROM period_summaries WHERE period_type = 'week' AND period_key = '2026-07-13' AND locale = 'ko'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "- 첫 주간 요약");

        // 같은 (period_type, period_key, locale) 조합은 PRIMARY KEY 위반으로 재삽입이 거부돼야 함
        // (upsert는 summary::period::upsert가 ON CONFLICT로 별도 처리 — 여기서는 스키마 제약만 검증).
        let dup_result = conn.execute(
            "INSERT INTO period_summaries (period_type, period_key, locale, tz, engine, model, content, created_at)
             VALUES ('week', '2026-07-13', 'ko', 'Asia/Seoul', 'api', 'claude-haiku-4-5', '- 다른 요약', 2000)",
            [],
        );
        assert!(dup_result.is_err(), "동일한 (period_type, period_key, locale) 재삽입은 실패해야 함");

        // period_type이 다르면(월간 'month' vs 주간 'week') 같은 period_key라도 별도 행으로 공존해야 함.
        conn.execute(
            "INSERT INTO period_summaries (period_type, period_key, locale, tz, engine, model, content, created_at)
             VALUES ('month', '2026-07-13', 'ko', 'Asia/Seoul', 'cli', 'sonnet', '- 월간 요약', 1500)",
            [],
        )
        .expect("다른 period_type 조합은 별도 행으로 insert 성공해야 함");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM period_summaries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    // ── rename_stream(수동 제목 편집) + upsert_stream의 manualTitle 보호 ──────

    #[test]
    fn rename_stream_trims_title_and_sets_manual_title_flag() {
        let tmp = TempDir::new();
        let conn = open_and_migrate_at(&tmp.db_path()).unwrap();
        conn.execute_batch(
            "INSERT INTO streams
               (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES ('claude_code:sess-rename', 'claude_code', 'session', '원래 제목', NULL, NULL, 1000, 1000, 'active', '{}', 1000);",
        )
        .unwrap();

        rename_stream(&conn, "claude_code:sess-rename", "  새 제목  ").expect("rename 성공해야 함");

        let (title, metadata_raw): (Option<String>, String) = conn
            .query_row(
                "SELECT title, metadata FROM streams WHERE id = 'claude_code:sess-rename'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(title.as_deref(), Some("새 제목"), "trim 후 저장돼야 함");
        let metadata: serde_json::Value = serde_json::from_str(&metadata_raw).unwrap();
        assert_eq!(metadata["manualTitle"], serde_json::json!(true));
    }

    #[test]
    fn rename_stream_rejects_blank_title_and_leaves_existing_title_untouched() {
        let tmp = TempDir::new();
        let conn = open_and_migrate_at(&tmp.db_path()).unwrap();
        conn.execute_batch(
            "INSERT INTO streams
               (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES ('claude_code:sess-blank', 'claude_code', 'session', '원래 제목', NULL, NULL, 1000, 1000, 'active', '{}', 1000);",
        )
        .unwrap();

        let err = rename_stream(&conn, "claude_code:sess-blank", "   ").expect_err("빈 제목은 에러여야 함");
        assert!(err.to_string().contains("빈 문자열"));

        let title: Option<String> = conn
            .query_row(
                "SELECT title FROM streams WHERE id = 'claude_code:sess-blank'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title.as_deref(), Some("원래 제목"), "실패 시 title은 변경되면 안 됨");
    }

    /// `StreamInput` 뼈대(title만 다르게)를 만드는 헬퍼 — 아래 upsert 보호 테스트 전용.
    fn stream_input(id: &str, title: &str, project: Option<&str>, git_branch: Option<&str>) -> StreamInput {
        StreamInput {
            id: id.to_string(),
            source: "claude_code".to_string(),
            kind: Some("session".to_string()),
            title: Some(title.to_string()),
            project: project.map(str::to_string),
            git_branch: git_branch.map(str::to_string),
            started_at: None,
            ended_at: None,
            status: None,
            metadata: None,
        }
    }

    #[test]
    fn upsert_stream_keeps_manual_title_after_rename_but_still_updates_other_columns() {
        let tmp = TempDir::new();
        let conn = open_and_migrate_at(&tmp.db_path()).unwrap();

        ingest(
            &conn,
            &IngestRequest {
                stream: Some(stream_input("claude_code:sess-manual", "자동 제목", None, None)),
                events: Vec::new(),
            },
        )
        .expect("첫 인제스트 성공해야 함");

        rename_stream(&conn, "claude_code:sess-manual", "사용자가 지정한 제목").expect("rename 성공해야 함");

        // 재캡처가 흘러들어와 title/project/git_branch 모두 갱신을 시도.
        ingest(
            &conn,
            &IngestRequest {
                stream: Some(stream_input(
                    "claude_code:sess-manual",
                    "새로 캡처된 제목",
                    Some("/tmp/project"),
                    Some("feature"),
                )),
                events: Vec::new(),
            },
        )
        .expect("재인제스트 성공해야 함");

        let (title, project, git_branch): (Option<String>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT title, project, git_branch FROM streams WHERE id = 'claude_code:sess-manual'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            title.as_deref(),
            Some("사용자가 지정한 제목"),
            "manualTitle 스트림은 재캡처에도 title이 보호돼야 함"
        );
        assert_eq!(
            project.as_deref(),
            Some("/tmp/project"),
            "title 외 컬럼은 기존처럼 COALESCE로 갱신돼야 함"
        );
        assert_eq!(git_branch.as_deref(), Some("feature"));
    }

    /// 커넥터가 metadata를 실어 보낸 스트림을 만든다(Linear 이슈의 상태/에픽 케이스).
    fn stream_input_with_metadata(id: &str, title: &str, metadata: serde_json::Value) -> StreamInput {
        StreamInput {
            id: id.to_string(),
            source: "linear".to_string(),
            kind: Some("session".to_string()),
            title: Some(title.to_string()),
            project: Some("Maintenance".to_string()),
            git_branch: None,
            started_at: None,
            ended_at: None,
            status: None,
            metadata: Some(metadata),
        }
    }

    #[test]
    fn upsert_stream_merges_metadata_so_connector_state_stays_current() {
        // Linear 이슈 상태는 QA→완료처럼 나중에 바뀐다. ON CONFLICT에서 metadata를 갱신하지 않으면
        // 최초 삽입 시점 값으로 고정돼 상태 축이 영원히 낡은 채로 남는다(실제로 그런 상태였다).
        let tmp = TempDir::new();
        let conn = open_and_migrate_at(&tmp.db_path()).unwrap();

        ingest(
            &conn,
            &IngestRequest {
                stream: Some(stream_input_with_metadata(
                    "linear:TICKET-1",
                    "TICKET-1 로그인 오류 수정",
                    serde_json::json!({ "linearProject": "결제 화면", "state": "QA", "stateType": "started" }),
                )),
                events: Vec::new(),
            },
        )
        .expect("첫 인제스트 성공해야 함");

        ingest(
            &conn,
            &IngestRequest {
                stream: Some(stream_input_with_metadata(
                    "linear:TICKET-1",
                    "TICKET-1 로그인 오류 수정",
                    serde_json::json!({ "linearProject": "결제 화면", "state": "완료", "stateType": "completed" }),
                )),
                events: Vec::new(),
            },
        )
        .expect("재인제스트 성공해야 함");

        let metadata: String = conn
            .query_row("SELECT metadata FROM streams WHERE id = 'linear:TICKET-1'", [], |r| r.get(0))
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(parsed["state"], "완료", "재수집 시 상태가 최신으로 갱신돼야 함");
        assert_eq!(parsed["stateType"], "completed");
        assert_eq!(parsed["linearProject"], "결제 화면");
    }

    #[test]
    fn upsert_stream_metadata_merge_keeps_manual_title_key() {
        // metadata 갱신을 "통째 덮어쓰기"로 구현하면 rename_stream이 심어둔 manualTitle이 조용히
        // 사라져 사용자 편집이 날아간다 — json_patch 병합이라 캡처가 모르는 키는 보존돼야 한다.
        let tmp = TempDir::new();
        let conn = open_and_migrate_at(&tmp.db_path()).unwrap();

        ingest(
            &conn,
            &IngestRequest {
                stream: Some(stream_input("linear:TICKET-2", "자동 제목", None, None)),
                events: Vec::new(),
            },
        )
        .expect("첫 인제스트 성공해야 함");
        rename_stream(&conn, "linear:TICKET-2", "사용자가 지정한 제목").expect("rename 성공해야 함");

        ingest(
            &conn,
            &IngestRequest {
                stream: Some(stream_input_with_metadata(
                    "linear:TICKET-2",
                    "새로 캡처된 제목",
                    serde_json::json!({ "state": "완료" }),
                )),
                events: Vec::new(),
            },
        )
        .expect("재인제스트 성공해야 함");

        let (title, metadata): (Option<String>, String) = conn
            .query_row("SELECT title, metadata FROM streams WHERE id = 'linear:TICKET-2'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(parsed["state"], "완료", "새 키는 병합돼야 함");
        assert!(
            parsed.get("manualTitle").is_some(),
            "manualTitle이 병합 과정에서 사라지면 안 됨"
        );
        assert_eq!(
            title.as_deref(),
            Some("사용자가 지정한 제목"),
            "manualTitle 보호 로직도 그대로 동작해야 함"
        );
    }

    #[test]
    fn upsert_stream_updates_title_for_non_manual_stream_as_before() {
        let tmp = TempDir::new();
        let conn = open_and_migrate_at(&tmp.db_path()).unwrap();

        ingest(
            &conn,
            &IngestRequest {
                stream: Some(stream_input("claude_code:sess-auto", "첫 제목", None, None)),
                events: Vec::new(),
            },
        )
        .expect("첫 인제스트 성공해야 함");

        ingest(
            &conn,
            &IngestRequest {
                stream: Some(stream_input("claude_code:sess-auto", "갱신된 제목", None, None)),
                events: Vec::new(),
            },
        )
        .expect("재인제스트 성공해야 함");

        let title: Option<String> = conn
            .query_row(
                "SELECT title FROM streams WHERE id = 'claude_code:sess-auto'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title.as_deref(), Some("갱신된 제목"), "rename 안 한 스트림은 기존처럼 갱신돼야 함");
    }
}
