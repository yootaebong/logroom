mod capture;
mod data_admin;
mod db;
mod ingest;
mod model;
mod query;
mod summary;
mod tray;
mod update;

use capture::config::{self, CaptureConfig, ReviewProfile, SummaryConfig};
use rusqlite::Connection;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::MacosLauncher;

/// `pub(crate)`: `summary::period`(M7-③)의 주간/월간 캐스케이드도 daily 생성 파이프라인과 동일한
/// 락-해제-await-락 패턴을 쓰기 위해 이 타입이 필요하다.
pub(crate) type Db = Arc<Mutex<Connection>>;
/// 캡처 일시정지(M4) 런타임 플래그. watch.rs 감시 스레드 + ingest 서버(AppState) + 아래 두 커맨드가
/// 같은 `Arc`를 공유해 재시작 없이 즉시 반영된다(docs/04-privacy-security.md "일시정지 의미론").
type CapturePausedFlag = Arc<AtomicBool>;

// 읽기 커맨드: query.rs 의 SQL 구현 호출. 실패 시 panic 없이 빈 값 반환.
#[tauri::command]
fn list_events_by_day(local_date: String, tz: String, db: State<'_, Db>) -> serde_json::Value {
    let conn = db.lock().expect("db mutex poisoned");
    query::list_events_by_day(&conn, &local_date, &tz).unwrap_or_else(|e| {
        eprintln!("[logroom] list_events_by_day: {e}");
        serde_json::json!({ "streams": [], "events": [] })
    })
}

#[tauri::command]
fn list_my_messages_by_day(local_date: String, tz: String, db: State<'_, Db>) -> serde_json::Value {
    let conn = db.lock().expect("db mutex poisoned");
    query::list_my_messages_by_day(&conn, &local_date, &tz).unwrap_or_else(|e| {
        eprintln!("[logroom] list_my_messages_by_day: {e}");
        serde_json::json!({ "messages": [] })
    })
}

// sources/types/from_ts/to_ts는 모두 선택(FE 계약, docs/05-ui-ux.md "② Search" 필터).
// 미지정/빈 배열은 전체를 의미한다 — query.rs::build_event_filter 참고.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn search_events(
    query: String,
    sources: Option<Vec<String>>,
    types: Option<Vec<String>>,
    from_ts: Option<i64>,
    to_ts: Option<i64>,
    db: State<'_, Db>,
) -> Vec<serde_json::Value> {
    let conn = db.lock().expect("db mutex poisoned");
    let sources = sources.unwrap_or_default();
    let types = types.unwrap_or_default();
    query::search_events(&conn, &query, &sources, &types, from_ts, to_ts).unwrap_or_else(|e| {
        eprintln!("[logroom] search_events: {e}");
        Vec::new()
    })
}

// fromTs/toTs는 모두 선택(FE 계약, StreamDetailPanel.tsx "메시지형 스트림은 그날만" 조회).
// 둘 다 지정되면 이벤트를 [fromTs, toTs) 구간으로 좁히고, 하나라도 없으면 현행대로 전체 이벤트를
// 반환한다 — query.rs::get_stream 참고.
#[tauri::command]
fn get_stream(
    id: String,
    from_ts: Option<i64>,
    to_ts: Option<i64>,
    db: State<'_, Db>,
) -> Option<serde_json::Value> {
    let conn = db.lock().expect("db mutex poisoned");
    query::get_stream(&conn, &id, from_ts, to_ts).unwrap_or_else(|e| {
        eprintln!("[logroom] get_stream: {e}");
        None
    })
}

