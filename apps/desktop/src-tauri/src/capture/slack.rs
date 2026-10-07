//! Slack 커넥터(v1) — 로컬 폴링 + 수동 user token(docs/08-connectors.md, ADR-0015).
//! 파일감시(watch.rs)와 달리 root/파일 개념이 없다: `auth.test`로 신원 확인 → 주기적으로
//! `search.messages`(`from:<@내 id>`)를 호출해 새 메시지만 가져와 기존 파이프라인(스크럽 →
//! 캡처 일시정지 체크 → `db::ingest()`)에 그대로 태운다. DM 스트림 title은 상대방 user id를
//! `users.info`로 1회(폴러 수명 캐시) 해석해 사람이 읽을 수 있는 이름으로 표시한다.
//!
//! ## 이중 커서 — 최신 우선(desc) 백필
//! 설정 직후 최근 데이터부터 보이도록, 과거 히스토리 백필은 asc(과거→현재)가 아니라
//! **desc(현재→과거)**로 진행한다. `capture_cursors`(source=`"slack"`)에 두 커서를 둔다:
//! - `search:newest`([`CURSOR_RESOURCE_NEWEST`]): 전방 증분 — 지금까지 본 **최신** msg ts(ms).
//!   매 사이클 `after:<newest 날짜>`로 새 메시지를 전부(페이지네이션 asc — 순서보다 누락 없음이
//!   중요) 가져와 전진시킨다.
//! - `search:backfill`([`CURSOR_RESOURCE_BACKFILL`]): 후방 백필 — 지금까지 내려간 **가장 오래된**
//!   msg ts(ms). 사이클마다 `before:<backfill 날짜>` + `sort_dir=desc`로 한 배치
//!   ([`MAX_PAGES_PER_CYCLE`] 상한)만큼 전진시키고, 배치 결과가 0건이면 바닥(더 이상 과거 없음)에
//!   닿았다고 보아 완료 마킹한다 — `mtime` 컬럼을 "백필 완료" 플래그로 재사용한다(`Some(_)`=완료,
//!   `None`=진행 중, 새 컬럼/테이블 추가 없음).
//! - **첫 실행**(두 커서 모두 없음, [`is_bootstrap`]): desc 최신 배치 1회로 newest=배치 최댓값·
//!   backfill=배치 최솟값을 동시에 초기화한다([`init_cursors_from_bootstrap_batch`]).
//! - **기존 사용자 마이그레이션**([`resolve_cursor_state`]): v1의 구 단일 커서
//!   (`resource="search"`, [`LEGACY_CURSOR_RESOURCE`])는 asc로 진행하던 중이던 과거 어느 중간
//!   지점이라 "최신"으로 승계할 수 없다. 발견 즉시 삭제하고 첫 실행과 동일하게 desc 최신 배치부터
//!   다시 시작한다 — 이미 저장돼 있던 과거 메시지들과 backfill이 내려가며 만나는 구간은
//!   `(source, external_id)` UNIQUE 제약으로 자연히 중복 스킵되어 메꿔진다(데이터 유실 없음).
//! - **따라잡기 가속**(기존 [`CATCHUP_INTERVAL_SECS`])은 backfill이 미완료인 동안 계속 적용되고,
//!   완료된 뒤에는 `pollMinutes` 간격의 증분만 남는다(`spawn`의 폴링 루프 참고).
//! - Slack `before:`/`after:`는 day 단위 해상도라 자정 경계에서 값이 애매해질 수 있다 — 경계
//!   중복은 idempotent라 무해하지만 **누락**은 없어야 하므로 `after`는 커서 날짜의 하루 **전**
//!   ([`after_date_for_cursor`]), `before`는 커서 날짜의 하루 **뒤**([`before_date_for_backfill`])로
//!   여유를 둔다.
//!
//! 순수함수(정규화·쿼리 문자열 조립·백오프 계산·상한 판정·커서 전진/마이그레이션 판정)는
//! 네트워크/DB 없이 단위테스트한다. 네트워크 호출·폴링 루프는 `run`/`spawn`/`poll_once`에 있으며
//! 이 파일의 테스트 대상이 아니다(수동 확인 필요 — 완료 보고 참고).

use crate::capture::config;
use crate::capture::scrub;
use crate::db;
use crate::model::{EventInput, IngestRequest, StreamInput};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const SOURCE: &str = "slack";
/// v1의 구 단일 커서 `resource` 값. 이중 커서 도입 전 사용자의 마이그레이션 판정에만 쓰인다 —
/// 발견 즉시 삭제하고 값은 승계하지 않는다(위 모듈 문서 "이중 커서" 참고).
const LEGACY_CURSOR_RESOURCE: &str = "search";
/// 전방 증분 커서(`capture_cursors.resource`): 지금까지 본 **최신** msg ts(ms).
const CURSOR_RESOURCE_NEWEST: &str = "search:newest";
/// 후방 백필 커서(`capture_cursors.resource`): 지금까지 내려간 **가장 오래된** msg ts(ms).
/// `mtime` 컬럼을 "백필 완료" 플래그로 겸용한다(`Some(_)`=완료, `None`=진행 중).
const CURSOR_RESOURCE_BACKFILL: &str = "search:backfill";

/// 한 번의 `search.messages` 호출에서 가져올 결과 수(Slack 기본/최대값 사이 보수적인 값).
const SEARCH_PAGE_SIZE: u32 = 100;
/// 폭주 방지용 페이지 상한(호출 1건당). Slack Tier 2 레이트리밋을 고려해 보수적으로 제한한다.
/// 전방 증분과 후방 백필은 각각 별도의 `search.messages` 호출이라 한 사이클에 최대 이 값의 2배
/// 페이지가 소비될 수 있다. 실측: 이 상한 때문에 백필 한 배치는 최대
/// `MAX_PAGES_PER_CYCLE * SEARCH_PAGE_SIZE`(1,000)건만 내려가, 밀린 이력이 많으면 여러 사이클에
/// 나눠 진행한다 — backfill이 끝날 때까지는 평소 간격 대신 [`CATCHUP_INTERVAL_SECS`]로 다음
/// 사이클을 즉시 돌려 보완한다.
const MAX_PAGES_PER_CYCLE: u32 = 10;
/// 429 연속 실패 시 백오프 상한(초) — docs/08-connectors.md "Rate limit 전략"(최대 5분).
const MAX_BACKOFF_SECS: u64 = 5 * 60;
/// 429가 `Retry-After` 헤더를 안 주는 드문 경우의 초기 백오프(초).
const INITIAL_BACKOFF_SECS: u64 = 5;
/// 한 호출 내 페이지 요청 사이 최소 간격(초). 백필 가속(연속 사이클) 모드에서도 유지해 Slack
/// Tier 2 레이트리밋을 존중한다(docs/08-connectors.md "Rate limit 전략").
const PAGE_REQUEST_INTERVAL_SECS: u64 = 1;
/// backfill이 미완료인 동안 다음 사이클까지 대기하는 짧은 간격(초). 평소 간격(`pollMinutes`)보다
/// 훨씬 짧게 잡아 밀린 과거 이력을 빠르게 따라잡는다. backfill 완료 후에는 쓰이지 않는다.
const CATCHUP_INTERVAL_SECS: u64 = 10;

type Db = Arc<Mutex<rusqlite::Connection>>;

/// `slack:<channel_id>` 형태의 stream id.
pub fn stream_id(channel_id: &str) -> String {
    format!("{SOURCE}:{channel_id}")
}

