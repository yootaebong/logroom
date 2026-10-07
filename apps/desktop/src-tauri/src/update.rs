//! 자동 업데이트(M5, ADR-0013) — 수동/백그라운드 업데이트 확인 + 다운로드·설치.
//!
//! 공증 없는 배포 전략(ad-hoc 서명)이라 무결성은 Tauri updater의 minisign 서명으로 보장한다
//! (`tauri.conf.json`의 `plugins.updater.pubkey`). 체크는 항상 **버전 요청만**(식별자 없음)이고,
//! 설치는 항상 사용자가 명시적으로 눌러야 진행된다(백그라운드 체크는 발견 알림만, 자동 다운로드 금지).
//!
//! `lib.rs`의 `check_update`/`install_update` 커맨드가 이 모듈의 [`check_update`]/[`install_update`]를
//! 그대로 호출하고, 앱 시작 시 [`spawn_periodic_check`]로 백그라운드 주기 체크를 등록한다.

use crate::capture::config;
use serde::Serialize;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

/// 앱 시작 후 최초 백그라운드 체크까지 대기 시간(docs/07-decisions.md ADR-0013). 최초 실행 시
/// 구버전이면 곧바로 업데이트 모달을 띄우기 위해 짧게 잡는다(과거 60초 → 3초 — release 산출물이
/// 구버전이어도 사용자가 바로 알 수 있게. 설치 자체는 여전히 모달의 사용자 클릭으로만 진행).
const INITIAL_CHECK_DELAY: Duration = Duration::from_secs(3);
/// 이후 반복 백그라운드 체크 간격(24시간).
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 새 버전 발견 시 FE로 emit되는 이벤트명(수동/백그라운드 공통). 자동 다운로드는 하지 않고 알림만 보낸다.
const EVENT_UPDATE_AVAILABLE: &str = "update-available";
/// 다운로드 진행 시 FE로 emit되는 이벤트명(`install_update` 진행 중에만 발생).
const EVENT_UPDATE_PROGRESS: &str = "update-progress";

/// `check_update`/백그라운드 체크가 공유하는 결과 shape(FE 계약 camelCase).
/// 이미 최신 버전이면 이 값 자체가 없다(`Option::None` → FE에는 `null`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// 항상 `true`(값이 존재하는 것 자체가 "업데이트 있음"을 의미) — FE 타입 판별을 명시적으로 만들기 위한 필드.
    pub available: bool,
    pub version: String,
    pub notes: Option<String>,
    /// epoch ms. 서버 응답에 `pub_date`가 없으면 `null`.
    pub date: Option<i64>,
}

/// `install_update` 진행 중 emit되는 다운로드 진행률(다운로드된 누적 바이트 + 전체 바이트, 알 수 없으면 `null`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProgress {
    downloaded: u64,
    total: Option<u64>,
}

fn to_update_info(update: &tauri_plugin_updater::Update) -> UpdateInfo {
    UpdateInfo {
        available: true,
        version: update.version.clone(),
        notes: update.body.clone(),
        date: update.date.map(|d| d.unix_timestamp() * 1000),
    }
}

/// updater 플러그인으로 최신 버전을 조회한다(버전 요청만 — 사용자 식별자 없음, ADR-0013).
/// 이미 최신 버전이면 `Ok(None)`.
async fn fetch_update(app: &AppHandle) -> anyhow::Result<Option<tauri_plugin_updater::Update>> {
    Ok(app.updater()?.check().await?)
}

/// `check_update` 커맨드 본체 — 수동 확인(설정 다이얼로그 "업데이트 확인" 버튼) + 백그라운드 체크 공용.
pub async fn check_update(app: &AppHandle) -> anyhow::Result<Option<UpdateInfo>> {
    let update = fetch_update(app).await?;
    Ok(update.as_ref().map(to_update_info))
}

/// `install_update` 커맨드 본체: 다운로드+설치를 진행하며 진행률을 `update-progress`로 emit한다.
/// 설치 완료 후 재시작은 이 함수의 책임이 아니다 — FE가 사용자 확인을 받은 뒤 기존 `restart_app`
/// 커맨드를 별도로 호출한다(자동 재시작 없음).
pub async fn install_update(app: &AppHandle) -> anyhow::Result<()> {
    let Some(update) = fetch_update(app).await? else {
        anyhow::bail!("설치할 업데이트가 없습니다");
    };

    let mut downloaded: u64 = 0;
    update
        .download_and_install(
            |chunk_length, total| {
                downloaded += chunk_length as u64;
                let _ = app.emit(EVENT_UPDATE_PROGRESS, UpdateProgress { downloaded, total });
            },
            || {},
        )
        .await?;
    Ok(())
}

/// 앱 시작 60초 후 + 이후 24시간 간격으로 백그라운드에서 업데이트를 확인한다(ADR-0013 자동 체크 정책).
/// 매 주기마다 `~/.logroom/config.json`의 `checkUpdates`(기본 true)를 다시 읽으므로, 설정을 바꿔도
/// 재시작 없이 다음 주기부터 반영된다. 새 버전 발견 시 자동 다운로드 없이 `update-available`만 emit한다.
pub fn spawn_periodic_check(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(INITIAL_CHECK_DELAY).await;
        loop {
            if config::load_check_updates() {
                match check_update(&app).await {
                    Ok(Some(info)) => {
                        let _ = app.emit(EVENT_UPDATE_AVAILABLE, info);
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("[logroom] 백그라운드 업데이트 체크 실패: {e}"),
                }
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}
