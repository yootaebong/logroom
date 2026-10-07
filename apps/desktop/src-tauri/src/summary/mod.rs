//! 일일 AI 요약(M7-①, ADR-0016) — 발췌 생성([`excerpt`]) + 프롬프트([`prompts`]) +
//! 엔진 이중화([`engine`]) + `daily_summaries` 캐시 read/write(이 모듈). `lib.rs`의
//! `get_daily_summary`/`generate_daily_summary`/`preview_summary_input` 커맨드가 이 모듈을 사용한다.
//!
//! 주간/월간 롤업(M7-③)은 [`period`] 서브모듈 — 일간 요약을 원본 재발췌 없이 합성한다.
//!
//! 재개 브리핑(M7-②)은 [`resume`] 서브모듈 — 일간/주간/월간과 달리 "프로젝트 단위·재개 지향"이며
//! 캐시 저장이 없다(항상 온디맨드 최신 생성).
//!
//! 업무평가서(ADR-0017)는 [`review`] 서브모듈 — 월간·주간 요약 위에 한 칸 더 얹어 분기 평가·월간
//! 점검을 만든다.

pub mod engine;
pub mod excerpt;
pub mod period;
pub mod prompts;
pub mod resume;
pub mod review;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("시스템 시간")
        .as_millis() as i64
}

/// 캐시 행의 `prompt_version` 이 현재 [`prompts::PROMPT_VERSION`] 과 다르면 `true` — 조회 결과의
/// `stale` 필드. FE 가 이 값으로 "옛 프롬프트로 만든 요약"을 열 때 한 번 자동 재생성한다
/// (SummaryView.tsx). 빈 값(버전 기록 이전 행)도 낡은 것으로 본다([`is_stale_prompt_version`]과 같은 기준).
pub(crate) fn is_stale_version(prompt_version: &str) -> bool {
    prompt_version != prompts::PROMPT_VERSION
}

/// `(localDate, locale)` 캐시 1건 조회. FE 계약(camelCase)과 1:1 대응. 없으면 `None`
/// (`get_daily_summary` 커맨드 + `generate_daily_summary`의 `force=false` 캐시 히트 경로가 사용).
pub fn get_cached(conn: &Connection, local_date: &str, locale: &str) -> anyhow::Result<Option<Value>> {
    conn.query_row(
        "SELECT tz, engine, model, content, created_at, prompt_version FROM daily_summaries
         WHERE local_date = ?1 AND locale = ?2",
        params![local_date, locale],
        |row| {
            let prompt_version: String = row.get("prompt_version")?;
            Ok(json!({
                "localDate": local_date,
                "locale": locale,
                "tz": row.get::<_, String>("tz")?,
                "engine": row.get::<_, String>("engine")?,
                "model": row.get::<_, String>("model")?,
                "content": row.get::<_, String>("content")?,
                "createdAt": row.get::<_, i64>("created_at")?,
                "promptVersion": prompt_version,
                "stale": is_stale_version(&prompt_version),
            }))
        },
    )
    .optional()
    .map_err(Into::into)
}

/// `(localDate, locale)` 캐시를 upsert한다(`generate_daily_summary`가 생성 성공 후 호출).
/// 재생성 시 기존 행을 덮어쓴다(날짜/로케일 조합당 최신 요약 1건만 보관). `prompt_version`은
/// 호출부가 넘기지 않고 이 함수 내부에서 [`prompts::PROMPT_VERSION`]을 직접 기록한다 — 호출부마다
/// 넘기게 하면 깜빡하고 누락시킬 위험이 있기 때문이다(리뷰 지적 방지).
pub fn upsert(
    conn: &Connection,
    local_date: &str,
    locale: &str,
    tz: &str,
    engine: &str,
    model: &str,
    content: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO daily_summaries (local_date, locale, tz, engine, model, content, created_at, prompt_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(local_date, locale) DO UPDATE SET
           tz = excluded.tz,
           engine = excluded.engine,
           model = excluded.model,
           content = excluded.content,
           created_at = excluded.created_at,
           prompt_version = excluded.prompt_version",
        params![local_date, locale, tz, engine, model, content, now_ms(), prompts::PROMPT_VERSION],
    )?;
    Ok(())
}