/// 스트림 제목 수동 편집(연필 아이콘 → 편집). trim 후 빈 문자열이면 에러(db.rs 참고).
/// 이후 재캡처가 흘러들어와도 `db::upsert_stream`이 이 title을 보호한다.
#[tauri::command]
fn rename_stream(id: String, title: String, db: State<'_, Db>) -> Result<(), String> {
    let conn = db.lock().expect("db mutex poisoned");
    db::rename_stream(&conn, &id, &title).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_digest(local_date: String, tz: String, db: State<'_, Db>) -> serde_json::Value {
    let conn = db.lock().expect("db mutex poisoned");
    query::get_digest(&conn, &local_date, &tz).unwrap_or_else(|e| {
        eprintln!("[logroom] get_digest: {e}");
        serde_json::json!({
            "totals": {
                "streams": 0,
                "agentStreams": 0,
                "prompts": 0,
                "responses": 0,
                "toolEvents": 0,
                "tokensIn": 0,
                "tokensOut": 0,
                "firstTs": null,
                "lastTs": null,
            },
            "projects": [],
        })
    })
}

/// 전체 프로젝트 표시명 목록(요약 뷰 "재개" 스코프의 프로젝트 드롭다운, M7-② 발견성 개선) — 날짜
/// 범위와 무관하게 DB 전체 스트림을 대상으로 한다(다이제스트/타임라인의 그날 프로젝트 목록과 달리
/// "재개할 수 있는 모든 프로젝트"를 보여줘야 하므로).
#[tauri::command]
fn list_projects(db: State<'_, Db>) -> Vec<String> {
    let conn = db.lock().expect("db mutex poisoned");
    query::list_projects(&conn).unwrap_or_else(|e| {
        eprintln!("[logroom] list_projects: {e}");
        Vec::new()
    })
}

// 일일 AI 요약 커맨드(M7-①, ADR-0016): 설정 조회/저장 + 캐시 조회/생성 + 전송 내용 미리보기.
// summary/ 모듈(mod.rs·excerpt.rs·prompts.rs·engine.rs) 참고.

/// 요약 설정 조회. `anthropicApiKey`/`openaiApiKey`/`geminiApiKey` 3개 키 각각 캡처 설정의
/// slack/github/linear 토큰과 동일한 마스킹 왕복 원칙(`"••••" + 마지막 4자`)을 독립적으로 적용한다
/// (보안 리뷰: FE-BE 라운드트립 시 원문 노출 최소화).
#[tauri::command]
fn get_summary_config() -> serde_json::Value {
    let mut cfg = config::load_summary_config();
    if let Some(key) = &cfg.anthropic_api_key {
        cfg.anthropic_api_key = Some(config::mask_summary_api_key(key));
    }
    if let Some(key) = &cfg.openai_api_key {
        cfg.openai_api_key = Some(config::mask_summary_api_key(key));
    }
    if let Some(key) = &cfg.gemini_api_key {
        cfg.gemini_api_key = Some(config::mask_summary_api_key(key));
    }
    serde_json::to_value(cfg).expect("SummaryConfig 직렬화는 항상 성공")
}

/// 요약 설정 저장. 들어온 3개 키 각각이 마스킹 값(FE가 변경 없이 그대로 되돌려보낸 경우)이면 그
/// 필드의 기존 키를 보존한다(`get_summary_config`와 동일한 원칙, config.rs::resolve_summary_api_key_update
/// 참고) — 필드별로 독립 판정하므로 한 키만 바꿔도 나머지 두 키가 손상되지 않는다.
#[tauri::command]
fn set_summary_config(mut config: SummaryConfig) -> Result<(), String> {
    let previous = config::load_summary_config();
    config.anthropic_api_key =
        config::resolve_summary_api_key_update(previous.anthropic_api_key.as_deref(), config.anthropic_api_key);
    config.openai_api_key =
        config::resolve_summary_api_key_update(previous.openai_api_key.as_deref(), config.openai_api_key);
    config.gemini_api_key =
        config::resolve_summary_api_key_update(previous.gemini_api_key.as_deref(), config.gemini_api_key);
    config::save_summary_config(&config).map_err(|e| e.to_string())
}

/// 요약 엔진 가용성 진단 — "지금 요약을 켜면 실제로 동작하는가"를 FE가 **미리** 보여주기 위한
/// 커맨드다. 온보딩 opt-in 스텝이 이 값으로 "Claude Code CLI 감지됨 · 별도 키 불필요" 같은
/// 안내를 띄운다.
///
/// - 키 **원문은 나가지 않는다** — 보유 여부(bool)만 담는다(`ApiKeysPresent`).
/// - CLI 감지는 후보 경로에 `--version`을 실행하는 로컬 프로세스 호출뿐이라 **네트워크 요청이
///   없다**(ADR-0013의 아웃바운드 정책에 영향 없음).
/// - `resolved`는 `decide_engine`이 고른 결과(`"cli:claude"` 등)이고, 쓸 수 있는 엔진이 하나도
///   없으면 `null`이다. 판정 규칙을 FE에 복제하지 않으려고 BE가 계산해서 내려준다.
#[tauri::command]
async fn detect_summary_engine() -> serde_json::Value {
    let cfg = config::load_summary_config();
    let cli = summary::engine::detect_all_cli().await;
    let api_keys = summary::engine::ApiKeysPresent::from_config(&cfg);
    let resolved =
        summary::engine::decide_engine(cfg.engine, cfg.cli_provider, cfg.api_provider, cli, api_keys)
            .ok()
            .map(|r| r.as_str());
    serde_json::json!({ "cli": cli, "apiKeys": api_keys, "resolved": resolved })
}

/// 캐시된 일일 요약 조회(다이제스트 카드가 열릴 때마다 호출). 없으면 `null`.
#[tauri::command]
fn get_daily_summary(local_date: String, locale: String, db: State<'_, Db>) -> Result<Option<serde_json::Value>, String> {
    let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    summary::get_cached(&conn, &local_date, &locale).map_err(|e| e.to_string())
}

/// 발췌 텍스트만 미리 본다(설정 다이얼로그/요약 카드의 "전송 내용 미리보기" 다이얼로그) — 실제
/// 생성/저장은 하지 않는다.
#[tauri::command]
fn preview_summary_input(local_date: String, tz: String, locale: String, db: State<'_, Db>) -> Result<String, String> {
    let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    summary::excerpt::build_daily_excerpt(&conn, &local_date, &tz, &locale).map_err(|e| e.to_string())
}

/// 일일 요약 생성(또는 캐시 반환). `enabled=false`거나 캡처 일시정지 중이면 아웃바운드 자체를
/// 내지 않고 즉시 `Err`(docs/04-privacy-security.md "일시정지 의미론"과 동일 원칙 — 요약도 커넥터와
/// 같은 아웃바운드 네트워크로 취급). `force=false`이고 캐시가 있으면 그대로 반환(재생성 없음).
/// `excerpt_override`가 있으면(미리보기 에디터에서 사용자가 수정한 발췌) 자동 발췌 대신 그 내용을
/// 쓴다 — 이때도 발송 직전 스크럽·길이 안전컷은 동일하게 적용(`sanitize_excerpt_override`).
#[tauri::command]
async fn generate_daily_summary(
    local_date: String,
    tz: String,
    locale: String,
    force: bool,
    excerpt_override: Option<String>,
    db: State<'_, Db>,
    capture_paused: State<'_, CapturePausedFlag>,
) -> Result<serde_json::Value, String> {
    let cfg = config::load_summary_config();
    // 구분 가능한 실패 상태는 안정 에러 코드로 반환(리뷰 지적) — FE가 i18n 키로 매핑한다.
    // 코드 집합: summary_disabled / summary_paused / summary_no_data(excerpt.rs) /
    // summary_no_engine·summary_api_key_missing·summary_timeout(engine.rs).
    if !cfg.enabled {
        return Err("summary_disabled".to_string());
    }
    if capture_paused.load(Ordering::Relaxed) {
        return Err("summary_paused".to_string());
    }

    if !force {
        let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
        if let Some(cached) = summary::get_cached(&conn, &local_date, &locale).map_err(|e| e.to_string())? {
            return Ok(cached);
        }
    }

    summary::generate_daily_and_cache(db.inner(), &local_date, &tz, &locale, excerpt_override, &cfg)
        .await
        .map_err(|e| e.to_string())
}

// 재개 브리핑 커맨드(M7-②): "어디까지 했지?"에 답하는 **프로젝트 단위** 브리핑. daily/period와 달리
// 캐시가 없다(프로젝트 상태는 계속 바뀌므로 항상 온디맨드 최신 생성) — summary/resume.rs 참고.
// enabled/캡처 일시정지 게이트는 daily/period 커맨드와 동일한 순서로 적용한다(아웃바운드이므로).

/// 재개 브리핑 발췌 텍스트만 미리 본다("전송 내용 미리보기") — 실제 생성/엔진 호출은 하지 않는다.
/// `preview_summary_input`과 동일한 원칙이지만, 이 커맨드는 발췌만으로도 아웃바운드가 아니라서
/// enabled/일시정지 게이트를 걸지 않는다(daily의 `preview_summary_input`과 동일 — 순수 조회).
/// "지금 상태"(git) 블록 조회가 `tokio::process::Command`(로컬 전용, 아웃바운드 아님)를 쓰므로
/// 이 커맨드도 async다(daily의 동기 미리보기와 다른 유일한 지점).
#[tauri::command]
async fn preview_resume_input(
    project: String,
    tz: String,
    locale: String,
    db: State<'_, Db>,
) -> Result<String, String> {
    summary::resume::build_resume_excerpt(db.inner(), &project, &tz, &locale)
        .await
        .map_err(|e| e.to_string())
}

/// 재개 브리핑 생성. `enabled=false`거나 캡처 일시정지 중이면 즉시 `Err`(daily/period와 동일한
/// 아웃바운드 게이트 순서 — 요약 엔진 호출은 항상 이 순서를 먼저 통과해야 한다). 캐시가 없으므로
/// `force`/`excerptOverride` 개념도 없다 — 호출할 때마다 최신 발췌로 새로 생성한다.
#[tauri::command]
async fn generate_resume_briefing(
    project: String,
    tz: String,
    locale: String,
    db: State<'_, Db>,
    capture_paused: State<'_, CapturePausedFlag>,
) -> Result<serde_json::Value, String> {
    let cfg = config::load_summary_config();
    if !cfg.enabled {
        return Err("summary_disabled".to_string());
    }
    if capture_paused.load(Ordering::Relaxed) {
        return Err("summary_paused".to_string());
    }

    summary::resume::generate_resume_briefing(db.inner(), &project, &tz, &locale, &cfg)
        .await
        .map_err(|e| e.to_string())
}

// 주간/월간 AI 요약 롤업 커맨드(M7-③): 원본 이벤트를 재발췌하지 않고 하위 계층 요약(daily/weekly)을
// 합성한다. summary/period.rs 참고. 위 일일 요약 커맨드들과 동일한 enabled/일시정지/캐시 원칙을
// periodType("week" | "month") 축으로 재사용한다.

/// 캐시된 주간/월간 요약 조회. 없으면 `null`.
#[tauri::command]
fn get_period_summary(
    period_type: String,
    period_key: String,
    locale: String,
    db: State<'_, Db>,
) -> Result<Option<serde_json::Value>, String> {
    let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    summary::period::get_cached(&conn, &period_type, &period_key, &locale).map_err(|e| e.to_string())
}

/// 기간 팩트 집계(활동일·세션·프롬프트·GitHub·Linear·Slack 건수) — 요약이 없어도 조회 가능한
/// SQL 직접 집계다(설계 원칙 "숫자는 DB, 서사는 LLM", summary/period.rs::get_period_stats 참고).
#[tauri::command]
fn get_period_stats(
    period_type: String,
    period_key: String,
    tz: String,
    db: State<'_, Db>,
) -> Result<serde_json::Value, String> {
    let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    summary::period::get_period_stats(&conn, &period_type, &period_key, &tz).map_err(|e| e.to_string())
}

/// 기간 합성 발췌 미리보기 — **캐스케이드 생성을 포함**한다(하위 계층 요약이 없으면 그 자리에서
/// 생성·캐시 = 실제 엔진 호출·아웃바운드 발생 가능). "미리보기 = 실제 전송본" 계약
/// (preview_summary_input과 동일 원칙). 따라서 **generate와 동일하게 enabled/캡처 일시정지를
/// 선행 확인**한다 — 이 게이트가 없으면 요약 off/일시정지 상태에서 미리보기만 눌러도 외부 전송이
/// 일어나 일시정지 의미론(docs/04)을 깬다(보안 리뷰 Critical 픽스).
#[tauri::command]
async fn preview_period_summary_input(
    period_type: String,
    period_key: String,
    tz: String,
    locale: String,
    db: State<'_, Db>,
    capture_paused: State<'_, CapturePausedFlag>,
) -> Result<String, String> {
    let cfg = config::load_summary_config();
    if !cfg.enabled {
        return Err("summary_disabled".to_string());
    }
    if capture_paused.load(Ordering::Relaxed) {
        return Err("summary_paused".to_string());
    }
    summary::period::build_excerpt(
        db.inner(),
        &period_type,
        &period_key,
        &tz,
        &locale,
        &cfg,
        summary::period::Cascade::OneLevel,
    )
    .await
    .map_err(|e| e.to_string())
}

/// 주간/월간 요약 생성(또는 캐시 반환). `generate_daily_summary`와 동일한 순서로 enabled/캡처
/// 일시정지를 확인한 뒤, `force=false`이고 캐시가 있으면 그대로 반환한다. `excerpt_override`가
/// 있으면(미리보기 에디터 수정본) 캐스케이드 발췌 대신 그 내용을 쓴다.
/// `cascade=false`면 하위 요약 생성 없이 **캐시된 것만 롤업**한다 — 자동 캐치업(월간)이 콜드
/// 스타트에서 수십 회 엔진 호출로 번지는 것을 막는 경로(리뷰 Critical, period.rs::Cascade 참고).
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn generate_period_summary(
    period_type: String,
    period_key: String,
    tz: String,
    locale: String,
    force: bool,
    excerpt_override: Option<String>,
    cascade: bool,
    db: State<'_, Db>,
    capture_paused: State<'_, CapturePausedFlag>,
) -> Result<serde_json::Value, String> {
    let cfg = config::load_summary_config();
    if !cfg.enabled {
        return Err("summary_disabled".to_string());
    }
    if capture_paused.load(Ordering::Relaxed) {
        return Err("summary_paused".to_string());
    }

    if !force {
        let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
        if let Some(cached) =
            summary::period::get_cached(&conn, &period_type, &period_key, &locale).map_err(|e| e.to_string())?
        {
            return Ok(cached);
        }
    }

    summary::period::generate_and_cache(
        db.inner(),
        &period_type,
        &period_key,
        &tz,
        &locale,
        excerpt_override,
        &cfg,
        if cascade {
            // `force`(사용자가 "다시 생성"을 명시적으로 누른 경우)에만 Refresh — 캐시가 있어도
            // 프롬프트 버전이 낡은 하위 요약을 다시 만든다. 이게 없으면 주간이 개편 전 형식의
            // 일간 캐시를 그대로 이어붙여, 주간만 재생성해도 새 형식이 반영되지 않는다.
            // 평소(force=false) 조회에서는 기존 OneLevel 그대로라 대량 재생성이 일어나지 않는다.
            if force {
                summary::period::Cascade::Refresh
            } else {
                summary::period::Cascade::OneLevel
            }
        } else {
            summary::period::Cascade::CachedOnly
        },
    )
    .await
    .map_err(|e| e.to_string())
}

// 업무평가서 커맨드(ADR-0017): 분기 평가("quarter", "YYYY-Qn")·월간 점검("month", "YYYY-MM").
// summary/review.rs 참고. 생성·미리보기는 요약과 같은 enabled/캡처 일시정지 게이트를 따르고(발췌를
// 만들다 빠진 월간·주간 요약을 생성할 수 있어 미리보기도 아웃바운드가 날 수 있다), 조회·프로젝트
// 목록은 로컬 SQL만 쓰므로 게이트가 없다.

/// 캐시된 평가서 조회. 없으면 `null`.
#[tauri::command]
fn get_performance_review(
    period_type: String,
    period_key: String,
    locale: String,
    db: State<'_, Db>,
) -> Result<Option<serde_json::Value>, String> {
    let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    summary::review::get_cached(&conn, &period_type, &period_key, &locale).map_err(|e| e.to_string())
}

/// 평가 기간에 활동이 있었던 프로젝트 목록(평가서 "포함할 프로젝트" 체크리스트).
#[tauri::command]
fn list_review_projects(
    period_type: String,
    period_key: String,
    tz: String,
    db: State<'_, Db>,
) -> Result<Vec<serde_json::Value>, String> {
    let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    summary::review::list_review_projects(&conn, &period_type, &period_key, &tz).map_err(|e| e.to_string())
}

/// 평가서 발췌 미리보기("미리보기 = 실제 전송본"). 빠진 하위 요약을 생성할 수 있어 generate와 같은
/// 게이트를 먼저 확인한다(`preview_period_summary_input`과 같은 이유).
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn preview_performance_review_input(
    period_type: String,
    period_key: String,
    tz: String,
    locale: String,
    projects: Option<Vec<String>>,
    goals: Option<String>,
    db: State<'_, Db>,
    capture_paused: State<'_, CapturePausedFlag>,
) -> Result<String, String> {
    let cfg = config::load_summary_config();
    if !cfg.enabled {
        return Err("summary_disabled".to_string());
    }
    if capture_paused.load(Ordering::Relaxed) {
        return Err("summary_paused".to_string());
    }
    let inputs = summary::review::ReviewInputs::normalized(projects, goals, config::load_review_profile())
        .map_err(|e| e.to_string())?;
    summary::review::build_excerpt(db.inner(), &period_type, &period_key, &tz, &locale, &inputs, &cfg)
        .await
        .map_err(|e| e.to_string())
}

/// 평가서 생성 — 호출할 때마다 새로 만들어 캐시를 덮어쓴다(자동 생성 경로가 없고 사용자가 버튼을
/// 눌렀을 때만 호출되므로 `force` 구분이 없다). `excerpt_override`는 미리보기 에디터 수정본.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn generate_performance_review(
    period_type: String,
    period_key: String,
    tz: String,
    locale: String,
    projects: Option<Vec<String>>,
    goals: Option<String>,
    excerpt_override: Option<String>,
    db: State<'_, Db>,
    capture_paused: State<'_, CapturePausedFlag>,
) -> Result<serde_json::Value, String> {
    let cfg = config::load_summary_config();
    if !cfg.enabled {
        return Err("summary_disabled".to_string());
    }
    if capture_paused.load(Ordering::Relaxed) {
        return Err("summary_paused".to_string());
    }
    let inputs = summary::review::ReviewInputs::normalized(projects, goals, config::load_review_profile())
        .map_err(|e| e.to_string())?;
    summary::review::generate_and_cache(
        db.inner(),
        &period_type,
        &period_key,
        &tz,
        &locale,
        &inputs,
        excerpt_override,
        &cfg,
    )
    .await
    .map_err(|e| e.to_string())
}

