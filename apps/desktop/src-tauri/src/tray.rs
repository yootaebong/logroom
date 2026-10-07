//! 메뉴바(트레이) 상주(docs/05-ui-ux.md "앱 표면"): 열기 / 오늘 요약 / 캡처 일시정지 토글 / 종료.
//! 창을 닫아도(`CloseRequested`) 트레이로 상주해 캡처(감시 스레드 + 인제스트 서버)가 계속되도록
//! `lib.rs`가 이 모듈의 [`setup`]/[`show_main_window`]/[`apply_capture_paused`]를 사용한다.
//! 아이콘은 별도 에셋 없이 `app.default_window_icon()`(번들 아이콘)을 재사용한다.

use crate::capture::config;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, MenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

/// `lib.rs`의 캡처 일시정지 런타임 플래그와 동일 타입 — watch.rs 감시 스레드/ingest 서버/이 모듈의
/// 트레이 메뉴 핸들러가 같은 `Arc`를 공유한다(재시작 없이 즉시 반영).
pub type CapturePausedFlag = Arc<AtomicBool>;

/// 트레이 "캡처 일시정지" 체크박스 핸들. FE 커맨드(`set_capture_paused`)와 트레이 메뉴 클릭
/// 핸들러가 `app.manage()`로 공유해 어느 경로로 바꾸든 체크 상태가 양방향으로 동기화된다.
pub type PauseMenuItem = CheckMenuItem<tauri::Wry>;

/// Tauri가 `tauri.conf.json`의 `app.windows[0]`에 별도 `label`을 지정하지 않을 때 부여하는 기본 label.
const MAIN_WINDOW_LABEL: &str = "main";

const MENU_ID_OPEN: &str = "open";
const MENU_ID_DIGEST: &str = "digest";
const MENU_ID_PAUSE: &str = "pause";
const MENU_ID_QUIT: &str = "quit";

/// 트레이 메뉴 라벨(en/ko) — FE `apps/desktop/src/i18n/ui.ts`와 별개로 관리되는 최소 카피(항목
/// 4개뿐이라 공유 인프라 없이 이 모듈 안에 직접 둔다). [`config::load_locale`]이 반환하는
/// `"ko"`일 때만 한국어를 쓰고, 그 외(`None` 포함, 트레이는 항상 en/ko 둘 중 하나)는 영어로 폴백한다.
struct MenuLabels {
    open: &'static str,
    digest: &'static str,
    pause: &'static str,
    quit: &'static str,
}

impl MenuLabels {
    fn for_locale(locale: Option<&str>) -> Self {
        match locale {
            Some("ko") => Self {
                open: "LogRoom 열기",
                digest: "오늘 요약",
                pause: "캡처 일시정지",
                quit: "종료",
            },
            _ => Self {
                open: "Open LogRoom",
                digest: "Today's Digest",
                pause: "Pause Capture",
                quit: "Quit",
            },
        }
    }
}

/// 메인 창을 보이고 포커스한다 — 트레이 "열기"/"오늘 요약", 두 번째 실행 시 포커스 등 공통 진입점.
/// macOS는 창을 숨길 때 Dock에서도 감추므로(Accessory), 다시 열 때 Regular로 복원해야 Dock 아이콘도 돌아온다.
pub fn show_main_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);

    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 캡처 일시정지 상태를 한 곳에서 갱신한다: AtomicBool 갱신 → `config.json` 영구 저장 →
/// 트레이 체크박스 동기화 → FE에 `capture-paused-changed` emit. FE 커맨드(`set_capture_paused`)와
/// 트레이 메뉴 클릭 핸들러가 이 함수를 공유해 어느 경로로 바꿔도 재시작 없이 즉시 반영된다
/// (docs/04-privacy-security.md "일시정지 의미론").
pub fn apply_capture_paused(
    app: &AppHandle,
    flag: &CapturePausedFlag,
    pause_item: &PauseMenuItem,
    paused: bool,
) -> Result<(), String> {
    flag.store(paused, Ordering::Relaxed);

    // 부분 갱신 주의: 기존 설정을 로드해 capturePaused 필드만 바꿔 저장한다
    // (전체를 기본값으로 덮어쓰면 claudeCode/kiroCli 등 다른 설정이 유실된다).
    // 폴러의 검증 결과 저장과 겹치지 않도록 읽기~저장을 직렬화한다(`config::update_config` 참고).
    config::update_config(|cfg| {
        cfg.capture_paused = paused;
        true
    })
    .map_err(|e| e.to_string())?;

    if let Err(e) = pause_item.set_checked(paused) {
        eprintln!("[logroom] tray pause 체크박스 동기화 실패: {e}");
    }
    let _ = app.emit("capture-paused-changed", paused);
    Ok(())
}

/// 트레이 아이콘 + 메뉴(열기/오늘 요약/일시정지/종료)를 구성해 등록한다.
/// `lib.rs` setup에서 DB/캡처 초기화 이후 한 번 호출되며, 반환하는 [`PauseMenuItem`]은
/// 호출부가 `app.manage()`로 등록해 `set_capture_paused` 커맨드와 공유해야 한다.
pub fn setup(app: &AppHandle, capture_paused: CapturePausedFlag) -> tauri::Result<PauseMenuItem> {
    // 로케일은 부팅 시 1회만 로드한다 — FE가 이후 설정을 바꿔도 트레이는 다음 재시작부터
    // 반영된다(FE Settings "언어" 섹션 안내 문구와 합의된 동작, config::save_locale 참고).
    let labels = MenuLabels::for_locale(config::load_locale().as_deref());

    let pause_item = CheckMenuItem::with_id(
        app,
        MENU_ID_PAUSE,
        labels.pause,
        true,
        capture_paused.load(Ordering::Relaxed),
        None::<&str>,
    )?;

    let menu = MenuBuilder::new(app)
        .text(MENU_ID_OPEN, labels.open)
        .text(MENU_ID_DIGEST, labels.digest)
        .separator()
        .item(&pause_item)
        .separator()
        .text(MENU_ID_QUIT, labels.quit)
        .build()?;

    let mut builder = TrayIconBuilder::new().menu(&menu).show_menu_on_left_click(true);
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    let pause_item_for_handler = pause_item.clone();
    builder
        .on_menu_event(move |app, event| match event.id().as_ref() {
            MENU_ID_OPEN => show_main_window(app),
            MENU_ID_DIGEST => {
                show_main_window(app);
                let _ = app.emit("open-digest", ());
            }
            MENU_ID_PAUSE => {
                let next = !capture_paused.load(Ordering::Relaxed);
                if let Err(e) = apply_capture_paused(app, &capture_paused, &pause_item_for_handler, next) {
                    eprintln!("[logroom] 트레이 일시정지 토글 실패: {e}");
                }
            }
            MENU_ID_QUIT => {
                // 종료 직전 창 크기·위치 저장(트레이 상주라 창을 숨긴 채 종료해도 마지막 크기를
                // 기억 — lib.rs CloseRequested와 같은 이유).
                use tauri_plugin_window_state::{AppHandleExt, StateFlags};
                let _ = app.save_window_state(StateFlags::all());
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(pause_item)
}
