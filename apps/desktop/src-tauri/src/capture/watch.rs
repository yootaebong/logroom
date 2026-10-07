//! `~/.claude*/projects/**/*.jsonl`(자동 탐지 + config.rs) + `~/.kiro/sessions/cli/*.jsonl` backfill + 상시 감시
//! (docs/03-capture.md, docs/06-roadmap.md M1·M2).
//! 저장 경로: 파일 새 바이트 읽기 → normalize → `db::ingest()` 직접 호출(HTTP 인제스트 서버 우회).
//! cursor(`capture_cursors`)로 idempotent 재개. 감시 root 목록은 [`config::resolve_roots`]가 해석하며,
//! 소스별 root 하위 경로로 어느 어댑터를 쓸지 판별한다.

use crate::capture::config::{self, BodyPolicy, Source};
use crate::capture::hub::{self, RepoCache};
use crate::capture::kiro::{self, KiroContext};
use crate::capture::normalize::{self, FileContext};
use crate::capture::scrub;
use crate::db;
use crate::model::IngestRequest;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

type Db = Arc<Mutex<Connection>>;

/// notify 이벤트 드레인 창(한 응답 안의 다중 write를 한 번에 묶기 위한 대기 시간).
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(200);

fn is_jsonl(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
}

/// `subagents/agent-<id>.jsonl` 경로면 `<id>` 추출, 메인 세션 파일이면 `None`.
fn extract_agent_id(path: &Path) -> Option<String> {
    let parent_name = path.parent()?.file_name()?.to_str()?;
    if parent_name != "subagents" {
        return None;
    }
    path.file_stem()?
        .to_str()?
        .strip_prefix("agent-")
        .map(str::to_string)
}

/// 디렉토리 하위 모든 `.jsonl` 파일을 재귀 수집한다. backfill(이 파일)과
/// `capture/health.rs`(캡처 헬스 스캔) 양쪽에서 재사용한다.
pub(crate) fn walk_jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_jsonl_files(&path));
        } else if is_jsonl(&path) {
            out.push(path);
        }
    }
    out
}

/// 앱 시작 시 백그라운드 스레드로 backfill + notify 상시감시를 띄운다.
/// 감시 대상 root(Claude/Kiro CLI)가 하나도 없으면 조용히 종료.
/// `capture_paused`(M4)는 `lib.rs`가 ingest 서버와 공유하는 런타임 플래그 — 매 파일 처리 직전
/// 최신 값을 읽어 즉시 반영한다(재시작 불필요, docs/04-privacy-security.md "일시정지 의미론").
pub fn spawn(app: AppHandle, db: Db, capture_paused: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        if let Err(e) = run(app, db, capture_paused) {
            eprintln!("[logroom] capture watch error: {e}");
        }
    });
}