/// 평가 프로필 조회(파일에 없으면 전부 빈 값).
#[tauri::command]
fn get_review_profile() -> ReviewProfile {
    config::load_review_profile()
}

/// 평가 프로필 저장(정규화 후 저장 — 직무 60자, 모르는 연차 값은 미지정).
#[tauri::command]
fn set_review_profile(profile: ReviewProfile) -> Result<(), String> {
    config::save_review_profile(&profile).map_err(|e| e.to_string())
}

// "상세 보기" 온디맨드 요약 커맨드: 기본 요약(daily/period)은 스캔용으로 짧게 유지하고, 사용자가
// "상세 보기"를 눌렀을 때만 같은 발췌를 상세 전용 프롬프트로 재호출한다(summary/mod.rs
// `get_detail_cached`/`generate_detail_and_cache` 참고). scope("day" | "week" | "month")로
// 일간/주간/월간을 한 축으로 다룬다 — 재개(resume) 브리핑은 대상이 아니다(캐시가 없는 별도 모델).

/// 캐시된 상세 요약 조회(다이제스트 카드가 아니라 "상세 보기" 버튼이 열릴 때 호출). 없으면 `null`.
#[tauri::command]
fn get_summary_detail(
    scope: String,
    scope_key: String,
    locale: String,
    db: State<'_, Db>,
) -> Result<Option<serde_json::Value>, String> {
    let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    summary::get_detail_cached(&conn, &scope, &scope_key, &locale).map_err(|e| e.to_string())
}