/// Slack ts(`"1730000000.123456"`, 초.마이크로초 문자열) → epoch ms.
/// 소수부가 없거나(정수만) 파싱 실패해도 초 단위만으로 최대한 값을 산출한다. 완전히 파싱 불가하면
/// `None`(호출부가 해당 메시지를 skip).
pub fn parse_slack_ts(ts: &str) -> Option<i64> {
    let mut parts = ts.splitn(2, '.');
    let secs: i64 = parts.next()?.parse().ok()?;
    let micros: i64 = match parts.next() {
        Some(frac) if !frac.is_empty() => {
            // 소수부를 6자리(마이크로초)로 맞춘 뒤 밀리초로 변환(자릿수가 달라도 안전하게 처리).
            let padded: String = frac.chars().chain(std::iter::repeat('0')).take(6).collect();
            padded.parse().unwrap_or(0)
        }
        _ => 0,
    };
    Some(secs * 1000 + micros / 1000)
}

/// 채널 객체가 1:1 다이렉트 메시지(DM)인지 판별한다. `is_im` 필드를 우선 신뢰하고, 없으면 채널
/// id가 `D`로 시작하는지로 폴백 판별한다(그룹 DM은 `is_mpim`이며 id가 `G`로 시작해 여기 걸리지
/// 않는다 — docs/08-connectors.md "매핑").
fn is_dm_channel(channel: &Value) -> bool {
    match channel.get("is_im").and_then(Value::as_bool) {
        Some(is_im) => is_im,
        None => channel
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with('D')),
    }
}

/// DM 채널에서 상대방 user id 후보를 뽑는다. Slack `search.messages`가 돌려주는 최소 channel
/// 객체는 DM일 때 `name` 필드에 채널명 대신 상대방 user id를 담는다(실측 — 원래 문서 가정과
/// 달리 이름이 비어있지 않다). `user` 필드가 있으면 그쪽을 우선한다(더 명시적인 필드가 향후
/// 추가되는 경우를 대비).
fn dm_counterpart_user_id(channel: &Value) -> Option<&str> {
    channel
        .get("user")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| channel.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()))
}

/// 채널 객체(`search.messages` 매치의 `channel` 필드)에서 스트림 title을 산출한다.
/// `#name`(공개/비공개 채널·그룹 DM), `DM: <해석된 이름>`(1:1 다이렉트 메시지 — `dm_names`에
/// user id로 해석된 이름이 있으면 사용, 없으면 `DM: <user id>`로 폴백), 또는 채널 id/`DM`
/// (그마저 없는 경우) — docs/08-connectors.md "매핑" 참고.
fn channel_title(channel: &Value, dm_names: &HashMap<String, String>) -> String {
    if is_dm_channel(channel) {
        return match dm_counterpart_user_id(channel) {
            Some(user_id) => {
                let display = dm_names.get(user_id).map(String::as_str).unwrap_or(user_id);
                format!("DM: {display}")
            }
            None => "DM".to_string(),
        };
    }
    let name = channel
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty());
    if let Some(name) = name {
        return format!("#{name}");
    }
    channel
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(SOURCE)
        .to_string()
}

/// `search.messages` 매치 배열에서 DM 채널의 상대방 user id를 중복 없이 뽑는다(`poll_once`가
/// `users.info` 조회 대상을 결정하는 데 사용). 그룹 DM/일반 채널은 대상에서 제외된다.
pub fn dm_counterpart_ids(messages: &[Value]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    for msg in messages {
        let Some(channel) = msg.get("channel") else { continue };
        if !is_dm_channel(channel) {
            continue;
        }
        if let Some(user_id) = dm_counterpart_user_id(channel) {
            seen.insert(user_id.to_string());
        }
    }
    seen.into_iter().collect()
}

/// `search.messages` 매치 1건(JSON) → `EventInput`. 채널/ts/텍스트 중 필수 필드가 없으면 `None`
/// (방어적 스킵 — 원본 스키마가 실측 전 문서 근거로만 작성됐으므로 예상 밖 shape도 크래시 없이 넘어간다).
fn normalize_message(msg: &Value) -> Option<EventInput> {
    let channel = msg.get("channel")?;
    let channel_id = channel.get("id").and_then(Value::as_str)?;
    let ts_str = msg.get("ts").and_then(Value::as_str)?;
    let ts = parse_slack_ts(ts_str)?;
    let text = msg.get("text").and_then(Value::as_str).unwrap_or_default();
    let url = msg.get("permalink").and_then(Value::as_str).map(str::to_string);

    Some(EventInput {
        external_id: format!("{channel_id}:{ts_str}"),
        stream_id: stream_id(channel_id),
        ts,
        source: SOURCE.to_string(),
        event_type: "message".to_string(),
        title: None,
        body: Some(text.to_string()),
        model: None,
        tokens_in: None,
        tokens_out: None,
        url,
        parent_id: None,
        metadata: None,
    })
}

/// `search.messages`의 `matches[]`(JSON 배열) → 채널(스트림)별로 묶인 `IngestRequest` 목록.
/// `db::ingest()`가 요청 1건당 하나의 `stream` upsert를 전제하므로(`capture/normalize.rs`/`kiro.rs`와
/// 동일 계약), 한 응답에 여러 채널의 메시지가 섞여 있어도 채널당 1건으로 분리한다.
/// 이벤트는 채널 내 ts 오름차순으로 정렬한다(전방 증분과 후방 백필 결과가 섞여 들어와도 순서를
/// 다시 맞춘다). `workspace`는 스트림의 `project` 필드로 쓰인다(Slack 워크스페이스명, `auth.test`
/// 응답의 `team`). `dm_names`는 DM 채널 title 산출에 쓰이는 user id → 표시 이름 맵(`poll_once`가
/// `users.info` 조회 결과로 미리 채워 전달 — 이 함수 자체는 네트워크를 타지 않는 순수함수로 유지한다).
pub fn normalize_search_results(
    messages: &[Value],
    workspace: &str,
    dm_names: &HashMap<String, String>,
) -> Vec<IngestRequest> {
    let mut groups: BTreeMap<String, (Value, Vec<EventInput>)> = BTreeMap::new();

    for msg in messages {
        let Some(event) = normalize_message(msg) else { continue };
        let Some(channel) = msg.get("channel") else { continue };
        let Some(channel_id) = channel.get("id").and_then(Value::as_str) else { continue };
        groups
            .entry(channel_id.to_string())
            .or_insert_with(|| (channel.clone(), Vec::new()))
            .1
            .push(event);
    }

    groups
        .into_iter()
        .map(|(channel_id, (channel, mut events))| {
            events.sort_by_key(|e| e.ts);
            let stream = StreamInput {
                id: stream_id(&channel_id),
                source: SOURCE.to_string(),
                kind: Some("session".to_string()),
                title: Some(channel_title(&channel, dm_names)),
                project: Some(workspace.to_string()),
                git_branch: None,
                started_at: None,
                ended_at: None,
                status: None,
                metadata: None,
            };
            IngestRequest {
                stream: Some(stream),
                events,
            }
        })
        .collect()
}

/// 정규화된 요청들 전체에서 가장 최근 이벤트 ts(ms). 전방 증분(`search:newest`) 커서 전진에
/// 사용한다(없으면 `None`).
pub fn max_event_ts(requests: &[IngestRequest]) -> Option<i64> {
    requests
        .iter()
        .flat_map(|r| r.events.iter())
        .map(|e| e.ts)
        .max()
}