fn run(app: AppHandle, db: Db, capture_paused: Arc<AtomicBool>) -> anyhow::Result<()> {
    // root 목록(존재 확인 + 자동 탐지 + 설정 파일 extraRoots 반영 + 중복 제거)은 config.rs가 전담한다.
    let existing: Vec<(Source, PathBuf)> = config::resolve_roots();
    if existing.is_empty() {
        eprintln!("[logroom] 캡처 대상 디렉토리 없음(Claude Code/Kiro CLI 미사용) — 감시 안 함");
        return Ok(());
    }
    // body/metadata 절단 정책(ADR-0012)은 파일 처리 시작 전 한 번만 읽어 backfill·watch 양쪽에 공유한다.
    let body_policy = config::load_body_policy();
    // 시크릿 스크럽(M4) 사용 여부도 동일하게 시작 전 한 번만 읽는다(기본 true).
    let scrub_secrets = config::load_scrub_secrets();
    // 캡처 제외 프로젝트 목록도 동일하게 시작 전 한 번만 읽는다(재시작 후 적용, 기본 빈 목록).
    let exclude_projects = config::load_exclude_projects();
    // hub 세션(cwd 가 레포 밖) 레포 재귀속용 디렉토리→레포 루트 캐시. 감시 스레드 수명 동안 유지한다.
    let mut hub_cache = RepoCache::new();

    // 0) 이 기능 이전에 쌓인 hub 세션 소급(마커가 있으면 즉시 반환, 스트림마다 락을 잡고 놓는다).
    if let Err(e) = hub::backfill_hub_streams(&db, &exclude_projects) {
        eprintln!("[logroom] hub backfill error: {e}");
    }

    // 1) 상시 감시부터 등록: backfill 도중 append되는 라인도 놓치지 않는다("기록 최우선").
    //    watcher는 이 함수(스레드) 스코프 동안 살아있어야 하므로 아래 루프가 끝날 때까지 drop하지 않는다.
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    for (_, root) in &existing {
        watcher.watch(root, RecursiveMode::Recursive)?;
    }

    // 2) backfill: 기존에 있던 모든 .jsonl 을 cursor 위치부터 읽는다.
    //    backfill 도중 채널에 쌓인 이벤트는 아래 이벤트 루프에서 처리되며,
    //    (source, external_id) UNIQUE라 backfill과 중복 처리돼도 idempotent.
    for (source, root) in &existing {
        for path in walk_jsonl_files(root) {
            let paused = capture_paused.load(Ordering::Relaxed);
            match process_file(
                &db,
                &path,
                *source,
                body_policy,
                scrub_secrets,
                paused,
                &exclude_projects,
                &mut hub_cache,
            ) {
                Ok(inserted) if inserted > 0 => {
                    let _ = app.emit("event-ingested", serde_json::json!({ "inserted": inserted }));
                }
                Ok(_) => {}
                Err(e) => eprintln!("[logroom] backfill {}: {e}", path.display()),
            }
        }
    }

    // 3) 이벤트 루프: notify 콜백 → channel → 이 스레드에서 드레인 후 처리.
    //    한 응답 안에서 발생하는 다중 write가 여러 notify 이벤트를 유발해도
    //    DEBOUNCE_WINDOW 동안 경로를 dedup 수집해 process_file을 경로당 1회만 호출한다
    //    (파일 open/seek/DB lock 반복 감소 — cursor 기반이라 정확성에는 영향 없음).
    while let Ok(first) = rx.recv() {
        let paths = drain_changed_paths(&rx, first);
        for path in paths {
            let Some((source, _)) = existing.iter().find(|(_, root)| path.starts_with(root)) else {
                eprintln!("[logroom] {}: 알 수 없는 root, 스킵", path.display());
                continue;
            };
            let paused = capture_paused.load(Ordering::Relaxed);
            match process_file(
                &db,
                &path,
                *source,
                body_policy,
                scrub_secrets,
                paused,
                &exclude_projects,
                &mut hub_cache,
            ) {
                Ok(inserted) if inserted > 0 => {
                    let _ = app.emit("event-ingested", serde_json::json!({ "inserted": inserted }));
                }
                Ok(_) => {}
                Err(e) => eprintln!("[logroom] watch {}: {e}", path.display()),
            }
        }
    }

    Ok(())
}

/// `first` 이벤트로 드레인을 시작해 `DEBOUNCE_WINDOW` 동안 추가로 도착하는 notify 이벤트를 모은다.
/// Modify/Create + `.jsonl`인 이벤트의 경로만 최초 등장 순서대로 dedup해 반환한다 —
/// 다중 write가 짧은 시간에 여러 이벤트를 유발해도 호출부가 경로당 `process_file`을 1회만 호출하게 한다.
fn drain_changed_paths(
    rx: &mpsc::Receiver<notify::Result<notify::Event>>,
    first: notify::Result<notify::Event>,
) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut ordered = Vec::new();

    let mut collect = |res: notify::Result<notify::Event>| match res {
        Ok(event) => {
            if !matches!(event.kind, notify::EventKind::Modify(_) | notify::EventKind::Create(_)) {
                return;
            }
            for path in event.paths {
                if is_jsonl(&path) && seen.insert(path.clone()) {
                    ordered.push(path);
                }
            }
        }
        Err(e) => eprintln!("[logroom] notify error: {e}"),
    };

    collect(first);
    while let Ok(res) = rx.recv_timeout(DEBOUNCE_WINDOW) {
        collect(res);
    }

    ordered
}