/// 상세 요약 생성(또는 캐시 반환). `generate_daily_summary`와 동일한 순서로 enabled/캡처 일시정지를
/// 확인한 뒤(요약도 커넥터와 같은 아웃바운드 네트워크로 취급), `force=false`이고 캐시가 있으면 그대로
/// 반환한다(재호출 없음 — `summary::generate_detail_and_cache` 내부에서 처리).
#[tauri::command]
async fn generate_summary_detail(
    scope: String,
    scope_key: String,
    tz: String,
    locale: String,
    force: bool,
    db: State<'_, Db>,
    capture_paused: State<'_, CapturePausedFlag>,
) -> Result<serde_json::Value, String> {
    let cfg = config::load_summary_config();
    if !cfg.enabled {
        return Err("summary_disabled".to_string());
    }
    if capture_paused.load(Ordering::Relaxed) {
        return Err("summary_paused".to_string());
    }

    summary::generate_detail_and_cache(db.inner(), &scope, &scope_key, &tz, &locale, force, &cfg)
        .await
        .map_err(|e| e.to_string())
}

// 캡처 설정 커맨드: config.rs 참고. 파일 경로는 `~/.logroom/config.json`, 적용은 앱 재시작 후.
#[tauri::command]
fn get_capture_config() -> serde_json::Value {
    let mut cfg = config::load_config();
    // slack 토큰 원문은 FE 왕복에 노출하지 않고 마스킹 값(`"••••" + 마지막 4자`)만 돌려준다
    // (보안 리뷰: FE-BE 라운드트립 시 토큰 평문 노출 최소화). `set_capture_config`는 이 마스킹
    // 접두사를 그대로 되돌려받으면 "변경 없음"으로 보아 기존 토큰을 보존한다.
    if let Some(token) = &cfg.slack.token {
        cfg.slack.token = Some(config::mask_slack_token(token));
    }
    // github도 동일한 원칙(보안 리뷰)을 다중 계정 배열에 적용한다 — 각 계정의 토큰을 마스킹한다.
    for account in cfg.github.accounts.iter_mut() {
        account.token = config::mask_github_token(&account.token);
    }
    // linear도 동일 원칙(GitHub와 동일한 다중 계정 배열 왕복 보호).
    for account in cfg.linear.accounts.iter_mut() {
        account.token = config::mask_linear_token(&account.token);
    }
    // notion도 동일 원칙(GitHub/Linear와 동일한 다중 계정 배열 왕복 보호).
    for account in cfg.notion.accounts.iter_mut() {
        account.token = config::mask_notion_token(&account.token);
    }
    // ResolvedRoot가 derive(Serialize)를 직접 구현하므로 필드를 수기로 재조립하지 않는다
    // (필드 드리프트가 나면 컴파일 타임에 잡힌다).
    let resolved_roots = serde_json::to_value(config::resolve_roots_detailed())
        .expect("ResolvedRoot 직렬화는 항상 성공(경로는 lossy 문자열 변환)");
    serde_json::json!({ "config": cfg, "resolvedRoots": resolved_roots })
}