/// 정규화된 요청들 전체에서 가장 오래된 이벤트 ts(ms). 후방 백필(`search:backfill`) 커서 전진에
/// 사용한다(없으면 `None` — 배치가 비어있었다는 뜻, [`advance_backfill`] 참고).
pub fn min_event_ts(requests: &[IngestRequest]) -> Option<i64> {
    requests
        .iter()
        .flat_map(|r| r.events.iter())
        .map(|e| e.ts)
        .min()
}

fn ms_to_utc_date(ms: i64) -> NaiveDate {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.date_naive())
        .unwrap_or_else(|| Utc::now().date_naive())
}

/// Slack `search.messages` 전방 증분 쿼리의 `after:<YYYY-MM-DD>` 값. newest 커서(지금까지 본 최신
/// msg ts)가 있으면 그 날짜의 **하루 전**(day 단위 해상도로 인한 자정 경계 오차를 흡수하는 안전
/// 마진 — 정확한 컷은 ts 비교로 별도 수행하고, 재조회된 메시지는 `(source, external_id)` UNIQUE로
/// idempotent), 없으면(첫 실행 이전 상태) 오늘 날짜.
pub fn after_date_for_cursor(cursor_ts_ms: Option<i64>, today: NaiveDate) -> NaiveDate {
    match cursor_ts_ms {
        Some(ms) => ms_to_utc_date(ms) - ChronoDuration::days(1),
        None => today,
    }
}

/// Slack `search.messages` 후방 백필 쿼리의 `before:<YYYY-MM-DD>` 값. day 단위 해상도로 인한 자정
/// 경계 **누락**을 막기 위해 backfill 커서(지금까지 내려간 가장 오래된 msg ts) 날짜의 **하루 뒤**로
/// 여유를 둔다(경계 중복은 idempotent라 무해 — `after_date_for_cursor`와 반대 방향의 마진).
/// backfill 커서가 아직 없으면(부트스트랩의 첫 배치) 상한을 두지 않는다(`None` — 최신 메시지부터).
pub fn before_date_for_backfill(backfill_ts_ms: Option<i64>) -> Option<NaiveDate> {
    backfill_ts_ms.map(|ms| ms_to_utc_date(ms) + ChronoDuration::days(1))
}

/// `search.messages` 전방 증분 쿼리 문자열 조립: `from:<@user_id> after:<date>`.
pub fn build_search_query(user_id: &str, after_date: NaiveDate) -> String {
    format!("from:<@{user_id}> after:{}", after_date.format("%Y-%m-%d"))
}

/// `search.messages` 후방 백필 쿼리 문자열 조립: `from:<@user_id>`(+ `before:<date>`, 있으면).
/// `before_date`가 `None`이면(부트스트랩의 첫 배치) 상한 없이 최신 메시지부터 desc로 가져온다.
pub fn build_backfill_query(user_id: &str, before_date: Option<NaiveDate>) -> String {
    match before_date {
        Some(date) => format!("from:<@{user_id}> before:{}", date.format("%Y-%m-%d")),
        None => format!("from:<@{user_id}>"),
    }
}

/// 429 연속 실패 시 다음 백오프 대기시간(초). `retry_after`(Slack `Retry-After` 헤더)가 있으면
/// 그 값과 현재 지수 백오프 중 큰 쪽을 따르고, 없으면 이전 대기시간의 2배(최초는
/// [`INITIAL_BACKOFF_SECS`])를 쓴다. 상한은 [`MAX_BACKOFF_SECS`](docs/08-connectors.md "Rate limit 전략").
pub fn next_backoff_secs(previous_secs: u64, retry_after: Option<u64>) -> u64 {
    let exponential = if previous_secs == 0 {
        INITIAL_BACKOFF_SECS
    } else {
        previous_secs.saturating_mul(2)
    };
    let candidate = match retry_after {
        Some(retry_after) => exponential.max(retry_after),
        None => exponential,
    };
    candidate.min(MAX_BACKOFF_SECS)
}

/// 이번 배치 호출이 자체 페이지 상한([`MAX_PAGES_PER_CYCLE`])에 걸려 조기 종료됐는지 판정한다.
/// `last_page`는 이번 호출에서 마지막으로 가져온 페이지 번호, `total_pages`는 Slack 응답이 알려준
/// 전체 페이지 수. 상한에 걸렸다면(= `last_page`가 상한에 도달했는데 아직 `total_pages`에는 못
/// 미침 = 다음 페이지가 더 있음) 이번 호출이 전체를 다 가져오지 못했다는 뜻이다(로그성 정보 —
/// 사이클 간격은 이 값이 아니라 backfill 완료 여부로 결정된다, [`poll_once`] 참고).
pub fn hit_cycle_page_cap(last_page: u32, total_pages: u32) -> bool {
    last_page >= MAX_PAGES_PER_CYCLE && last_page < total_pages
}

/// 이중 커서 상태 — `capture_cursors`에서 읽은 값을 표현하는 순수 구조체([`resolve_cursor_state`]
/// 참고).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    /// 지금까지 본 **최신** msg ts(ms). `None`이면 아직 전방 증분이 시작되지 않음(첫 실행/마이그레이션
    /// 직후).
    pub newest_ts: Option<i64>,
    /// 지금까지 내려간 **가장 오래된** msg ts(ms). `None`이면 백필이 아직 시작되지 않음.
    pub backfill_ts: Option<i64>,
    /// 백필이 바닥(더 이상 과거 메시지 없음)에 도달해 완료됐는지.
    pub backfill_done: bool,
}

/// `capture_cursors`에서 읽은 원값들로부터 이번 사이클에 쓸 [`CursorState`]를 계산한다.
/// `legacy_exists`(v1의 구 단일 커서, [`LEGACY_CURSOR_RESOURCE`]) 존재 여부가 핵심 분기다 — 구
/// 커서는 asc로 진행하던 중이던 과거 어느 중간 지점이라 값 자체를 승계할 수 없다. 존재한다면 새
/// 커서 값이 우연히 남아있더라도 무시하고 **첫 실행과 동일한 상태**(둘 다 `None`, `backfill_done
/// = false`)로 되돌려 desc 최신 배치부터 다시 시작하게 한다(모듈 문서 "기존 사용자 마이그레이션"
/// 참고 — 구 커서 행 삭제 자체는 부수효과라 `poll_once`가 수행한다).
pub fn resolve_cursor_state(
    legacy_exists: bool,
    newest_ts: Option<i64>,
    backfill_ts: Option<i64>,
    backfill_done: bool,
) -> CursorState {
    if legacy_exists {
        CursorState {
            newest_ts: None,
            backfill_ts: None,
            backfill_done: false,
        }
    } else {
        CursorState {
            newest_ts,
            backfill_ts,
            backfill_done,
        }
    }
}

/// 두 커서가 모두 없는 "첫 실행"(또는 마이그레이션 직후) 상태인지 판정한다 — 참이면 `poll_once`가
/// desc 최신 배치 1회로 [`init_cursors_from_bootstrap_batch`]를 적용한다.
pub fn is_bootstrap(state: &CursorState) -> bool {
    state.newest_ts.is_none() && state.backfill_ts.is_none()
}

/// 첫 실행(또는 마이그레이션 직후, [`is_bootstrap`]이 참) 배치 결과로부터 두 커서의 초기값을
/// 계산한다: `(newest, backfill, backfill_done)`. desc 최신 배치의 최대/최소 이벤트 ts([`max_event_ts`]/
/// [`min_event_ts`])를 각각 newest/backfill로 삼는다. 배치가 비어있으면(이 계정으로 보낸 메시지가
/// 전혀 없음) 두 커서 모두 초기화하지 않고 backfill만 완료 처리한다 — 다음 사이클부터 정상 증분
/// 경로로 진입한다(`after_date_for_cursor(None, ..)`가 오늘 날짜를 쓴다).
pub fn init_cursors_from_bootstrap_batch(
    batch_min_ts: Option<i64>,
    batch_max_ts: Option<i64>,
) -> (Option<i64>, Option<i64>, bool) {
    match (batch_min_ts, batch_max_ts) {
        (Some(min_ts), Some(max_ts)) => (Some(max_ts), Some(min_ts), false),
        _ => (None, None, true),
    }
}