/// 해당 일간 캐시(`local_date`, `locale`)의 `prompt_version`이 현재 [`prompts::PROMPT_VERSION`]과
/// 다르면 `true`(갱신 대상) — 캐시 행 자체가 없으면 `false`를 반환한다("없음"은 이 함수가 아니라
/// 캐시 부재 경로([`Cascade::OneLevel`](period::Cascade::OneLevel) 등)가 이미 별도로 처리하는
/// 계약이라 여기서는 "낡음"으로 취급하지 않는다). 주간 롤업의 [`period::Cascade::Refresh`]가 이
/// 함수로 어느 날짜를 재생성할지 판단한다(`period.rs::build_weekly_excerpt` 참고).
pub fn is_stale_prompt_version(conn: &Connection, local_date: &str, locale: &str) -> anyhow::Result<bool> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT prompt_version FROM daily_summaries WHERE local_date = ?1 AND locale = ?2",
            params![local_date, locale],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match stored {
        Some(v) => is_stale_version(&v),
        None => false,
    })
}

/// 발췌 생성(override 우선) → 엔진 호출 → `daily_summaries` upsert까지 수행하는 공용 내부
/// 파이프라인. `lib.rs::generate_daily_summary` 커맨드와 [`period`] 모듈의 주간 캐스케이드(빠진
/// 날짜를 채우는 경로)가 이 함수를 공유한다 — 로직을 두 곳에 복붙하지 않기 위함(리뷰 지적 방지).
///
/// enabled/캡처 일시정지 체크는 호출부(커맨드) 책임이며 이 함수는 하지 않는다 — 캐스케이드 중간에
/// 날짜마다 재검사하면 이미 상위(`generate_period_summary` 커맨드)에서 확인한 것과 중복이다.
///
/// DB 잠금은 엔진 호출(`engine::generate`, 네트워크/프로세스 await) 전후로 짧게 두 번만 건다 —
/// await 구간 동안 뮤텍스를 들고 있으면 다른 커맨드가 그 사이 막히기 때문(`lib.rs`의 기존
/// `generate_daily_summary` 커맨드와 동일한 lock-release-await-lock 패턴).
pub async fn generate_daily_and_cache(
    db: &crate::Db,
    local_date: &str,
    tz: &str,
    locale: &str,
    excerpt_override: Option<String>,
    cfg: &crate::capture::config::SummaryConfig,
) -> anyhow::Result<Value> {
    let excerpt = match excerpt_override {
        Some(text) => excerpt::sanitize_excerpt_override(&text)?,
        None => {
            let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            excerpt::build_daily_excerpt(&conn, local_date, tz, locale)?
        }
    };
    let prompt = prompts::prompt_for_locale(locale);
    let combined = format!("{prompt}\n\n{excerpt}");
    let generated = engine::generate(&combined, cfg).await?;

    let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
    upsert(
        &conn,
        local_date,
        locale,
        tz,
        &generated.engine,
        &generated.model,
        &generated.content,
    )?;
    get_cached(&conn, local_date, locale)?.ok_or_else(|| anyhow::anyhow!("요약 저장 후 조회에 실패했습니다"))
}

/// `(scope, scopeKey, locale)` 상세 캐시 1건 조회. FE 계약(camelCase)과 1:1 대응. 없으면 `None`
/// (`get_summary_detail` 커맨드 + `generate_summary_detail`의 `force=false` 캐시 히트 경로가 사용).
pub fn get_detail_cached(
    conn: &Connection,
    scope: &str,
    scope_key: &str,
    locale: &str,
) -> anyhow::Result<Option<Value>> {
    conn.query_row(
        "SELECT tz, engine, model, content, created_at, prompt_version FROM summary_details
         WHERE scope = ?1 AND scope_key = ?2 AND locale = ?3",
        params![scope, scope_key, locale],
        |row| {
            let prompt_version: String = row.get("prompt_version")?;
            Ok(json!({
                "scope": scope,
                "scopeKey": scope_key,
                "locale": locale,
                "tz": row.get::<_, String>("tz")?,
                "engine": row.get::<_, String>("engine")?,
                "model": row.get::<_, String>("model")?,
                "content": row.get::<_, String>("content")?,
                "createdAt": row.get::<_, i64>("created_at")?,
                "stale": is_stale_version(&prompt_version),
            }))
        },
    )
    .optional()
    .map_err(Into::into)
}