#[tauri::command]
fn set_capture_config(mut config: CaptureConfig) -> Result<(), String> {
    // 폴러가 검증 결과를 되쓰는 것과 겹치지 않도록 기존 설정 읽기~저장을 직렬화한다
    // (`config::update_config` 참고).
    config::update_config(|previous| {
        // 들어온 slack.token이 마스킹 값(`get_capture_config`가 돌려준 값을 FE가 변경 없이 그대로
        // 되돌려보낸 경우)이면 기존 토큰을 보존한다 — 그렇지 않으면 저장할 때마다 마스킹 문자열
        // 자체가 토큰으로 덮어써지는 사고가 난다(보안 리뷰).
        config.slack.token =
            config::resolve_slack_token_update(previous.slack.token.as_deref(), config.slack.token);
        // github도 동일 원칙을 배열로 확장 적용(username·마스킹 값·인덱스 순 매칭 — config.rs 참고).
        config.github.accounts =
            config::resolve_github_accounts_update(&previous.github.accounts, config.github.accounts);
        // linear도 동일 원칙을 배열로 확장 적용(viewerId·마스킹 값·인덱스 순 매칭 — config.rs 참고).
        config.linear.accounts =
            config::resolve_linear_accounts_update(&previous.linear.accounts, config.linear.accounts);
        // notion은 계정 id로 매칭한다(없으면 workspaceId·마스킹 값·인덱스 순 — config.rs 참고).
        config.notion.accounts =
            config::resolve_notion_accounts_update(&previous.notion.accounts, config.notion.accounts);
        *previous = config;
        true
    })
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn restart_app(app: AppHandle) {
    app.restart();
}

// 캡처 헬스 커맨드: 소스별(Claude Code/Kiro CLI) 감시 상태 조회(capture/health.rs 참고).
// 폴링(FE `refetchInterval`) 대상이라 디스크 스캔은 호출 시점에만 수행한다.
#[tauri::command]
fn get_capture_health(db: State<'_, Db>) -> serde_json::Value {
    let conn = db.lock().expect("db mutex poisoned");
    let sources = capture::health::compute_health(&conn).unwrap_or_else(|e| {
        eprintln!("[logroom] get_capture_health: {e}");
        Vec::new()
    });
    serde_json::json!({ "sources": sources })
}

// DB 통계 커맨드(설정 다이얼로그 "데이터" 섹션, query.rs::get_db_stats 참고): 다이얼로그를 열 때만
// 조회하는 읽기 전용 정보라 폴링 대상은 아니다.
#[tauri::command]
fn get_db_stats(db: State<'_, Db>) -> serde_json::Value {
    let conn = db.lock().expect("db mutex poisoned");
    query::get_db_stats(&conn, &db::db_path()).unwrap_or_else(|e| {
        eprintln!("[logroom] get_db_stats: {e}");
        serde_json::json!({
            "dbSizeBytes": 0,
            "walSizeBytes": 0,
            "events": 0,
            "streams": 0,
            "oldestTs": null,
            "newestTs": null,
        })
    })
}

// 데이터 관리 커맨드(설정 다이얼로그 "데이터" 섹션 확장, data_admin.rs 참고): 내보내기/기간
// 삭제/DB 최적화. 모두 사용자가 명시적으로 누르는 동작이라 실패는 FE에 그대로 전달한다
// (list_events_by_day 등 폴링 대상 읽기 커맨드와 달리 빈 값으로 조용히 넘어가지 않는다).

/// 전체 데이터 내보내기: `~/Downloads/logroom-export-<YYYYMMDD-HHMMSS>/`에
/// streams.jsonl + events.jsonl(NDJSON) + logroom.db 사본을 생성한다.
#[tauri::command]
fn export_data(db: State<'_, Db>) -> Result<serde_json::Value, String> {
    let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    let downloads_dir = dirs::download_dir().ok_or_else(|| "Downloads 폴더를 찾을 수 없습니다".to_string())?;
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let export_dir = downloads_dir.join(format!("logroom-export-{timestamp}"));

    data_admin::export_data(&conn, &db::db_path(), &export_dir)
        .map(|summary| {
            serde_json::json!({
                "dir": summary.dir.to_string_lossy(),
                "streams": summary.streams,
                "events": summary.events,
            })
        })
        .map_err(|e| e.to_string())
}

/// 기간 삭제 미리보기: `ts`(epoch ms) 이전 이벤트 수.
#[tauri::command]
fn count_events_before(ts: i64, db: State<'_, Db>) -> Result<i64, String> {
    let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    data_admin::count_events_before(&conn, ts).map_err(|e| e.to_string())
}

/// 기간 삭제 실행: `ts`(epoch ms) 이전 이벤트를 영구 삭제한다(수동 실행 전용).
#[tauri::command]
fn delete_events_before(ts: i64, db: State<'_, Db>) -> Result<serde_json::Value, String> {
    let mut conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    data_admin::delete_events_before(&mut conn, ts)
        .map(|summary| {
            serde_json::json!({
                "deletedEvents": summary.deleted_events,
                "deletedStreams": summary.deleted_streams,
            })
        })
        .map_err(|e| e.to_string())
}

/// DB 최적화(VACUUM + checkpoint). 수십 초 걸릴 수 있다(FE가 안내 문구 표시).
#[tauri::command]
fn vacuum_db(db: State<'_, Db>) -> Result<serde_json::Value, String> {
    let conn = db.lock().map_err(|_| "db mutex poisoned".to_string())?;
    data_admin::vacuum_db(&conn, &db::db_path())
        .map(|summary| {
            serde_json::json!({
                "beforeBytes": summary.before_bytes,
                "afterBytes": summary.after_bytes,
            })
        })
        .map_err(|e| e.to_string())
}

// 캡처 일시정지(M4) 커맨드: 런타임 플래그(AtomicBool)를 즉시 갱신해 watch.rs/ingest.rs 양쪽에
// 재시작 없이 반영하고, config.json에도 영구 반영한다(docs/04-privacy-security.md "일시정지 의미론").
#[tauri::command]
fn get_capture_paused(flag: State<'_, CapturePausedFlag>) -> bool {
    flag.load(Ordering::Relaxed)
}

// 실제 갱신 로직(AtomicBool + config.json + 트레이 체크박스 + FE emit)은 트레이 메뉴 핸들러와
// 공유하는 tray::apply_capture_paused로 추출되어 있다 — 어느 경로로 바꿔도 양쪽이 즉시 동기화된다.
#[tauri::command]
fn set_capture_paused(
    paused: bool,
    app: AppHandle,
    flag: State<'_, CapturePausedFlag>,
    pause_item: State<'_, tray::PauseMenuItem>,
) -> Result<(), String> {
    tray::apply_capture_paused(&app, &flag, &pause_item, paused)
}

// 자동 업데이트(M5, ADR-0013) 커맨드: 체크/설치는 update.rs 참고. 공증 없는 배포라 무결성은
// updater 플러그인의 minisign 서명으로 보장한다(tauri.conf.json의 plugins.updater.pubkey).

/// 수동 업데이트 확인(설정 다이얼로그 "업데이트 확인" 버튼). 이미 최신 버전이면 `null`.
#[tauri::command]
async fn check_update(app: AppHandle) -> Result<Option<update::UpdateInfo>, String> {
    update::check_update(&app).await.map_err(|e| e.to_string())
}

/// 업데이트 다운로드+설치. 진행률은 `update-progress` 이벤트로 emit되고, 설치 후 재시작은 자동으로
/// 하지 않는다 — FE가 사용자 확인을 받은 뒤 기존 `restart_app` 커맨드를 별도로 호출한다.
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    update::install_update(&app).await.map_err(|e| e.to_string())
}