/// 전방 증분(`search:newest`) 커서 전진: 이번 사이클에 새로 발견된 메시지의 최대 ts가 있으면 그
/// 값으로, 없으면(신규 메시지 없음) 이전 값을 그대로 유지한다.
pub fn advance_newest(previous_newest_ts: Option<i64>, batch_max_ts: Option<i64>) -> Option<i64> {
    batch_max_ts.or(previous_newest_ts)
}

/// 후방 백필(`search:backfill`) 커서 전진: `(새 backfill_ts, backfill_done)`. 배치의 최소 이벤트
/// ts([`min_event_ts`])가 있으면 그 값으로 전진시키고 미완료 상태를 유지한다. **배치가 비어있으면
/// 바닥(더 이상 과거 메시지 없음)에 도달했다는 뜻**이라 완료 처리하고(재시작해도 다시 돌지 않음),
/// 커서 값은 이전 값을 그대로 둔다(마지막으로 도달한 바닥 시각으로서의 의미 유지).
pub fn advance_backfill(previous_backfill_ts: Option<i64>, batch_min_ts: Option<i64>) -> (Option<i64>, bool) {
    match batch_min_ts {
        Some(min_ts) => (Some(min_ts), false),
        None => (previous_backfill_ts, true),
    }
}

/// `auth.test` 응답에서 필요한 필드만 뽑은 결과.
struct AuthIdentity {
    user_id: String,
    team: String,
}