/// `(scope, scopeKey, locale)` 상세 캐시를 upsert한다(`generate_detail_and_cache`가 생성 성공 후
/// 호출). 재생성 시 기존 행을 덮어쓴다(scope·scopeKey·로케일 조합당 최신 상세 요약 1건만 보관).
/// `prompt_version`도 [`upsert`]와 동일하게 호출부가 넘기지 않고 이 함수 내부에서
/// [`prompts::PROMPT_VERSION`]을 직접 기록한다.
#[allow(clippy::too_many_arguments)]
pub fn upsert_detail(
    conn: &Connection,
    scope: &str,
    scope_key: &str,
    locale: &str,
    tz: &str,
    engine: &str,
    model: &str,
    content: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO summary_details (scope, scope_key, locale, tz, engine, model, content, created_at, prompt_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(scope, scope_key, locale) DO UPDATE SET
           tz = excluded.tz,
           engine = excluded.engine,
           model = excluded.model,
           content = excluded.content,
           created_at = excluded.created_at,
           prompt_version = excluded.prompt_version",
        params![scope, scope_key, locale, tz, engine, model, content, now_ms(), prompts::PROMPT_VERSION],
    )?;
    Ok(())
}

/// "상세 보기"(온디맨드) 생성 파이프라인 — `force=false`이고 캐시가 있으면 그대로 반환하고(재호출
/// 없음), 없거나 `force=true`면 발췌 생성 → `prompts::detail_prompt_for_locale` 조립 → 엔진 호출 →
/// `summary_details` upsert까지 수행한다. `lib.rs::generate_summary_detail` 커맨드가 이 함수를
/// 호출한다(enabled/캡처 일시정지 체크는 `generate_daily_summary`와 동일하게 호출부 책임).
///
/// 발췌는 새로 만들지 않고 scope에 맞는 기존 발췌 함수를 그대로 재사용한다 — `day`는
/// [`excerpt::build_daily_excerpt`], `week`/`month`는 [`period::build_weekly_excerpt`]/
/// [`period::build_monthly_excerpt`]. 주간·월간은 [`period::Cascade::CachedOnly`]로 호출해 하위
/// 요약을 새로 생성하지 않는다 — "상세 보기" 버튼 한 번이 대량 생성을 유발하면 안 되기 때문이다
/// (이미 캐시된 일간/주간 요약만 합성, 없으면 그 부분만 조용히 빠진다).
///
/// DB 잠금은 [`generate_daily_and_cache`]와 동일한 lock-release-await-lock 패턴을 따른다 — 엔진
/// 호출(await 구간) 동안 뮤텍스를 들고 있지 않는다.
pub async fn generate_detail_and_cache(
    db: &crate::Db,
    scope: &str,
    scope_key: &str,
    tz: &str,
    locale: &str,
    force: bool,
    cfg: &crate::capture::config::SummaryConfig,
) -> anyhow::Result<Value> {
    if !force {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
        if let Some(cached) = get_detail_cached(&conn, scope, scope_key, locale)? {
            return Ok(cached);
        }
    }

    let excerpt_text = match scope {
        "day" => {
            let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            excerpt::build_daily_excerpt(&conn, scope_key, tz, locale)?
        }
        "week" => period::build_weekly_excerpt(db, scope_key, tz, locale, cfg, period::Cascade::CachedOnly).await?,
        "month" => period::build_monthly_excerpt(db, scope_key, tz, locale, cfg, period::Cascade::CachedOnly).await?,
        other => anyhow::bail!("invalid scope: {other}"),
    };

    let prompt = prompts::detail_prompt_for_locale(locale);
    let combined = format!("{prompt}\n\n{excerpt_text}");
    let generated = engine::generate(&combined, cfg).await?;

    let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
    upsert_detail(
        &conn,
        scope,
        scope_key,
        locale,
        tz,
        &generated.engine,
        &generated.model,
        &generated.content,
    )?;
    get_detail_cached(&conn, scope, scope_key, locale)?
        .ok_or_else(|| anyhow::anyhow!("상세 요약 저장 후 조회에 실패했습니다"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!(
                "logroom-summary-mod-test-{}-{n}-{nanos}",
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

    #[test]
    fn get_cached_returns_none_when_missing() {
        let (_tmp, conn) = seeded_conn();
        assert_eq!(get_cached(&conn, "2026-07-16", "ko").unwrap(), None);
    }

    #[test]
    fn upsert_then_get_cached_round_trips() {
        let (_tmp, conn) = seeded_conn();
        upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 첫 요약").unwrap();

        let cached = get_cached(&conn, "2026-07-16", "ko").unwrap().expect("캐시 있어야 함");
        assert_eq!(cached["localDate"], json!("2026-07-16"));
        assert_eq!(cached["locale"], json!("ko"));
        assert_eq!(cached["tz"], json!("Asia/Seoul"));
        assert_eq!(cached["engine"], json!("cli"));
        assert_eq!(cached["model"], json!("sonnet"));
        assert_eq!(cached["content"], json!("- 첫 요약"));
        assert!(cached["createdAt"].as_i64().unwrap() > 0);
    }

    #[test]
    fn upsert_overwrites_existing_entry_for_same_date_and_locale() {
        let (_tmp, conn) = seeded_conn();
        upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 첫 요약").unwrap();
        upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "api", "claude-haiku-4-5", "- 재생성된 요약").unwrap();

        let cached = get_cached(&conn, "2026-07-16", "ko").unwrap().expect("캐시 있어야 함");
        assert_eq!(cached["engine"], json!("api"));
        assert_eq!(cached["content"], json!("- 재생성된 요약"));

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM daily_summaries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "같은 (localDate, locale)은 덮어써야지 별도 행이 되면 안 됨");
    }

    #[test]
    fn upsert_keeps_separate_entries_per_locale() {
        let (_tmp, conn) = seeded_conn();
        upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 한국어 요약").unwrap();
        upsert(&conn, "2026-07-16", "en", "Asia/Seoul", "cli", "sonnet", "- English summary").unwrap();

        assert_eq!(
            get_cached(&conn, "2026-07-16", "ko").unwrap().unwrap()["content"],
            json!("- 한국어 요약")
        );
        assert_eq!(
            get_cached(&conn, "2026-07-16", "en").unwrap().unwrap()["content"],
            json!("- English summary")
        );
    }

    #[test]
    fn upsert_stores_current_prompt_version() {
        let (_tmp, conn) = seeded_conn();
        upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 요약").unwrap();

        let cached = get_cached(&conn, "2026-07-16", "ko").unwrap().unwrap();
        assert_eq!(cached["promptVersion"], json!(prompts::PROMPT_VERSION));
    }

    // ── is_stale_prompt_version ──────────────────────────────────

    #[test]
    fn is_stale_prompt_version_false_when_current() {
        let (_tmp, conn) = seeded_conn();
        upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 요약").unwrap();
        assert!(!is_stale_prompt_version(&conn, "2026-07-16", "ko").unwrap());
    }

    #[test]
    fn is_stale_prompt_version_true_when_blank_or_older_version() {
        let (_tmp, conn) = seeded_conn();
        // upsert 직접 호출 없이 구버전 행을 그대로 재현(빈 문자열 = 마이그레이션 직후 기존 행).
        conn.execute(
            "INSERT INTO daily_summaries (local_date, locale, tz, engine, model, content, created_at, prompt_version)
             VALUES ('2026-07-16', 'ko', 'Asia/Seoul', 'cli', 'sonnet', '- 구버전 요약', 0, '')",
            [],
        )
        .unwrap();
        assert!(is_stale_prompt_version(&conn, "2026-07-16", "ko").unwrap());

        conn.execute(
            "INSERT INTO daily_summaries (local_date, locale, tz, engine, model, content, created_at, prompt_version)
             VALUES ('2026-07-17', 'ko', 'Asia/Seoul', 'cli', 'sonnet', '- 구버전 요약', 0, 'v1')",
            [],
        )
        .unwrap();
        assert!(is_stale_prompt_version(&conn, "2026-07-17", "ko").unwrap());
    }

    #[test]
    fn cached_results_expose_stale_flag_by_prompt_version() {
        // 조회 결과의 stale = 저장된 prompt_version 이 현재 버전과 다름(FE 자동 재생성 조건).
        let (_tmp, conn) = seeded_conn();
        upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 요약").unwrap();
        upsert_detail(&conn, "day", "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "상세").unwrap();
        assert_eq!(get_cached(&conn, "2026-07-16", "ko").unwrap().unwrap()["stale"], json!(false));
        assert_eq!(
            get_detail_cached(&conn, "day", "2026-07-16", "ko").unwrap().unwrap()["stale"],
            json!(false)
        );

        conn.execute("UPDATE daily_summaries SET prompt_version = 'v9'", []).unwrap();
        conn.execute("UPDATE summary_details SET prompt_version = ''", []).unwrap();
        assert_eq!(get_cached(&conn, "2026-07-16", "ko").unwrap().unwrap()["stale"], json!(true));
        assert_eq!(
            get_detail_cached(&conn, "day", "2026-07-16", "ko").unwrap().unwrap()["stale"],
            json!(true)
        );
    }

    #[test]
    fn is_stale_version_compares_with_current_prompt_version() {
        assert!(!is_stale_version(prompts::PROMPT_VERSION));
        assert!(is_stale_version("v9"));
        assert!(is_stale_version(""));
    }

    #[test]
    fn is_stale_prompt_version_false_when_row_missing() {
        let (_tmp, conn) = seeded_conn();
        assert!(!is_stale_prompt_version(&conn, "2026-07-16", "ko").unwrap());
    }

    // ── 상세 보기(summary_details) 캐시 get/upsert ─────────────────

    #[test]
    fn get_detail_cached_returns_none_when_missing() {
        let (_tmp, conn) = seeded_conn();
        assert_eq!(get_detail_cached(&conn, "day", "2026-07-16", "ko").unwrap(), None);
    }

    #[test]
    fn upsert_detail_then_get_detail_cached_round_trips() {
        let (_tmp, conn) = seeded_conn();
        upsert_detail(&conn, "day", "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "## logroom\n첫 상세 요약")
            .unwrap();

        let cached = get_detail_cached(&conn, "day", "2026-07-16", "ko").unwrap().expect("캐시 있어야 함");
        assert_eq!(cached["scope"], json!("day"));
        assert_eq!(cached["scopeKey"], json!("2026-07-16"));
        assert_eq!(cached["locale"], json!("ko"));
        assert_eq!(cached["tz"], json!("Asia/Seoul"));
        assert_eq!(cached["engine"], json!("cli"));
        assert_eq!(cached["model"], json!("sonnet"));
        assert_eq!(cached["content"], json!("## logroom\n첫 상세 요약"));
        assert!(cached["createdAt"].as_i64().unwrap() > 0);
    }

    #[test]
    fn upsert_detail_stores_current_prompt_version() {
        let (_tmp, conn) = seeded_conn();
        upsert_detail(&conn, "day", "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 상세").unwrap();

        let stored: String = conn
            .query_row(
                "SELECT prompt_version FROM summary_details WHERE scope = 'day' AND scope_key = '2026-07-16' AND locale = 'ko'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, prompts::PROMPT_VERSION);
    }

    #[test]
    fn upsert_detail_keeps_separate_rows_per_locale_for_same_scope_key() {
        let (_tmp, conn) = seeded_conn();
        upsert_detail(&conn, "day", "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 한국어 상세").unwrap();
        upsert_detail(&conn, "day", "2026-07-16", "en", "Asia/Seoul", "cli", "sonnet", "- English detail").unwrap();

        assert_eq!(
            get_detail_cached(&conn, "day", "2026-07-16", "ko").unwrap().unwrap()["content"],
            json!("- 한국어 상세")
        );
        assert_eq!(
            get_detail_cached(&conn, "day", "2026-07-16", "en").unwrap().unwrap()["content"],
            json!("- English detail")
        );

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM summary_details", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2, "같은 (scope, scopeKey)라도 locale이 다르면 별도 행이어야 함");
    }

    #[test]
    fn upsert_detail_overwrites_existing_entry_for_same_scope_and_key() {
        let (_tmp, conn) = seeded_conn();
        upsert_detail(&conn, "week", "2026-07-13", "ko", "Asia/Seoul", "cli", "sonnet", "- 첫 상세").unwrap();
        upsert_detail(
            &conn,
            "week",
            "2026-07-13",
            "ko",
            "Asia/Seoul",
            "api",
            "claude-haiku-4-5",
            "- 재생성된 상세",
        )
        .unwrap();

        let cached = get_detail_cached(&conn, "week", "2026-07-13", "ko").unwrap().expect("캐시 있어야 함");
        assert_eq!(cached["engine"], json!("api"));
        assert_eq!(cached["content"], json!("- 재생성된 상세"));

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM summary_details", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "같은 (scope, scopeKey, locale)은 덮어써야지 별도 행이 되면 안 됨");
    }
}