/// 파일 하나를 cursor offset부터 끝까지(마지막 개행까지만) 읽어 정규화·인제스트하고 cursor를 전진시킨다.
/// 반환값은 실제 삽입된 이벤트 수 — 호출부(`run`)가 이 값으로 `event-ingested` emit 여부를 결정한다
/// (emit 자체는 `AppHandle`이 필요 없는 이 함수 밖에서 처리해 단위테스트 가능하게 분리했다).
/// backfill과 watch 콜백 양쪽에서 재사용 — `(source, external_id)` UNIQUE라 재파싱돼도 idempotent.
/// `body_policy`(ADR-0012)는 tool_result body·tool_use metadata 절단 여부를 결정하고,
/// `scrub_secrets`(M4)는 ingest 직전 시크릿 패턴 마스킹(`scrub::scrub_request`) 적용 여부를 결정한다.
/// `capture_paused`(M4)가 true면 라인 파싱/스크럽/ingest를 모두 생략하고 cursor(offset/mtime)만
/// 전진시킨다 — 일시정지 중 발생한 활동은 재개 후에도 소급 수집되지 않는다("일시정지"의 정직한
/// 의미, docs/04-privacy-security.md "일시정지 의미론").
/// `exclude_projects`(제외할 프로젝트)에 정규화된 `project`가 매치되면 `capture_paused`와 동일하게
/// cursor만 전진시키고 저장은 영구 스킵한다(소급 삭제가 아니라 "매치된 이후" 활동만 미기록).
/// Claude 소스의 hub 스트림(metadata.hub)이면 같은 락 안에서 레포 재귀속(capture/hub.rs)까지 한다.
#[allow(clippy::too_many_arguments)]
fn process_file(
    db: &Db,
    path: &Path,
    source: Source,
    body_policy: BodyPolicy,
    scrub_secrets: bool,
    capture_paused: bool,
    exclude_projects: &[String],
    hub_cache: &mut RepoCache,
) -> anyhow::Result<usize> {
    let resource = path.to_string_lossy().to_string();
    let cursor_source = source.cursor_source();

    let cursor = {
        let conn = db.lock().expect("db mutex poisoned");
        db::get_cursor(&conn, cursor_source, &resource)?
    };
    let mut start_offset = cursor.map(|(offset, _)| offset).unwrap_or(0);
    let stored_mtime = cursor.and_then(|(_, mtime)| mtime);

    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let file_len = metadata.len();
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64);

    if start_offset < 0 || start_offset as u64 > file_len {
        // 파일 회전/교체 등으로 offset이 현재 크기를 벗어나면 처음부터 다시 읽는다.
        start_offset = 0;
    }
    if let (Some(stored), Some(current)) = (stored_mtime, mtime) {
        if current < stored {
            // 저장된 mtime보다 현재 mtime이 과거 → 파일이 교체/되돌려짐. 처음부터 다시 읽는다.
            start_offset = 0;
        }
    }
    file.seek(SeekFrom::Start(start_offset as u64))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    if buf.is_empty() {
        return Ok(0);
    }

    // 완전한 라인(마지막 개행)까지만 처리 — 아직 flush 안 된 라인은 다음 호출까지 대기.
    let Some(last_newline) = buf.iter().rposition(|&b| b == b'\n') else {
        return Ok(0);
    };
    let complete = &buf[..=last_newline];
    let new_offset = start_offset + complete.len() as i64;

    if capture_paused {
        // 일시정지: 개행 경계까지의 새 바이트를 읽었지만 파싱/스크럽/ingest는 전부 생략하고
        // cursor만 전진시킨다 — 재개해도 이 구간은 소급 수집되지 않는다.
        let conn = db.lock().expect("db mutex poisoned");
        db::upsert_cursor(&conn, cursor_source, &resource, new_offset, mtime)?;
        return Ok(0);
    }

    let text = String::from_utf8_lossy(complete);
    let mut parse_failures = 0usize;
    let lines: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str::<Value>(l) {
            Ok(v) => Some(v),
            Err(_) => {
                parse_failures += 1;
                None
            }
        })
        .collect();
    if parse_failures > 0 {
        eprintln!("[logroom] {}: {parse_failures}줄 파싱 실패", path.display());
    }

    let mut req = match source {
        Source::Claude => {
            let ctx = FileContext {
                agent_id: extract_agent_id(path),
            };
            normalize::normalize_lines(&lines, &ctx, body_policy)
        }
        Source::KiroCli => normalize_kiro_file(db, path, &lines, body_policy)?,
    };

    // 제외 프로젝트(정규화된 project가 exclude_projects prefix에 매치) — capture_paused와 동일한
    // 패턴으로 저장은 스킵하고 cursor만 전진시킨다.
    let project_excluded = req
        .as_ref()
        .and_then(|r| r.stream.as_ref())
        .and_then(|s| s.project.as_deref())
        .is_some_and(|project| config::project_matches_exclude(project, exclude_projects));
    if project_excluded {
        let conn = db.lock().expect("db mutex poisoned");
        db::upsert_cursor(&conn, cursor_source, &resource, new_offset, mtime)?;
        return Ok(0);
    }

    // claude/kiro 공통 ingest 직전 경로: 시크릿 스크럽(M4, docs/03-capture.md 공통 규칙).
    if scrub_secrets {
        if let Some(req) = req.as_mut() {
            scrub::scrub_request(req);
        }
    }

    let inserted = {
        let conn = db.lock().expect("db mutex poisoned");
        let inserted = match &req {
            Some(req) => db::ingest(&conn, req)?,
            None => 0,
        };
        // hub 세션: 새로 들어온 이벤트를 턴마다 실제 레포 자식 스트림으로 옮긴다(agent 는 전체 투표).
        // 실패해도 캡처(커서 전진)는 막지 않는다 — base 에 남은 이벤트는 다음 인제스트 때 다시 본다.
        if source == Source::Claude && inserted > 0 {
            if let Some(stream) = req.as_ref().and_then(|r| r.stream.as_ref()) {
                if let Err(e) = hub::reattribute_if_hub(&conn, &stream.id, exclude_projects, hub_cache) {
                    eprintln!("[logroom] hub reattribute {}: {e}", stream.id);
                }
            }
        }
        db::upsert_cursor(&conn, cursor_source, &resource, new_offset, mtime)?;
        inserted
    };

    Ok(inserted)
}