fn extract_ok(body: &Value) -> bool {
    body.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

fn extract_error(body: &Value) -> &str {
    body.get("error").and_then(Value::as_str).unwrap_or("unknown_error")
}

async fn auth_test(client: &reqwest::Client, token: &str) -> anyhow::Result<AuthIdentity> {
    let resp = client
        .post("https://slack.com/api/auth.test")
        .bearer_auth(token)
        .send()
        .await?;
    let body: Value = resp.json().await?;
    if !extract_ok(&body) {
        anyhow::bail!("auth.test 실패: {}", extract_error(&body));
    }
    let user_id = body
        .get("user_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("auth.test 응답에 user_id 없음"))?
        .to_string();
    let team = body
        .get("team")
        .and_then(Value::as_str)
        .unwrap_or("slack")
        .to_string();
    Ok(AuthIdentity { user_id, team })
}

/// `users.info`(스코프 `users:read`, docs/08-connectors.md)를 호출해 표시 이름을 얻는다.
/// `display_name`을 우선하고 비어있으면 `real_name`으로 폴백한다(둘 다 없으면 에러).
async fn fetch_user_name(client: &reqwest::Client, token: &str, user_id: &str) -> anyhow::Result<String> {
    let resp = client
        .get("https://slack.com/api/users.info")
        .bearer_auth(token)
        .query(&[("user", user_id)])
        .send()
        .await?;
    let body: Value = resp.json().await?;
    if !extract_ok(&body) {
        anyhow::bail!("users.info 실패: {}", extract_error(&body));
    }
    let profile = body.pointer("/user/profile");
    let display_name = profile
        .and_then(|p| p.get("display_name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let real_name = profile
        .and_then(|p| p.get("real_name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    display_name
        .or(real_name)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("users.info 응답에 display_name/real_name 없음"))
}

/// DM 상대방 `user_id` → 표시 이름. `cache`(폴러 수명 인메모리 캐시)에 있으면 네트워크 호출 없이
/// 그대로 쓰고, 없으면 [`fetch_user_name`]으로 채운다. 조회 실패는 캐시에 남기지 않아(일시적
/// 오류일 수 있으므로) 다음 사이클에 다시 시도되며, 이번 호출은 `user_id` 자체를 폴백으로
/// 돌려준다(`channel_title`이 "DM: <id>"로 표시).
async fn resolve_dm_name(
    client: &reqwest::Client,
    token: &str,
    user_id: &str,
    cache: &mut HashMap<String, String>,
) -> String {
    if let Some(name) = cache.get(user_id) {
        return name.clone();
    }
    match fetch_user_name(client, token, user_id).await {
        Ok(name) => {
            cache.insert(user_id.to_string(), name.clone());
            name
        }
        Err(e) => {
            eprintln!("[logroom] slack: users.info 조회 실패({user_id}): {e} — DM 제목에 id 그대로 사용");
            user_id.to_string()
        }
    }
}

enum PageOutcome {
    /// 이번 페이지 매치들 + 남은 페이지 수(1부터 시작하는 페이지 기준 전체 `pages`).
    Matches(Vec<Value>, u32),
    /// 429 — `Retry-After` 헤더 값(초, 없으면 `None`).
    RateLimited(Option<u64>),
}

async fn fetch_search_page(
    client: &reqwest::Client,
    token: &str,
    query: &str,
    page: u32,
    sort_dir: &str,
) -> anyhow::Result<PageOutcome> {
    let resp = client
        .get("https://slack.com/api/search.messages")
        .bearer_auth(token)
        .query(&[
            ("query", query),
            ("sort", "timestamp"),
            ("sort_dir", sort_dir),
            ("count", &SEARCH_PAGE_SIZE.to_string()),
            ("page", &page.to_string()),
        ])
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        return Ok(PageOutcome::RateLimited(retry_after));
    }

    let body: Value = resp.json().await?;
    if !extract_ok(&body) {
        anyhow::bail!("search.messages 실패: {}", extract_error(&body));
    }
    let matches = body
        .pointer("/messages/matches")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let pages = body
        .pointer("/messages/paging/pages")
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    Ok(PageOutcome::Matches(matches, pages.max(1)))
}

/// 쿼리 하나에 대해 페이지네이션 전체를 순회해 매치를 모두 모은다. `sort_dir`은 `"asc"`(전방
/// 증분 — 커서 직후부터 순서대로) 또는 `"desc"`(후방 백필 — 최신부터 과거로) 중 하나를 호출부가
/// 지정한다. 429는 `Retry-After` 준수 + 지수 백오프(최대 5분)로 재시도하고, 그 외 에러는 즉시
/// 상위로 전파해 이번 폴링 주기를 포기한다(커서가 전진하지 않으므로 다음 주기에 안전하게
/// 재시도된다). 반환값의 `bool`은 [`hit_cycle_page_cap`] 판정 결과(로그성 정보 — 사이클 간격
/// 결정에는 쓰이지 않는다, [`poll_once`] 참고) — 페이지 사이에는 [`PAGE_REQUEST_INTERVAL_SECS`]
/// 간격을 둬 백필 가속 중에도 레이트리밋을 존중한다.
async fn fetch_all_matches(
    client: &reqwest::Client,
    token: &str,
    query: &str,
    sort_dir: &str,
) -> anyhow::Result<(Vec<Value>, bool)> {
    let mut all = Vec::new();
    let mut page = 1u32;
    let mut backoff_secs = 0u64;
    let capped;

    loop {
        match fetch_search_page(client, token, query, page, sort_dir).await? {
            PageOutcome::Matches(matches, total_pages) => {
                backoff_secs = 0;
                all.extend(matches);
                if page >= total_pages || page >= MAX_PAGES_PER_CYCLE {
                    capped = hit_cycle_page_cap(page, total_pages);
                    break;
                }
                tokio::time::sleep(Duration::from_secs(PAGE_REQUEST_INTERVAL_SECS)).await;
                page += 1;
            }
            PageOutcome::RateLimited(retry_after) => {
                backoff_secs = next_backoff_secs(backoff_secs, retry_after);
                eprintln!("[logroom] slack: 429, {backoff_secs}초 대기 후 재시도");
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                if backoff_secs >= MAX_BACKOFF_SECS {
                    anyhow::bail!("slack: 429 백오프 상한 도달, 이번 주기 포기");
                }
            }
        }
    }

    Ok((all, capped))
}

/// 커서 상태 로드 + (필요하면) 구 커서 마이그레이션 부수효과. 구 단일 커서
/// ([`LEGACY_CURSOR_RESOURCE`])가 있으면 값을 승계하지 않고 즉시 삭제한 뒤, [`resolve_cursor_state`]가
/// "첫 실행" 규칙을 적용하게 한다. 결정 로직 자체는 [`resolve_cursor_state`](순수함수)에 위임한다.
fn load_cursor_state(conn: &Connection) -> anyhow::Result<CursorState> {
    let legacy_exists = db::get_cursor(conn, SOURCE, LEGACY_CURSOR_RESOURCE)?.is_some();
    if legacy_exists {
        db::delete_cursor(conn, SOURCE, LEGACY_CURSOR_RESOURCE)?;
    }
    let newest_ts = db::get_cursor(conn, SOURCE, CURSOR_RESOURCE_NEWEST)?.map(|(offset, _)| offset);
    let backfill_row = db::get_cursor(conn, SOURCE, CURSOR_RESOURCE_BACKFILL)?;
    let backfill_ts = backfill_row.map(|(offset, _)| offset);
    let backfill_done = backfill_row.is_some_and(|(_, mtime)| mtime.is_some());
    Ok(resolve_cursor_state(legacy_exists, newest_ts, backfill_ts, backfill_done))
}

/// 폴링 1주기: **일시정지 체크(최상단, 조기 반환)** → 커서 상태 로드(+ 마이그레이션) →
/// 검색(부트스트랩 1회 또는 전방 증분+후방 백필) → ts 필터 → DM 이름 해석 → 정규화 →
/// (일시정지 아니면) 스크럽+ingest → 커서 전진. `capture_paused`가 true면 커서 로드조차 하지 않고
/// 즉시 반환한다 — Slack API 호출을 포함한 아웃바운드 네트워크 자체를 내지 않는다(커서 불변,
/// docs/04-privacy-security.md "일시정지 의미론": 커넥터도 파일감시와 동일하게 일시정지 중 네트워크를
/// 내지 않아야 함). 메시지가 없어도 폴링 자체가 성공하면 `search:newest` 커서의 `updated_at`을
/// 갱신해 "마지막 성공 폴링 시각"으로 쓴다(capture/health.rs "헬스" 참고 — 파일감시와 달리 이 값만으로
/// stale을 판정한다). 반환값(`bool`)은 이번 사이클 종료 시점의 backfill 완료 여부 — `spawn`의 루프가
/// `false`면 [`CATCHUP_INTERVAL_SECS`]로, `true`면 평소 간격(`pollMinutes`)으로 다음 사이클을 돈다.
/// 일시정지로 조기 반환하는 경우는 backfill 상태를 알 수 없으므로 `true`(평소 간격)를 돌려준다 —
/// 원인 불명 에러 시 폴백과 동일한 근거(레이트리밋 존중, 아래 `spawn`의 에러 처리 주석 참고).
#[allow(clippy::too_many_arguments)]
async fn poll_once(
    app: &AppHandle,
    db: &Db,
    capture_paused: &Arc<AtomicBool>,
    client: &reqwest::Client,
    token: &str,
    identity: &AuthIdentity,
    scrub_secrets: bool,
    dm_name_cache: &mut HashMap<String, String>,
) -> anyhow::Result<bool> {
    if capture_paused.load(Ordering::Relaxed) {
        return Ok(true);
    }

    let state = {
        let conn = db.lock().expect("db mutex poisoned");
        load_cursor_state(&conn)?
    };
    let today = Utc::now().date_naive();

    let mut combined: Vec<Value> = Vec::new();
    let new_newest_ts: Option<i64>;
    let new_backfill_ts: Option<i64>;
    let backfill_done: bool;
    let touched_backfill: bool;

    if is_bootstrap(&state) {
        // 첫 실행 또는 마이그레이션 직후: 상한 없이(desc) 최신 배치 1회로 두 커서를 동시에
        // 초기화한다(모듈 문서 "이중 커서" 참고). dm_names는 빈 맵으로 정규화해 ts 추출에만
        // 쓴다(title은 아래 최종 정규화 단계에서 실제 dm_names로 다시 계산).
        let query = build_backfill_query(&identity.user_id, None);
        let (matches, _capped) = fetch_all_matches(client, token, &query, "desc").await?;
        let ts_only = normalize_search_results(&matches, &identity.team, &HashMap::new());
        let (newest, backfill, done) = init_cursors_from_bootstrap_batch(min_event_ts(&ts_only), max_event_ts(&ts_only));
        new_newest_ts = newest;
        new_backfill_ts = backfill;
        backfill_done = done;
        touched_backfill = true;
        combined.extend(matches);
    } else {
        // 전방 증분: 매 사이클 수행 — 새로 도착한 메시지를 놓치지 않는다.
        let after_date = after_date_for_cursor(state.newest_ts, today);
        let fwd_query = build_search_query(&identity.user_id, after_date);
        let (fwd_matches, _capped) = fetch_all_matches(client, token, &fwd_query, "asc").await?;
        let fwd_filtered: Vec<Value> = fwd_matches
            .into_iter()
            .filter(|msg| {
                let ts = msg.get("ts").and_then(Value::as_str).and_then(parse_slack_ts);
                match (ts, state.newest_ts) {
                    (Some(ts), Some(cursor)) => ts > cursor,
                    _ => true,
                }
            })
            .collect();
        let fwd_max_ts = max_event_ts(&normalize_search_results(&fwd_filtered, &identity.team, &HashMap::new()));
        new_newest_ts = advance_newest(state.newest_ts, fwd_max_ts);
        combined.extend(fwd_filtered);

        // 후방 백필: 아직 바닥에 닿지 않았을 때만 — 한 배치(MAX_PAGES_PER_CYCLE 상한)를 desc로 전진.
        if state.backfill_done {
            new_backfill_ts = state.backfill_ts;
            backfill_done = true;
            touched_backfill = false;
        } else {
            let before_date = before_date_for_backfill(state.backfill_ts);
            let bwd_query = build_backfill_query(&identity.user_id, before_date);
            let (bwd_matches, capped) = fetch_all_matches(client, token, &bwd_query, "desc").await?;
            if capped {
                eprintln!("[logroom] slack: 백필 배치가 페이지 상한에 걸림 — 다음 사이클에 이어서 내려감");
            }
            let bwd_filtered: Vec<Value> = bwd_matches
                .into_iter()
                .filter(|msg| {
                    let ts = msg.get("ts").and_then(Value::as_str).and_then(parse_slack_ts);
                    match (ts, state.backfill_ts) {
                        (Some(ts), Some(cursor)) => ts < cursor,
                        _ => true,
                    }
                })
                .collect();
            let bwd_min_ts = min_event_ts(&normalize_search_results(&bwd_filtered, &identity.team, &HashMap::new()));
            let (backfill, done) = advance_backfill(state.backfill_ts, bwd_min_ts);
            new_backfill_ts = backfill;
            backfill_done = done;
            touched_backfill = true;
            combined.extend(bwd_filtered);
        }
    }

    let mut dm_names: HashMap<String, String> = HashMap::new();
    for user_id in dm_counterpart_ids(&combined) {
        let name = resolve_dm_name(client, token, &user_id, dm_name_cache).await;
        dm_names.insert(user_id, name);
    }

    let requests = normalize_search_results(&combined, &identity.team, &dm_names);
    let paused = capture_paused.load(Ordering::Relaxed);

    if !paused {
        for mut req in requests {
            if scrub_secrets {
                scrub::scrub_request(&mut req);
            }
            let conn = db.lock().expect("db mutex poisoned");
            let inserted = db::ingest(&conn, &req)?;
            if inserted > 0 {
                let _ = app.emit("event-ingested", serde_json::json!({ "inserted": inserted }));
            }
        }
    }

    // 새 메시지가 없어도 이 폴링 사이클이 성공했다는 사실 자체를 newest 커서의 updated_at 갱신으로
    // 남긴다 — capture/health.rs가 "마지막 성공 폴링 시각"으로 재사용한다.
    let conn = db.lock().expect("db mutex poisoned");
    db::upsert_cursor(&conn, SOURCE, CURSOR_RESOURCE_NEWEST, new_newest_ts.unwrap_or(0), None)?;
    if touched_backfill {
        // mtime을 "백필 완료" 플래그로 재사용(Some=완료, None=진행 중) — 모듈 문서 "이중 커서" 참고.
        let done_marker = if backfill_done { Some(1) } else { None };
        db::upsert_cursor(&conn, SOURCE, CURSOR_RESOURCE_BACKFILL, new_backfill_ts.unwrap_or(0), done_marker)?;
    }

    Ok(backfill_done)
}

/// 앱 시작 시 Slack 폴러를 백그라운드 태스크로 띄운다. `enabled=false`거나 토큰이 없으면 조용히
/// 종료한다(다른 캡처 소스의 "감시 대상 없음" 로그와 동일한 패턴, watch.rs 참고).
pub fn spawn(app: AppHandle, db: Db, capture_paused: Arc<AtomicBool>) {
    let cfg = config::load_slack_config();
    if !cfg.enabled {
        return;
    }
    let Some(token) = cfg.token.clone() else {
        eprintln!("[logroom] slack: enabled=true 이지만 토큰이 없어 폴러를 시작하지 않습니다");
        return;
    };
    let scrub_secrets = config::load_scrub_secrets();
    let poll_interval = Duration::from_secs(u64::from(cfg.poll_minutes.max(1)) * 60);

    tauri::async_runtime::spawn(async move {
        // 타임아웃(30초): 응답 지연/네트워크 장애 시 폴링 루프가 무한 대기하지 않게 한다.
        // 리다이렉트 비활성화: Slack API는 리다이렉트를 내려주지 않으므로, 토큰이 실린 요청이
        // 의도치 않은(또는 손상된) Location으로 재전송되는 것을 원천 차단하는 방어적 조치.
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(client) => client,
            Err(e) => {
                eprintln!("[logroom] slack: HTTP 클라이언트 생성 실패({e}) — 폴러 정지");
                return;
            }
        };

        let identity = match auth_test(&client, &token).await {
            Ok(identity) => identity,
            Err(e) => {
                eprintln!("[logroom] slack: auth.test 실패({e}) — 토큰을 확인하세요. 폴러 정지");
                return;
            }
        };
        eprintln!(
            "[logroom] slack: 폴러 시작(워크스페이스={}, {}분 간격)",
            identity.team, cfg.poll_minutes
        );

        // DM 상대방 user id → 표시 이름(폴러 수명 한정 인메모리 캐시, docs/08-connectors.md).
        let mut dm_name_cache: HashMap<String, String> = HashMap::new();

        loop {
            let backfill_done = match poll_once(
                &app,
                &db,
                &capture_paused,
                &client,
                &token,
                &identity,
                scrub_secrets,
                &mut dm_name_cache,
            )
            .await
            {
                Ok(backfill_done) => backfill_done,
                Err(e) => {
                    eprintln!("[logroom] slack: 폴링 실패({e}) — 다음 주기에 재시도");
                    // 실패로 현재 backfill 상태를 알 수 없을 때는 평소 간격으로 안전하게 재시도한다
                    // (레이트리밋 존중 — 원인 불명 오류를 짧은 간격으로 몰아치지 않는다).
                    true
                }
            };

            let sleep_duration = if backfill_done {
                poll_interval
            } else {
                eprintln!(
                    "[logroom] slack: 백필 미완료(밀린 이력 있음) — {CATCHUP_INTERVAL_SECS}초 뒤 다음 배치"
                );
                Duration::from_secs(CATCHUP_INTERVAL_SECS)
            };
            tokio::time::sleep(sleep_duration).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── parse_slack_ts ───────────────────────────────────────────

    #[test]
    fn parse_slack_ts_converts_seconds_and_micros_to_ms() {
        assert_eq!(parse_slack_ts("1730000000.123456"), Some(1_730_000_000_123));
    }

    #[test]
    fn parse_slack_ts_handles_seconds_only() {
        assert_eq!(parse_slack_ts("1730000000"), Some(1_730_000_000_000));
    }

    #[test]
    fn parse_slack_ts_handles_short_fraction() {
        // "1730000000.5" -> 5십만 마이크로초 -> 500ms.
        assert_eq!(parse_slack_ts("1730000000.5"), Some(1_730_000_000_500));
    }

    #[test]
    fn parse_slack_ts_invalid_returns_none() {
        assert_eq!(parse_slack_ts("not-a-ts"), None);
        assert_eq!(parse_slack_ts(""), None);
    }

    // ── normalize_search_results(정규화 순수함수) ─────────────────

    fn sample_match(channel_id: &str, channel_name: &str, ts: &str, text: &str) -> Value {
        json!({
            "type": "message",
            "channel": { "id": channel_id, "name": channel_name },
            "ts": ts,
            "text": text,
            "permalink": format!("https://example.slack.com/archives/{channel_id}/p{ts}"),
        })
    }

    fn no_dm_names() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn normalize_search_results_maps_channel_message_to_event_and_stream() {
        let messages = vec![sample_match("C123", "general", "1730000000.000100", "안녕하세요")];
        let requests = normalize_search_results(&messages, "acme-workspace", &no_dm_names());

        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        let stream = req.stream.as_ref().expect("stream 있어야 함");
        assert_eq!(stream.id, "slack:C123");
        assert_eq!(stream.source, "slack");
        assert_eq!(stream.kind.as_deref(), Some("session"));
        assert_eq!(stream.title.as_deref(), Some("#general"));
        assert_eq!(stream.project.as_deref(), Some("acme-workspace"));

        assert_eq!(req.events.len(), 1);
        let event = &req.events[0];
        assert_eq!(event.external_id, "C123:1730000000.000100");
        assert_eq!(event.stream_id, "slack:C123");
        assert_eq!(event.source, "slack");
        assert_eq!(event.event_type, "message");
        assert_eq!(event.body.as_deref(), Some("안녕하세요"));
        assert_eq!(event.ts, 1_730_000_000_000);
        assert!(event.url.as_deref().unwrap().contains("C123"));
    }

    #[test]
    fn normalize_search_results_dm_channel_without_name_uses_dm_title() {
        let dm = json!({
            "type": "message",
            "channel": { "id": "D999", "name": "", "is_im": true },
            "ts": "1730000001.000000",
            "text": "1:1 메시지",
        });
        let requests = normalize_search_results(&[dm], "acme-workspace", &no_dm_names());
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].stream.as_ref().unwrap().title.as_deref(), Some("DM"));
    }

    #[test]
    fn normalize_search_results_dm_channel_resolves_counterpart_name_from_map() {
        // 실측: search.messages의 DM channel 객체는 `name` 필드에 채널명 대신 상대방 user id를
        // 담아 돌려준다("#U04K8PA9JD6" 오염 title의 원인) — dm_names로 해석된 이름을 써야 한다.
        let dm = json!({
            "type": "message",
            "channel": { "id": "D04K8PA9JD6", "name": "U04K8PA9JD6", "is_im": true },
            "ts": "1730000002.000000",
            "text": "1:1 메시지",
        });
        let mut dm_names = HashMap::new();
        dm_names.insert("U04K8PA9JD6".to_string(), "홍길동".to_string());

        let requests = normalize_search_results(&[dm], "acme-workspace", &dm_names);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].stream.as_ref().unwrap().title.as_deref(),
            Some("DM: 홍길동")
        );
    }

    #[test]
    fn normalize_search_results_dm_channel_falls_back_to_raw_user_id_when_unresolved() {
        let dm = json!({
            "type": "message",
            "channel": { "id": "D04K8PA9JD6", "name": "U04K8PA9JD6", "is_im": true },
            "ts": "1730000003.000000",
            "text": "1:1 메시지",
        });
        // dm_names에 해당 user id가 없음(users.info 조회 실패/미조회) — id 그대로 폴백.
        let requests = normalize_search_results(&[dm], "acme-workspace", &no_dm_names());
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].stream.as_ref().unwrap().title.as_deref(),
            Some("DM: U04K8PA9JD6"),
            "id는 그대로 노출되더라도 'DM: ' 접두어로 DM임은 드러나야 함"
        );
    }

    #[test]
    fn normalize_search_results_group_dm_keeps_hash_prefixed_name() {
        // 그룹 DM은 is_im이 아니라 is_mpim이며, name 필드에 이미 참가자 나열이 담겨 있어
        // 현행 "#name" 포맷을 그대로 유지한다(docs/08-connectors.md "매핑").
        let group_dm = json!({
            "type": "message",
            "channel": { "id": "G123", "name": "mpdm-alice--bob-1", "is_mpim": true },
            "ts": "1730000004.000000",
            "text": "그룹 DM 메시지",
        });
        let requests = normalize_search_results(&[group_dm], "acme-workspace", &no_dm_names());
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].stream.as_ref().unwrap().title.as_deref(),
            Some("#mpdm-alice--bob-1")
        );
    }

    #[test]
    fn normalize_search_results_groups_multiple_channels_into_separate_requests() {
        let messages = vec![
            sample_match("C1", "general", "1730000000.000001", "첫 채널 메시지"),
            sample_match("C2", "random", "1730000000.000002", "둘째 채널 메시지"),
            sample_match("C1", "general", "1730000000.000003", "첫 채널 두번째 메시지"),
        ];
        let requests = normalize_search_results(&messages, "acme-workspace", &no_dm_names());
        assert_eq!(requests.len(), 2, "채널별로 요청이 분리돼야 함");

        let c1 = requests
            .iter()
            .find(|r| r.stream.as_ref().unwrap().id == "slack:C1")
            .expect("C1 요청 있어야 함");
        assert_eq!(c1.events.len(), 2);
        // ts 오름차순 정렬 확인.
        assert!(c1.events[0].ts <= c1.events[1].ts);
    }

    #[test]
    fn normalize_search_results_skips_message_missing_required_fields() {
        let broken = json!({ "type": "message", "text": "채널/ts 없음" });
        let requests = normalize_search_results(&[broken], "acme-workspace", &no_dm_names());
        assert!(requests.is_empty());
    }

    // ── is_dm_channel / dm_counterpart_user_id / dm_counterpart_ids ───────────────

    #[test]
    fn is_dm_channel_true_when_is_im_field_true() {
        assert!(is_dm_channel(&json!({ "id": "D1", "is_im": true })));
    }

    #[test]
    fn is_dm_channel_true_when_id_starts_with_d_and_is_im_missing() {
        assert!(is_dm_channel(&json!({ "id": "D1" })));
    }

    #[test]
    fn is_dm_channel_false_for_public_channel() {
        assert!(!is_dm_channel(&json!({ "id": "C1", "name": "general" })));
    }

    #[test]
    fn is_dm_channel_false_for_group_dm_even_with_g_prefixed_id() {
        assert!(!is_dm_channel(&json!({ "id": "G1", "name": "mpdm-a--b-1", "is_mpim": true })));
    }

    #[test]
    fn dm_counterpart_user_id_prefers_user_field_over_name() {
        let channel = json!({ "id": "D1", "user": "U1", "name": "U2" });
        assert_eq!(dm_counterpart_user_id(&channel), Some("U1"));
    }

    #[test]
    fn dm_counterpart_user_id_falls_back_to_name_field() {
        let channel = json!({ "id": "D1", "name": "U2" });
        assert_eq!(dm_counterpart_user_id(&channel), Some("U2"));
    }

    #[test]
    fn dm_counterpart_ids_dedupes_and_skips_non_dm_channels() {
        let messages = vec![
            json!({ "channel": { "id": "D1", "name": "U1", "is_im": true } }),
            json!({ "channel": { "id": "D1", "name": "U1", "is_im": true } }),
            json!({ "channel": { "id": "D2", "name": "U2", "is_im": true } }),
            json!({ "channel": { "id": "C1", "name": "general" } }),
        ];
        let mut ids = dm_counterpart_ids(&messages);
        ids.sort();
        assert_eq!(ids, vec!["U1".to_string(), "U2".to_string()]);
    }

    // ── max_event_ts ─────────────────────────────────────────────

    #[test]
    fn max_event_ts_returns_largest_ts_across_requests() {
        let messages = vec![
            sample_match("C1", "general", "1730000000.000001", "a"),
            sample_match("C2", "random", "1730000005.000001", "b"),
        ];
        let requests = normalize_search_results(&messages, "acme", &no_dm_names());
        assert_eq!(max_event_ts(&requests), Some(1_730_000_005_000));
    }

    #[test]
    fn max_event_ts_none_when_no_requests() {
        assert_eq!(max_event_ts(&[]), None);
    }

    // ── min_event_ts ─────────────────────────────────────────────

    #[test]
    fn min_event_ts_returns_smallest_ts_across_requests() {
        let messages = vec![
            sample_match("C1", "general", "1730000010.000001", "a"),
            sample_match("C2", "random", "1730000005.000001", "b"),
        ];
        let requests = normalize_search_results(&messages, "acme", &no_dm_names());
        assert_eq!(min_event_ts(&requests), Some(1_730_000_005_000));
    }

    #[test]
    fn min_event_ts_none_when_no_requests() {
        assert_eq!(min_event_ts(&[]), None);
    }

    // ── after_date_for_cursor(전방 증분 날짜 계산) ─────────────────

    #[test]
    fn after_date_for_cursor_none_uses_today() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 9).unwrap();
        assert_eq!(after_date_for_cursor(None, today), today);
    }

    #[test]
    fn after_date_for_cursor_some_uses_day_before_cursor_date() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 9).unwrap();
        // 2026-07-05 00:00:00 UTC epoch ms.
        let cursor_ts_ms = NaiveDate::from_ymd_opt(2026, 7, 5)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let expected = NaiveDate::from_ymd_opt(2026, 7, 4).unwrap();
        assert_eq!(after_date_for_cursor(Some(cursor_ts_ms), today), expected);
    }

    // ── before_date_for_backfill(후방 백필 날짜 계산) ──────────────

    #[test]
    fn before_date_for_backfill_none_when_no_cursor_yet() {
        // 첫 백필 배치는 상한 없이 최신 메시지부터 가져온다.
        assert_eq!(before_date_for_backfill(None), None);
    }

    #[test]
    fn before_date_for_backfill_adds_one_day_margin_to_avoid_boundary_omission() {
        let cursor_ts_ms = NaiveDate::from_ymd_opt(2026, 7, 5)
            .unwrap()
            .and_hms_opt(0, 30, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let expected = NaiveDate::from_ymd_opt(2026, 7, 6).unwrap();
        assert_eq!(before_date_for_backfill(Some(cursor_ts_ms)), Some(expected));
    }

    // ── build_search_query / build_backfill_query ──────────────────

    #[test]
    fn build_search_query_formats_from_and_after() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 4).unwrap();
        assert_eq!(
            build_search_query("U123ABC", date),
            "from:<@U123ABC> after:2026-07-04"
        );
    }

    #[test]
    fn build_backfill_query_includes_before_clause_when_date_given() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 6).unwrap();
        assert_eq!(
            build_backfill_query("U123ABC", Some(date)),
            "from:<@U123ABC> before:2026-07-06"
        );
    }

    #[test]
    fn build_backfill_query_omits_before_clause_for_bootstrap() {
        assert_eq!(build_backfill_query("U123ABC", None), "from:<@U123ABC>");
    }

    // ── next_backoff_secs(429 백오프) ─────────────────────────────

    #[test]
    fn next_backoff_secs_first_call_uses_initial_backoff() {
        assert_eq!(next_backoff_secs(0, None), INITIAL_BACKOFF_SECS);
    }

    #[test]
    fn next_backoff_secs_doubles_without_retry_after_header() {
        assert_eq!(next_backoff_secs(10, None), 20);
    }

    #[test]
    fn next_backoff_secs_respects_larger_retry_after_header() {
        assert_eq!(next_backoff_secs(5, Some(30)), 30);
    }

    #[test]
    fn next_backoff_secs_keeps_exponential_when_larger_than_retry_after() {
        assert_eq!(next_backoff_secs(60, Some(10)), 120);
    }

    #[test]
    fn next_backoff_secs_caps_at_max_backoff() {
        assert_eq!(next_backoff_secs(MAX_BACKOFF_SECS, None), MAX_BACKOFF_SECS);
        assert_eq!(next_backoff_secs(1000, Some(10_000)), MAX_BACKOFF_SECS);
    }

    // ── hit_cycle_page_cap(배치 상한 판정) ─────────────────────────

    #[test]
    fn hit_cycle_page_cap_true_when_more_pages_remain() {
        // 배치 상한(10)에 도달했지만 Slack이 전체 25페이지라고 응답 — 아직 15페이지가 남아있으므로
        // 이번 호출은 전체를 다 못 가져왔다는 뜻이다.
        assert!(hit_cycle_page_cap(MAX_PAGES_PER_CYCLE, 25));
    }

    #[test]
    fn hit_cycle_page_cap_false_when_cap_coincides_with_natural_end() {
        // 배치 상한(10)에 도달한 시점이 마침 전체 페이지의 끝이기도 함 — 더 가져올 게 없다.
        assert!(!hit_cycle_page_cap(MAX_PAGES_PER_CYCLE, MAX_PAGES_PER_CYCLE));
    }

    #[test]
    fn hit_cycle_page_cap_false_when_natural_end_reached_before_cap() {
        // 배치 상한(10)보다 한참 적은 페이지에서 자연스럽게 끝남 — 애초에 상한에 걸리지 않았다.
        assert!(!hit_cycle_page_cap(3, 3));
    }

    // ── CursorState / resolve_cursor_state / is_bootstrap(구 커서 마이그레이션 · 첫 실행 판정) ──

    #[test]
    fn resolve_cursor_state_passes_through_when_no_legacy_cursor() {
        let state = resolve_cursor_state(false, Some(100), Some(50), false);
        assert_eq!(
            state,
            CursorState {
                newest_ts: Some(100),
                backfill_ts: Some(50),
                backfill_done: false
            }
        );
    }

    #[test]
    fn resolve_cursor_state_ignores_existing_new_cursor_values_when_legacy_exists() {
        // 구 커서가 남아있으면 새 커서 값이 우연히 존재하더라도 무시하고 첫 실행 규칙을
        // 재적용한다(구 커서 값은 asc로 진행 중이던 과거 어느 중간 지점이라 newest로 승계할 수
        // 없음 — docs/08-connectors.md "백필").
        let state = resolve_cursor_state(true, Some(100), Some(50), true);
        assert_eq!(
            state,
            CursorState {
                newest_ts: None,
                backfill_ts: None,
                backfill_done: false
            }
        );
    }

    #[test]
    fn is_bootstrap_true_when_both_cursors_absent() {
        assert!(is_bootstrap(&CursorState {
            newest_ts: None,
            backfill_ts: None,
            backfill_done: false
        }));
    }

    #[test]
    fn is_bootstrap_false_when_either_cursor_present() {
        assert!(!is_bootstrap(&CursorState {
            newest_ts: Some(1),
            backfill_ts: None,
            backfill_done: false
        }));
        assert!(!is_bootstrap(&CursorState {
            newest_ts: None,
            backfill_ts: Some(1),
            backfill_done: false
        }));
    }

    // ── init_cursors_from_bootstrap_batch(첫 실행 초기화) ──────────

    #[test]
    fn init_cursors_from_bootstrap_batch_sets_newest_and_backfill_from_batch_extremes() {
        let (newest, backfill, done) = init_cursors_from_bootstrap_batch(Some(100), Some(500));
        assert_eq!(newest, Some(500));
        assert_eq!(backfill, Some(100));
        assert!(!done);
    }

    #[test]
    fn init_cursors_from_bootstrap_batch_empty_marks_backfill_done_without_setting_cursors() {
        // 이 계정으로 보낸 메시지가 전혀 없는 경우 — 두 커서 모두 그대로 두고 backfill만 완료
        // 처리해 다음 사이클부터 정상 증분 경로(after_date_for_cursor(None, ..) = 오늘)로 진입한다.
        let (newest, backfill, done) = init_cursors_from_bootstrap_batch(None, None);
        assert_eq!(newest, None);
        assert_eq!(backfill, None);
        assert!(done);
    }

    // ── advance_newest(전방 증분 갱신) ─────────────────────────────

    #[test]
    fn advance_newest_uses_new_batch_max_when_present() {
        assert_eq!(advance_newest(Some(100), Some(200)), Some(200));
    }

    #[test]
    fn advance_newest_keeps_previous_when_no_new_messages() {
        assert_eq!(advance_newest(Some(100), None), Some(100));
    }

    // ── advance_backfill(백필 전진 · 바닥 판정) ────────────────────

    #[test]
    fn advance_backfill_advances_to_batch_minimum() {
        let (backfill, done) = advance_backfill(Some(1000), Some(700));
        assert_eq!(backfill, Some(700));
        assert!(!done);
    }

    #[test]
    fn advance_backfill_marks_done_when_batch_empty() {
        // 결과 0건 = 바닥 도달 — 완료 마킹하고 이전 값은 그대로 유지한다(재시작해도 다시 안 돎).
        let (backfill, done) = advance_backfill(Some(700), None);
        assert_eq!(backfill, Some(700));
        assert!(done);
    }
}