/// 자동 업데이트 백그라운드 체크 사용 여부 조회(설정 다이얼로그 "일반" 섹션 체크박스).
#[tauri::command]
fn get_check_updates() -> bool {
    config::load_check_updates()
}

/// 자동 업데이트 백그라운드 체크 사용 여부 저장. 재시작 없이 다음 체크 주기부터 반영된다.
#[tauri::command]
fn set_check_updates(enabled: bool) -> Result<(), String> {
    config::save_check_updates(enabled).map_err(|e| e.to_string())
}

// 앱 i18n(FE `apps/desktop/src/i18n`) 커맨드: 트레이 메뉴 로케일 저장. FE는 store의 locale이
// "system"이면 실제 표시 언어("en"/"ko")로 미리 해석해 전달한다(App.tsx 동기화 effect 참고) —
// Rust 쪽에는 "system" 개념이 없다. 트레이 메뉴 자체는 `tray::setup`이 부팅 시 1회만 읽으므로
// 적용은 다음 재시작부터다(FE Settings "언어" 섹션 안내 문구).
#[tauri::command]
fn set_app_locale(locale: String) -> Result<(), String> {
    config::save_locale(Some(locale)).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 싱글 인스턴스: 두 번째 실행은 기존 창 표시+포커스(트레이로 숨어있던 경우도 복원).
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tray::show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        // 로그인 시 자동 실행(설정 다이얼로그 "일반" 섹션): 사용자가 명시적으로 켜기 전까지는
        // 등록하지 않는다(opt-in) — 여기서는 플러그인만 등록하고 활성화 자체는 FE의
        // autostart::enable() 호출로만 이뤄진다.
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        // 자동 업데이트(M5, ADR-0013): 체크/설치는 tauri-plugin-updater(update.rs가 감싼 커맨드로만
        // 노출 — FE는 플러그인 JS API를 직접 쓰지 않는다), 설치 후 재시작 경로는 tauri-plugin-process.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // 재개 브리핑(M7-②) "복사" 버튼 — FE가 writeText로 클립보드에 쓴다.
        .plugin(tauri_plugin_clipboard_manager::init())
        // 직전 창 크기·위치 복원(사용자 요구) — 종료 시 저장, 다음 실행 시 복원. 트레이로 숨김/복원과
        // 무관하게 실제 종료(quit) 시점의 크기를 기억한다.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        // 트레이 상주(docs/05-ui-ux.md "앱 표면"): 창을 닫아도 종료하지 않고 숨겨서 캡처를 지속한다.
        // macOS는 숨길 때 Dock에서도 감춰(Accessory) 메뉴바 트레이로만 상주하게 한다.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // 창을 숨기기 전에 현재 크기·위치를 저장한다 — 트레이 상주 앱은 "창 닫기 = 숨김"이라
                // window-state 플러그인의 기본 저장 시점(앱 종료)이 안 잡힌다(실측: ⌘Q·강제종료 모두
                // 미저장). 여기와 트레이 "종료"(tray.rs) 두 곳에서 명시적으로 저장한다.
                use tauri_plugin_window_state::{AppHandleExt, StateFlags};
                let _ = window.app_handle().save_window_state(StateFlags::all());
                api.prevent_close();
                let _ = window.hide();
                #[cfg(target_os = "macos")]
                let _ = window
                    .app_handle()
                    .set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
        })
        .setup(|app| {
            // 1) DB 열기 + 마이그레이션
            let conn = db::open_and_migrate().map_err(|e| e.to_string())?;
            let db: Db = std::sync::Arc::new(std::sync::Mutex::new(conn));

            // 1-1) 읽기 커맨드가 사용할 DB 핸들 등록(쓰기는 인제스트 경로로 직렬화).
            app.manage(db.clone());

            // 1-1-b) 캡처 일시정지(M4) 런타임 플래그: config.json의 capturePaused 초기값으로 시작해
            // watch.rs 감시 스레드 + ingest 서버 + get/set_capture_paused 커맨드가 공유한다
            // (재시작 없이 즉시 적용 — docs/04-privacy-security.md "일시정지 의미론").
            let capture_paused: CapturePausedFlag = Arc::new(AtomicBool::new(config::load_capture_paused()));
            app.manage(capture_paused.clone());

            // 1-1-c) 트레이 아이콘 + 메뉴 등록(docs/05-ui-ux.md "앱 표면"). 반환된 체크박스 핸들은
            // set_capture_paused 커맨드와 공유해 트레이 클릭 ↔ 설정 다이얼로그 양방향 동기화한다.
            let pause_item = tray::setup(app.handle(), capture_paused.clone())?;
            app.manage(pause_item);

            // 1-2) Claude Code JSONL backfill + 상시감시 (docs/03-capture.md 소스1).
            // 감시자는 앱 내부 프로세스라 db::ingest() 를 직접 호출한다(HTTP 인제스트 우회).
            capture::spawn(app.handle().clone(), db.clone(), capture_paused.clone());

            // 1-2-b) Slack 커넥터(v1, docs/08-connectors.md, ADR-0015): enabled+토큰이 있을 때만
            // 백그라운드 폴러를 띄운다(내부에서 자체 판단 후 조용히 종료 가능 — watch.rs와 동일 패턴).
            capture::slack::spawn(app.handle().clone(), db.clone(), capture_paused.clone());

            // 1-2-c) GitHub 커넥터(v1, docs/08-connectors.md): 다중 계정 각각 독립 태스크로 폴링한다
            // (enabled+계정 있을 때만 — 내부에서 자체 판단 후 조용히 종료 가능, slack.rs와 동일 패턴).
            capture::github::spawn(app.handle().clone(), db.clone(), capture_paused.clone());

            // 1-2-d) Linear 커넥터(v1, docs/08-connectors.md): GitHub와 동일하게 계정(워크스페이스)별
            // 독립 태스크로 폴링한다(enabled+계정 있을 때만).
            capture::linear::spawn(app.handle().clone(), db.clone(), capture_paused.clone());

            // 1-2-e) Notion 커넥터(v1, docs/08-connectors.md "Notion v1"): GitHub/Linear와 동일하게
            // 계정(워크스페이스)별 독립 태스크로 폴링한다(enabled+계정 있을 때만). 시간 범위 필터가
            // 없는 Search API 특성상 단일 커서+조기 중단 방식이라 백필/따라잡기 가속은 없다
            // (`capture/notion.rs` 모듈 문서 참고).
            capture::notion::spawn(app.handle().clone(), db.clone(), capture_paused.clone());

            // 2) 인제스트 토큰/포트 준비 (~/.logroom/ingest.json)
            let cfg = ingest::prepare_config().map_err(|e| e.to_string())?;
            let preferred_port = cfg.port;

            // 3) 인제스트 서버를 백그라운드로 실행
            // 시크릿 스크럽(M4) 사용 여부는 요청마다 설정 파일을 다시 읽지 않도록 기동 시 한 번만 로드한다
            // (watch.rs의 scrub_secrets 로딩 방식과 동일 — docs/04-privacy-security.md).
            let state = ingest::AppState {
                db,
                token: cfg.token,
                app: app.handle().clone(),
                scrub_secrets: config::load_scrub_secrets(),
                capture_paused,
            };
            tauri::async_runtime::spawn(async move {
                if let Err(e) = ingest::serve(state, preferred_port).await {
                    eprintln!("[logroom] ingest server error: {e}");
                }
            });

            // 4) 자동 업데이트 백그라운드 체크(M5, ADR-0013): 시작 60초 후 + 24시간 간격, 발견 시
            // update-available emit만(자동 다운로드 없음 — 설치는 항상 사용자 승인 필요).
            update::spawn_periodic_check(app.handle().clone());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_events_by_day,
            list_my_messages_by_day,
            search_events,
            get_stream,
            rename_stream,
            get_digest,
            list_projects,
            get_summary_config,
            set_summary_config,
            detect_summary_engine,
            get_daily_summary,
            preview_summary_input,
            generate_daily_summary,
            preview_resume_input,
            generate_resume_briefing,
            get_period_summary,
            get_period_stats,
            preview_period_summary_input,
            generate_period_summary,
            get_summary_detail,
            generate_summary_detail,
            get_performance_review,
            list_review_projects,
            preview_performance_review_input,
            generate_performance_review,
            get_review_profile,
            set_review_profile,
            get_capture_config,
            set_capture_config,
            restart_app,
            get_capture_health,
            get_capture_paused,
            set_capture_paused,
            get_db_stats,
            export_data,
            count_events_before,
            delete_events_before,
            vacuum_db,
            check_update,
            install_update,
            get_check_updates,
            set_check_updates,
            set_app_locale
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