/// Kiro CLI jsonl 파일 하나 처리: 세션 메타(`<sessionId>.json`) 로드 → ts 보간 시작점 산출 →
/// `kiro::normalize_kiro_lines` 호출. 메타는 매 호출마다 다시 읽는다(title 갱신은 upsert COALESCE로 자연 반영).
fn normalize_kiro_file(
    db: &Db,
    path: &Path,
    lines: &[Value],
    body_policy: BodyPolicy,
) -> anyhow::Result<Option<IngestRequest>> {
    let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else {
        return Ok(None);
    };
    let session_id = session_id.to_string();

    let meta_path = path.with_extension("json");
    let meta: Option<Value> = match fs::read(&meta_path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(v) => Some(v),
            Err(_) => {
                eprintln!("[logroom] {}: 세션 메타 파싱 실패", meta_path.display());
                None
            }
        },
        // 파일 부재는 정상 케이스(메타 미기록)이므로 조용히 넘어간다.
        Err(_) => None,
    };

    let title = meta.as_ref().and_then(|m| m.get("title")).and_then(Value::as_str).map(str::to_string);
    let cwd = meta.as_ref().and_then(|m| m.get("cwd")).and_then(Value::as_str);
    let project = cwd.map(normalize::normalize_project);
    let parent_session_id = meta
        .as_ref()
        .and_then(|m| m.get("parent_session_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let created_at = meta.as_ref().and_then(|m| m.get("created_at")).and_then(Value::as_str);

    let ctx = KiroContext {
        session_id: session_id.clone(),
        title,
        project,
        parent_session_id,
    };

    // start_ts 우선순위: 이 스트림의 기존 최신 ts → 메타 created_at(ISO8601) → 파일 mtime → 0.
    let stream_max_ts = {
        let conn = db.lock().expect("db mutex poisoned");
        db::max_ts_of_stream(&conn, &kiro::stream_id(&session_id))?
    };
    let start_ts = stream_max_ts
        .or_else(|| {
            created_at
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp_millis())
        })
        .or_else(|| {
            fs::metadata(path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
        })
        .unwrap_or(0);

    Ok(kiro::normalize_kiro_lines(lines, &ctx, start_ts, body_policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use std::sync::atomic::AtomicU64;
    use std::time::SystemTime;

    /// `config.rs`/`query.rs` 테스트와 동일 패턴 — `tempfile` 크레이트 없이 임시 디렉토리를 만든다.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("logroom-watch-test-{}-{n}-{nanos}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// V1~ 최신까지 마이그레이션이 적용된 빈 DB 커넥션(`query.rs::tests::seeded_conn`과 동일 패턴).
    fn seeded_db(tmp: &TempDir) -> Db {
        let conn = crate::db::open_and_migrate_at(&tmp.path.join("logroom.db")).expect("마이그레이션 성공");
        Arc::new(Mutex::new(conn))
    }

    /// 정규화 가능한 최소 `user` 라인 1개(`normalize.rs::tests::user_text_becomes_prompt`와 동일 shape)를
    /// jsonl 파일로 써서 경로를 돌려준다.
    fn write_session_line(tmp: &TempDir) -> PathBuf {
        let path = tmp.path.join("session.jsonl");
        let line = json!({
            "type": "user",
            "uuid": "line-1",
            "parentUuid": null,
            "sessionId": "sess-1",
            "cwd": "/tmp/example",
            "timestamp": "2026-07-01T00:00:00Z",
            "message": { "content": [ { "type": "text", "text": "일시정지 중 프롬프트" } ] }
        });
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, "{line}").unwrap();
        path
    }

    /// [`write_session_line`]과 동일하지만 `cwd`(→ project 정규화 대상)를 지정할 수 있다
    /// (excludeProjects 매치 테스트용). 인자로 받은 `cwd`에 `.git`이 없으므로
    /// `normalize::normalize_project`는 `cwd`를 그대로 project로 돌려준다.
    fn write_session_line_with_cwd(tmp: &TempDir, cwd: &str) -> PathBuf {
        let path = tmp.path.join("session.jsonl");
        let line = json!({
            "type": "user",
            "uuid": "line-1",
            "parentUuid": null,
            "sessionId": "sess-1",
            "cwd": cwd,
            "timestamp": "2026-07-01T00:00:00Z",
            "message": { "content": [ { "type": "text", "text": "제외 프로젝트 테스트 프롬프트" } ] }
        });
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, "{line}").unwrap();
        path
    }

    fn event_count(db: &Db) -> i64 {
        let conn = db.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn process_file_paused_advances_cursor_without_storing_events() {
        let tmp = TempDir::new();
        let db = seeded_db(&tmp);
        let path = write_session_line(&tmp);
        let file_len = fs::metadata(&path).unwrap().len() as i64;

        let inserted =
            process_file(&db, &path, Source::Claude, BodyPolicy::Essential, true, true, &[], &mut RepoCache::new()).expect("처리 성공");

        assert_eq!(inserted, 0);
        assert_eq!(event_count(&db), 0);

        let conn = db.lock().unwrap();
        let cursor = db::get_cursor(&conn, Source::Claude.cursor_source(), &path.to_string_lossy())
            .unwrap()
            .expect("일시정지 중에도 cursor는 남아야 함");
        assert_eq!(cursor.0, file_len, "cursor offset이 파일 끝까지 전진해야 함");
    }

    #[test]
    fn process_file_resumed_after_pause_does_not_retroactively_capture_skipped_bytes() {
        let tmp = TempDir::new();
        let db = seeded_db(&tmp);
        let path = write_session_line(&tmp);

        // 1) 일시정지 상태로 처리 — cursor가 파일 끝까지 전진하고 아무것도 저장되지 않는다.
        process_file(&db, &path, Source::Claude, BodyPolicy::Essential, true, true, &[], &mut RepoCache::new()).expect("처리 성공");
        assert_eq!(event_count(&db), 0);

        // 2) 재개(paused=false) 후 같은 파일을 다시 처리해도 이미 지나간 구간은 소급 수집되지
        //    않는다(새 바이트가 없어 buf가 비므로 즉시 반환) — "일시정지"의 정직한 의미.
        let inserted =
            process_file(&db, &path, Source::Claude, BodyPolicy::Essential, true, false, &[], &mut RepoCache::new()).expect("처리 성공");
        assert_eq!(inserted, 0);
        assert_eq!(event_count(&db), 0);
    }

    #[test]
    fn process_file_not_paused_stores_events_and_advances_cursor() {
        // 대조군: 같은 입력으로 paused=false면 정상적으로 이벤트가 저장됨을 함께 확인한다.
        let tmp = TempDir::new();
        let db = seeded_db(&tmp);
        let path = write_session_line(&tmp);

        let inserted =
            process_file(&db, &path, Source::Claude, BodyPolicy::Essential, true, false, &[], &mut RepoCache::new()).expect("처리 성공");

        assert_eq!(inserted, 1);
        assert_eq!(event_count(&db), 1);
    }

    // ── excludeProjects(프로젝트 제외) ───────────────────────────

    #[test]
    fn process_file_excluded_project_advances_cursor_without_storing_events() {
        let tmp = TempDir::new();
        let db = seeded_db(&tmp);
        let cwd = tmp.path.join("secret-project");
        let path = write_session_line_with_cwd(&tmp, cwd.to_str().unwrap());
        let file_len = fs::metadata(&path).unwrap().len() as i64;
        let exclude_projects = vec![cwd.to_string_lossy().to_string()];

        let inserted = process_file(
            &db,
            &path,
            Source::Claude,
            BodyPolicy::Essential,
            true,
            false,
            &exclude_projects,
            &mut RepoCache::new(),
        )
        .expect("처리 성공");

        assert_eq!(inserted, 0, "제외 프로젝트는 저장되지 않아야 함");
        assert_eq!(event_count(&db), 0);

        let conn = db.lock().unwrap();
        let cursor = db::get_cursor(&conn, Source::Claude.cursor_source(), &path.to_string_lossy())
            .unwrap()
            .expect("제외되어도 cursor는 전진해야 함(영구 미기록, 소급 삭제 아님)");
        assert_eq!(cursor.0, file_len, "cursor offset이 파일 끝까지 전진해야 함");
    }

    #[test]
    fn process_file_non_excluded_project_stores_events_normally() {
        // 대조군: exclude_projects에 매치되지 않는 project는 정상 저장된다.
        let tmp = TempDir::new();
        let db = seeded_db(&tmp);
        let cwd = tmp.path.join("normal-project");
        let path = write_session_line_with_cwd(&tmp, cwd.to_str().unwrap());
        let exclude_projects = vec![tmp.path.join("secret-project").to_string_lossy().to_string()];

        let inserted = process_file(
            &db,
            &path,
            Source::Claude,
            BodyPolicy::Essential,
            true,
            false,
            &exclude_projects,
            &mut RepoCache::new(),
        )
        .expect("처리 성공");

        assert_eq!(inserted, 1, "매치되지 않는 project는 정상 저장돼야 함");
        assert_eq!(event_count(&db), 1);
    }

    // ── notify 디바운스 드레인 ───────────────────────────

    #[test]
    fn drain_changed_paths_dedups_in_first_seen_order() {
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let path_a = PathBuf::from("/tmp/a.jsonl");
        let path_b = PathBuf::from("/tmp/b.jsonl");
        let non_jsonl = PathBuf::from("/tmp/ignore.txt");

        let modify_event = |path: &PathBuf| {
            Ok(notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
                .add_path(path.clone()))
        };

        // 채널에 미리 중복/무관 경로를 밀어넣어 둔다 — drain_changed_paths가 이들을 모두
        // 소진(dedup)하고 DEBOUNCE_WINDOW 타임아웃 후 최초 등장 순서로 반환해야 한다.
        tx.send(modify_event(&path_b)).unwrap();
        tx.send(modify_event(&path_a)).unwrap(); // 중복(path_a는 first로도 옴)
        tx.send(modify_event(&non_jsonl)).unwrap(); // .jsonl 아님 — 제외
        tx.send(modify_event(&path_b)).unwrap(); // 중복

        let first = modify_event(&path_a);
        let paths = drain_changed_paths(&rx, first);

        assert_eq!(paths, vec![path_a, path_b]);
    }
}
