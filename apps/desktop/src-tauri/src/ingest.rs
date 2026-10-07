//! 로컬 인제스트 서버 (docs/03-capture.md, docs/04-privacy-security.md).
//! axum on 127.0.0.1:<port>. 토큰/포트는 ~/.logroom/ingest.json (0600).
//! - GET  /v1/health : liveness (토큰 불필요, 민감정보 없음)
//! - POST /v1/ingest : 이벤트 수신 (X-LogRoom-Token 상수시간 검증 필수). 캡처 일시정지(M4) 중이면
//!   저장 없이 `{ok:true, inserted:0, paused:true}`로 응답한다.

use crate::capture::scrub;
use crate::db;
use crate::model::{HealthResponse, IngestRequest};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use rand::Rng;
use rusqlite::Connection;
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use subtle::ConstantTimeEq;
use tauri::{AppHandle, Emitter};
use tokio::net::TcpListener;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub token: String,
    pub app: AppHandle,
    /// 시크릿 스크럽(M4) 사용 여부. 요청마다 설정 파일을 다시 읽지 않도록 서버 기동 시
    /// `config::load_scrub_secrets()`로 한 번만 로드해 보관한다(watch.rs와 동일한 방식).
    pub scrub_secrets: bool,
    /// 캡처 일시정지(M4) 런타임 플래그. `lib.rs`가 watch.rs 감시 스레드와 공유하는 동일 `Arc`라
    /// `set_capture_paused` 커맨드 호출 즉시(서버 재시작 없이) 이 값도 반영된다(docs/04-privacy-security.md).
    pub capture_paused: Arc<AtomicBool>,
}

pub struct IngestConfig {
    pub port: u16,
    pub token: String,
}

fn logroom_dir() -> PathBuf {
    dirs::home_dir().expect("home_dir 없음").join(".logroom")
}

fn ingest_json_path() -> PathBuf {
    logroom_dir().join("ingest.json")
}

fn gen_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 기존 ingest.json 이 있으면 토큰/포트 재사용(캡처러 재설정 불필요),
/// 없으면 새 토큰 + 포트 0(랜덤 발급). 토큰 키체인 보관은 후속(M5).
pub fn prepare_config() -> anyhow::Result<IngestConfig> {
    if let Ok(raw) = fs::read_to_string(ingest_json_path()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let (Some(port), Some(token)) = (
                v.get("port").and_then(|p| p.as_u64()),
                v.get("token").and_then(|t| t.as_str()),
            ) {
                return Ok(IngestConfig {
                    port: port as u16,
                    token: token.to_string(),
                });
            }
        }
    }
    Ok(IngestConfig {
        port: 0,
        token: gen_token(),
    })
}

fn write_ingest_json(port: u16, token: &str) -> anyhow::Result<()> {
    let dir = logroom_dir();
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let path = ingest_json_path();
    fs::write(&path, json!({ "port": port, "token": token }).to_string())?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// 이전 포트 우선 바인딩(사용 중이면 랜덤) → ingest.json 갱신 → axum 서브.
pub async fn serve(state: AppState, preferred_port: u16) -> anyhow::Result<()> {
    let listener = match TcpListener::bind(("127.0.0.1", preferred_port)).await {
        Ok(l) => l,
        Err(_) => TcpListener::bind(("127.0.0.1", 0)).await?,
    };
    let actual_port = listener.local_addr()?.port();
    write_ingest_json(actual_port, &state.token)?;
    eprintln!("[logroom] ingest server on 127.0.0.1:{actual_port}");

    let router = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/ingest", post(ingest))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(8 * 1024 * 1024))
        .with_state(state);

    axum::serve(listener, router).await?;
    Ok(())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

async fn ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<IngestRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let provided = headers
        .get("x-logroom-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if provided.as_bytes().ct_eq(state.token.as_bytes()).unwrap_u8() != 1 {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 캡처 일시정지(M4): 파일감시(watch.rs)와 동일하게 저장 없이 즉시 응답 — 일시정지 중 활동은
    // 영구 미기록(docs/04-privacy-security.md "일시정지 의미론").
    if state.capture_paused.load(Ordering::Relaxed) {
        return Ok(Json(json!({ "ok": true, "inserted": 0, "paused": true })));
    }

    // 파일감시(watch.rs) 경로와 동일하게 ingest 직전 시크릿 스크럽을 적용한다(M4).
    let mut req = req;
    if state.scrub_secrets {
        scrub::scrub_request(&mut req);
    }

    let db = state.db.clone();
    let inserted = tokio::task::spawn_blocking(move || {
        let conn = db.lock().expect("db mutex poisoned");
        db::ingest(&conn, &req)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // 실시간 UI 갱신 신호 (docs/01, 05)
    let _ = state.app.emit("event-ingested", json!({ "inserted": inserted }));
    Ok(Json(json!({ "ok": true, "inserted": inserted })))
}
