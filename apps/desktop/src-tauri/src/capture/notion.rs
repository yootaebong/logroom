//! Notion 커넥터(v1) — 로컬 폴링 + 다중 계정(개인·회사 워크스페이스)
//! (docs/08-connectors.md "Notion v1", ADR-0015 "OAuth v2 보류"). 참조 구현은 다중 계정+계정별
//! 폴러 태스크+토큰 검증+백오프 구조를 그대로 따르는 `capture/linear.rs`이고, 단일 커서 로드/전진과
//! 헬스 연동은 `capture/slack.rs`/`capture/health.rs::classify_slack`을 재사용한다.
//!
//! ## 왜 Slack/GitHub/Linear와 아키텍처가 다른가(docs/08-connectors.md "Notion v1" §왜 다른가)
//! ① `POST /v1/search`에 시간 범위 필터가 없다 — 지원 필터는 `object`(page/data_source)와
//! `in_trash`뿐이고 정렬만 `last_edited_time` asc/desc다. Slack의 `after:`/`before:`, Linear의
//! `updatedAt: { gt/lt }`에 해당하는 게 없어 **이중 커서(전방 증분+후방 백필)를 쓸 수 없다**(아래
//! "커서" 참고). ② 편집 이력 API가 없다 — `last_edited_time`은 스냅샷 한 개뿐이라 재편집하면
//! 덮어쓰인다. 커넥터를 켜기 전의 과거 편집 이력은 원천적으로 확보할 수 없다(아래 "한계"). ③ 토큰
//! 종류에 따라 "나"를 알 수 있는지가 갈린다(아래 "인증").
//!
//! ## 인증 — 토큰 두 종류, 판별은 폴러가 한다(docs/08-connectors.md "Notion v1" §인증)
//! 사용자는 토큰을 붙여넣기만 한다. 폴러가 `GET /v1/users/me`의 `type`으로 종류를 가른다
//! ([`parse_identity`]):
//! - **Personal Access Token**(`type: "person"`, 2026-05 도입) — 토큰을 만든 사람이 온다. 그 사람이 볼
//!   수 있는 페이지 전체가 보이고(페이지별 연결 불필요), **마지막 편집자가 그 사람인 편집만 남긴다**
//!   ([`keep_edits_by`]). 회사 워크스페이스는 남의 편집이 대부분이라 이 필터가 없으면 쓸 수 없다.
//!   워크스페이스 소유자가 아니어도 만들 수 있어(Plus는 전원, Business/Enterprise는 관리자가 허용한
//!   사람) **회사 워크스페이스의 일반 구성원이 쓸 수 있는 유일한 방식**이다.
//! - **Internal Integration Secret**(`type: "bot"`) — 워크스페이스 소유자만 만들 수 있고, 페이지마다
//!   integration을 연결한 범위만 보인다. bot이라 "나"를 알 수 없어 **편집자 필터가 없다** — 연결된
//!   페이지의 모든 편집이 들어온다. 읽기 전용 권한으로 좁힐 수 있다는 점 때문에 남겨 둔다.
//!
//! 워크스페이스 식별: integration은 `bot.workspace_id`/`workspace_name`을 주지만 **PAT는 워크스페이스
//! 정보를 전혀 주지 않고**, 같은 사람이 개인·회사 워크스페이스에 각각 PAT를 만들면 `users/me`가 같은
//! 사람을 돌려준다. 그래서 커서 resource 키는 API 응답이 아니라 **로컬 계정 id**
//! (`NotionAccount::id`)이고, 타임라인의 `project`는 사용자가 붙인 이름(`label`)을 쓴다. 인증 헤더는
//! `Authorization: Bearer <token>`이다(Linear의 접두어 없는 헤더와 다르다).
//!
//! ## 스코프(docs/08-connectors.md "Notion v1" §스코프)
//! **page 객체만** 받는다(`filter: { property: "object", value: "page" }`, 데이터베이스 아이템도 page
//! 객체라 DB 콘텐츠는 그대로 들어오지만 `data_source` 객체 자체의 편집은 스코프 밖). 휴지통은
//! 제외(`in_trash` 기본 false). **본문(블록)·댓글은 저장하지 않는다** — 제목만 저장한다.
//!
//! ## 커서 — 단일 커서 + 조기 중단(docs/08-connectors.md "Notion v1" §커서, 셋 중 가장 단순)
//! 시간 필터가 없어 "건너뛰기"가 불가능하다. 대신 desc 정렬이 보장하는 순서를 이용해 위에서부터
//! 읽다가 이미 본 지점을 만나면 멈춘다. `capture_cursors`는 `source = "notion"`, `resource =
//! "search:<계정 id>"` **하나**만 쓴다(Slack/Linear의 `:newest`/`:backfill` 쌍 없음, `mtime`도
//! 쓰지 않음 — "백필 완료" 개념 자체가 없다).
//! - `offset` = 지금까지 **훑은** 최대 `last_edited_time`(epoch ms). 편집자 필터로 버린 항목도 포함한다
//!   — 남긴 것만으로 계산하면 커서가 "내 마지막 편집"에 묶여, 남이 편집을 많이 하는 회사
//!   워크스페이스에서는 매 사이클 그 지점까지 수천 건을 다시 훑는다.
//! - **증분 사이클**([`take_until_cursor_boundary`]): desc 정렬로 페이지를 순회하며
//!   `last_edited_time < cursor`인 첫 항목을 만나면 **그 페이지까지 처리하고 중단**한다(desc 정렬이라
//!   그 아래는 전부 이미 본 것). 평상시에는 1페이지만 읽는다. 경계의 동일 ms 중복은
//!   `(source, external_id)` UNIQUE 제약으로 자연히 dedup된다.
//! - **첫 실행**(커서 없음): `has_more`가 끝날 때까지 전량 1회 스캔한다. 안전장치로 절대 상한
//!   ([`MAX_BOOTSTRAP_PAGES`])을 두고, 상한에 걸리면 경고 로그를 남기고 그 배치의 최댓값을 커서로
//!   저장한다(그보다 오래된 페이지는 확보하지 못한다 — 아래 "한계").
//! - **폴링 성공 표시**: 새 편집이 없어도 사이클이 정상 완료되면 커서의 `updated_at`만 갱신한다
//!   (Slack과 동일 — 헬스 판정의 "마지막 성공 폴링 시각"을 별도 상태 없이 cursor 하나로 표현).
//! - **따라잡기 가속 없음** — 백필 개념이 없으므로 항상 `pollMinutes` 간격이다.
//!
//! ## 매핑(docs/08-connectors.md "Notion v1" §매핑)
//! **페이지=스트림**(`notion:<page id>`, Linear가 팀=스트림에서 이슈=스트림으로 재매핑한 전례와
//! 같은 이유 — 하루치 활동이 한 덩어리로 뭉치면 "오늘 무슨 문서를 만졌나"가 안 보인다).
//! `project` = 계정 이름([`project_name`] — `label` → integration의 `workspace_name` → `"Notion"`).
//! 상위 부모 체인에서 뽑는 방식은 페이지마다 추가 호출이 붙어 v1에서 채택하지 않는다.
//! `event.type`은 항상 `"message"`.
//! **`externalId`에 시각을 넣는 것이 이 커넥터의 핵심 결정이다**(`nt:<page id>:<last_edited_time
//! ms>`) — `nt:<page id>`로 고정하면 UNIQUE 제약에 걸려 최초 1건 이후 같은 페이지의 편집이 영영
//! 기록되지 않는다("왜 다른가" ②의 가치가 통째로 사라진다). 시각을 포함시키면 며칠에 걸쳐 고친
//! 문서가 날짜별로 남고, 같은 스냅샷을 여러 사이클에 걸쳐 다시 봐도 idempotent하다. page id는
//! 전역 UUID라 계정이 여럿이어도 겹치지 않는다. `title`은 `created_time == last_edited_time`이면
//! `"<제목> 생성"`, 그 외에는 `"<제목> 편집"`([`event_title_for_page`]).
//! `event.body`는 페이지 제목(본문 미수집 — 위 "스코프"). `event.url`은 page object의 `url`.
//! **페이지 제목 추출**([`extract_page_title`]): `properties`에서 `type == "title"`인 속성의
//! `title[].plain_text`를 이어붙인다. 속성이 없거나 비면 `"제목 없음"`으로 폴백한다(조용히 skip하지
//! 않는다 — 제목 없는 페이지도 편집 활동이다).
//!
//! ## Rate limit 전략(docs/08-connectors.md "Notion v1" §Rate limit 전략)
//! 연결(토큰) 단위 60초 창 180회(Business/Enterprise는 600회)이고, 워크스페이스 단위 한도가 따로
//! 있다(2026-09 기준). 페이지 요청 사이 최소 [`PAGE_REQUEST_INTERVAL_MS`](400ms, 분당 150회) 간격을
//! 둔다. **429·529**: `Retry-After` 헤더(초)만큼 대기 후 재시도하고, 연속 실패는 지수 백오프로
//! 늘리되 최대 5분 상한(Slack/GitHub/Linear와 동일 정책). 상한에 도달한 뒤에도 실패하면 이번 주기는
//! 포기하고 다음 주기에 커서 위치부터 재시도한다(커서가 전진하지 않았으므로 유실 없음). **401**(무효
//! 토큰 — PAT는 만료일이 있다)은 그 계정 폴러 자체를 중단시키고 로그를 남긴다(검증 실패가 아니라
//! 폴링 도중 토큰이 무효화된 경우도 동일하게 취급, [`CycleOutcome::Unauthorized`]). **403**은
//! 권한/블록 한도 문제라 즉시 이번 주기를 포기하고 다음 주기에 재시도한다. 그 외 에러(네트워크, JSON
//! 파싱 실패 등)는 로그만 남기고 다음 주기 재시도.
//!
//! ## 한계(docs/08-connectors.md "Notion v1" §한계)
//! 과거 편집 이력을 확보할 수 없다(위 "왜 다른가" ②). PAT의 편집자 필터는 **마지막 편집자**만
//! 본다 — 내가 고친 뒤 폴링 전에 동료가 같은 페이지를 고치면 내 편집은 남지 않는다. integration은
//! 편집자 필터가 없어 팀 워크스페이스에서는 남의 편집도 들어온다. 본문·댓글 미수집. integration은
//! 연결 누락이 조용히 실패한다(헬스가 `ok`라도 연결된 페이지가 0개면 캡처는 0건 —
//! `capture/health.rs::compute_health`의 주석 참고). 첫 실행 절대 상한 초과 시 일부 페이지 누락.
//!
//! **주의**: `docs/08-connectors.md`는 안전 상한 초과 로그를 `tracing::warn!`으로 표기했으나, 이
//! 레포는 tracing 크레이트를 쓰지 않고(Cargo.toml 미의존) 다른 세 커넥터 모두 `eprintln!`을 쓴다 —
//! 새 의존성을 들이지 않기 위해 기존 관례(`eprintln!`)를 그대로 따른다(구현 중 판단 지점).
//!
//! 정규화·커서 조기중단 판정·편집자 필터·토큰 판별·제목 추출·백오프·요청 바디 조립 같은 순수함수는
//! 네트워크/DB 없이 단위테스트한다. 네트워크 호출·폴링 루프(`poll_account_once`/`spawn`)는 이 파일의
//! 테스트 대상이 아니다(다른 커넥터와 동일한 관례 — 수동 확인 필요).

use crate::capture::config::{self, NotionAccount, NotionTokenKind};
use crate::capture::scrub;
use crate::db;
use crate::model::{EventInput, IngestRequest, StreamInput};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const SOURCE: &str = "notion";

/// Notion API 버전 고정(docs/08-connectors.md "Notion v1" §스코프) — 2026-09 기준 최신이다. 이전
/// 버전(`2025-09-03`, `filter.value`가 `database` → `data_source`로 바뀐 버전)과의 차이는 `archived` →
/// `in_trash` 등 이 커넥터가 읽지 않는 필드뿐이다. 버전을 고정해야 응답 shape이 흔들리지 않는다.
const NOTION_VERSION: &str = "2026-03-11";
const API_BASE: &str = "https://api.notion.com/v1";
/// 페이지 없을 때 노출하는 제목 폴백(docs "매핑" — 조용히 skip하지 않는다).
const DEFAULT_PAGE_TITLE: &str = "제목 없음";
/// 계정 이름(`label`)도 워크스페이스명도 없을 때 쓰는 `project` 폴백(docs "매핑").
const DEFAULT_PROJECT: &str = "Notion";
/// `POST /v1/search` 한 페이지 크기.
const PAGE_SIZE: u32 = 100;
/// 첫 실행(커서 없음) 전량 스캔의 절대 페이지 상한 — 초과 시 경고 로그 + 그 배치의 최댓값을
/// 커서로 저장한다(docs "커서").
const MAX_BOOTSTRAP_PAGES: u32 = 200;
/// 페이지 요청 사이 최소 간격(ms) — 평균 초당 3회 제한보다 여유 있게 잡는다(docs "Rate limit 전략").
const PAGE_REQUEST_INTERVAL_MS: u64 = 400;
/// 429/529 연속 실패 시 백오프 상한(초, Slack/GitHub/Linear와 동일 정책).
const MAX_BACKOFF_SECS: u64 = 5 * 60;
/// 429가 `Retry-After` 헤더를 안 주는 드문 경우의 초기 백오프(초).
const INITIAL_BACKOFF_SECS: u64 = 5;

type Db = Arc<Mutex<rusqlite::Connection>>;

/// `notion:<page id>` 형태의 stream id(페이지=스트림, docs "매핑").
pub fn stream_id(page_id: &str) -> String {
    format!("{SOURCE}:{page_id}")
}

/// `capture_cursors` resource 키: `"search:<계정 id>"`(docs "커서").
fn search_resource(account_id: &str) -> String {
    format!("search:{account_id}")
}

/// `GET /v1/users/me`로 판별한 토큰의 주인(docs "인증").
#[derive(Debug, Clone, PartialEq)]
enum Identity {
    /// Personal Access Token — 토큰을 만든 사람. 이 사람이 마지막으로 편집한 것만 남긴다.
    Person {
        user_id: String,
        name: Option<String>,
    },
    /// Internal Integration Secret — bot. 연결된 페이지의 모든 편집이 들어온다.
    Bot {
        workspace_id: Option<String>,
        workspace_name: Option<String>,
    },
}

impl Identity {
    /// 편집자 필터 기준 user id — PAT만 값이 있다(bot은 "나"를 알 수 없다).
    fn editor_filter(&self) -> Option<&str> {
        match self {
            Identity::Person { user_id, .. } => Some(user_id),
            Identity::Bot { .. } => None,
        }
    }

    fn kind(&self) -> NotionTokenKind {
        match self {
            Identity::Person { .. } => NotionTokenKind::Personal,
            Identity::Bot { .. } => NotionTokenKind::Integration,
        }
    }

    fn workspace_name(&self) -> Option<&str> {
        match self {
            Identity::Person { .. } => None,
            Identity::Bot { workspace_name, .. } => workspace_name.as_deref(),
        }
    }
}

/// `users/me` 응답 → [`Identity`]. `type`이 `person`이면 PAT, `bot`이면 internal integration이다.
/// PAT인데 `id`가 없으면 편집자 필터를 걸 수 없으므로 `None`(호출부가 이 계정을 건너뛴다 — 필터 없이
/// 돌리면 회사 워크스페이스의 남의 편집이 전부 들어온다). 빈 문자열은 값이 없는 것으로 본다.
fn parse_identity(payload: &Value) -> Option<Identity> {
    let non_empty = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match payload.get("type").and_then(Value::as_str)? {
        "person" => Some(Identity::Person {
            user_id: non_empty(payload.get("id"))?,
            name: non_empty(payload.get("name")),
        }),
        "bot" => Some(Identity::Bot {
            workspace_id: non_empty(payload.pointer("/bot/workspace_id")),
            workspace_name: non_empty(payload.pointer("/bot/workspace_name")),
        }),
        _ => None,
    }
}

/// 스트림 `project` — 사용자가 붙인 이름 → integration의 워크스페이스명 → `"Notion"`(docs "매핑").
/// PAT는 워크스페이스명을 알 수 없어서 이름을 안 붙이면 계정들이 모두 `"Notion"`으로 뭉친다.
fn project_name(label: Option<&str>, workspace_name: Option<&str>) -> String {
    label
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .or(workspace_name.filter(|w| !w.is_empty()))
        .unwrap_or(DEFAULT_PROJECT)
        .to_string()
}

/// RFC3339 timestamp(`"2021-01-01T12:00:00.000Z"`) → epoch ms. 파싱 실패 시 `None`(호출부가 해당
/// 페이지를 skip).
fn parse_notion_timestamp(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).timestamp_millis())
}

/// 페이지 `properties`에서 `type == "title"`인 속성의 `title[].plain_text`를 이어붙인다(docs
/// "매핑"). 속성이 없거나 배열이 비어있으면 빈 문자열(호출부가 [`DEFAULT_PAGE_TITLE`]로 폴백).
fn extract_page_title(properties: &Value) -> String {
    let Some(props) = properties.as_object() else {
        return String::new();
    };
    for prop in props.values() {
        if prop.get("type").and_then(Value::as_str) != Some("title") {
            continue;
        }
        return prop
            .get("title")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|t| t.get("plain_text").and_then(Value::as_str))
                    .collect::<String>()
            })
            .unwrap_or_default();
    }
    String::new()
}

/// 이벤트 title: `created_time == last_edited_time`이면 생성, 그 외에는 편집(docs "매핑").
fn event_title_for_page(
    display_title: &str,
    created_time_ms: i64,
    last_edited_time_ms: i64,
) -> String {
    if created_time_ms == last_edited_time_ms {
        format!("{display_title} 생성")
    } else {
        format!("{display_title} 편집")
    }
}

/// 정규화된 페이지 1건 — 그룹핑([`finalize_groups`])과 조기 중단 판정([`take_until_cursor_boundary`])이
/// 함께 쓰는 `last_edited_time`을 이벤트와 별도로 들고 다닌다(`capture/linear.rs::MappedEvent`와
/// 동일한 이유 — 이벤트 자체에는 굳이 다시 파싱하지 않도록).
struct MappedPage {
    page_id: String,
    /// 스트림 title로 쓰일 페이지 제목(비어있지 않음 — [`DEFAULT_PAGE_TITLE`] 폴백 적용됨).
    title: String,
    last_edited_time: i64,
    /// `last_edited_by.id` — PAT 편집자 필터([`keep_edits_by`])가 쓴다. 응답에 없으면 `None`.
    last_edited_by: Option<String>,
    event: EventInput,
}

/// `search` 응답의 page 객체 1건 → [`MappedPage`]. 필수 필드(`id`/`created_time`/`last_edited_time`)가
/// 없거나 파싱 실패하면 `None`(방어적 skip — 문서 근거로만 작성된 shape이라 실제 응답과 다를 수
/// 있음).
fn map_page_node(node: &Value) -> Option<MappedPage> {
    let page_id = node.get("id").and_then(Value::as_str)?.to_string();
    let created_time = node
        .get("created_time")
        .and_then(Value::as_str)
        .and_then(parse_notion_timestamp)?;
    let last_edited_time = node
        .get("last_edited_time")
        .and_then(Value::as_str)
        .and_then(parse_notion_timestamp)?;
    let url = node.get("url").and_then(Value::as_str).map(str::to_string);
    let last_edited_by = node
        .pointer("/last_edited_by/id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let raw_title = node
        .get("properties")
        .map(extract_page_title)
        .unwrap_or_default();
    let display_title = if raw_title.is_empty() {
        DEFAULT_PAGE_TITLE.to_string()
    } else {
        raw_title
    };
    let event_title = event_title_for_page(&display_title, created_time, last_edited_time);

    let event = EventInput {
        external_id: format!("nt:{page_id}:{last_edited_time}"),
        stream_id: stream_id(&page_id),
        ts: last_edited_time,
        source: SOURCE.to_string(),
        event_type: "message".to_string(),
        title: Some(event_title),
        body: Some(display_title.clone()),
        model: None,
        tokens_in: None,
        tokens_out: None,
        url,
        parent_id: None,
        metadata: None,
    };

    Some(MappedPage {
        page_id,
        title: display_title,
        last_edited_time,
        last_edited_by,
        event,
    })
}

fn map_page_nodes(nodes: &[Value]) -> Vec<MappedPage> {
    nodes.iter().filter_map(map_page_node).collect()
}

/// 증분 사이클에서 desc 정렬 배치 하나를 순회하며 커서 경계를 찾는다(docs "커서" §증분 사이클).
/// `cursor`가 있고 `last_edited_time`이 **cursor보다 작은** 첫 항목을 만나면 그 항목까지 포함해
/// 잘라내고 `true`(조기 중단)를 반환한다. 경계를 못 찾으면 배치 전체 + `false`(다음 페이지 계속).
/// `cursor`가 `None`이면(첫 실행) 항상 배치 전체 + `false`(전량 스캔, [`MAX_BOOTSTRAP_PAGES`] 상한은
/// 호출부에 적용된다).
///
/// **`<= cursor`가 아니라 `< cursor`인 이유** — Notion의 `last_edited_time`은 해상도가 거칠어
/// (초/분 단위로 절단돼 내려온다) 서로 다른 페이지가 **같은 값**을 갖는 일이 흔하다. 이전 사이클이
/// 커서를 T로 올린 뒤 같은 T 구간 안에서 다른 페이지가 편집되면, `<=`로 자를 경우 desc 배치의 첫
/// T 항목에서 멈춰 **그 뒤에 이어지는 같은 T 항목들을 영영 놓친다**(다음 사이클에도 커서가 T라
/// 똑같이 잘린다). `<`로 두면 T 구간 전체를 매번 다시 훑고, 이미 저장된 것은
/// `(source, external_id)` UNIQUE 제약으로 조용히 dedup된다 — 경계 중복은 무해하지만 누락은
/// 복구 불가라는 Slack 커넥터의 원칙(docs/08-connectors.md Slack v1 "이중 커서")과 같은 판단이다.
fn take_until_cursor_boundary(
    mapped: Vec<MappedPage>,
    cursor: Option<i64>,
) -> (Vec<MappedPage>, bool) {
    let Some(cursor) = cursor else {
        return (mapped, false);
    };
    match mapped.iter().position(|m| m.last_edited_time < cursor) {
        Some(idx) => (mapped.into_iter().take(idx + 1).collect(), true),
        None => (mapped, false),
    }
}

/// PAT 편집자 필터(docs "인증"): `editor`가 있으면 마지막 편집자가 그 사람인 페이지만 남긴다.
/// `last_edited_by`가 응답에 없는 항목은 누가 고쳤는지 알 수 없으므로 버린다(남의 편집을 내 것으로
/// 기록하는 쪽보다 하나 놓치는 쪽이 낫다). `editor`가 `None`(integration)이면 그대로 둔다.
///
/// **커서 전진은 이 필터 전의 목록으로 계산해야 한다**(모듈 문서 "커서" §offset) — 호출부가
/// [`max_last_edited_time`]을 먼저 구한 뒤 이 함수를 부른다.
fn keep_edits_by(mapped: Vec<MappedPage>, editor: Option<&str>) -> Vec<MappedPage> {
    let Some(editor) = editor else {
        return mapped;
    };
    mapped
        .into_iter()
        .filter(|m| m.last_edited_by.as_deref() == Some(editor))
        .collect()
}

/// 이번 사이클에서 수집한 페이지들 중 최대 `last_edited_time`(ms) — 커서 전진에 사용(docs "커서"
/// §offset). 비어있으면 `None`(호출부가 이전 커서 값을 그대로 유지).
fn max_last_edited_time(mapped: &[MappedPage]) -> Option<i64> {
    mapped.iter().map(|m| m.last_edited_time).max()
}

/// 페이지별로 그룹핑된 [`MappedPage`] 목록 → `IngestRequest` 목록(페이지당 1건 그룹핑 —
/// `capture/slack.rs::normalize_search_results`/`capture/linear.rs::finalize_groups`와 동일 계약,
/// `db::ingest()`가 요청 1건당 하나의 `stream` upsert를 전제하기 때문). 실제로는 한 사이클에 같은
/// 페이지가 두 번 이상 잡힐 일이 거의 없지만(검색 결과 자체가 페이지당 1건), 방어적으로 그룹핑하고
/// 이벤트를 ts 오름차순 정렬한다. `project`는 계정 이름([`project_name`])이다(계정 전체가 공유하는
/// 값이라 페이지별로 들고 다니지 않는다).
fn finalize_groups(mapped: Vec<MappedPage>, project: &str) -> Vec<IngestRequest> {
    let mut groups: BTreeMap<String, (String, Vec<EventInput>)> = BTreeMap::new();
    for m in mapped {
        let entry = groups
            .entry(m.page_id.clone())
            .or_insert_with(|| (m.title.clone(), Vec::new()));
        entry.1.push(m.event);
    }
    groups
        .into_iter()
        .map(|(page_id, (title, mut events))| {
            events.sort_by_key(|e| e.ts);
            let stream = StreamInput {
                id: stream_id(&page_id),
                source: SOURCE.to_string(),
                kind: Some("session".to_string()),
                title: Some(title),
                project: Some(project.to_string()),
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

/// `POST /v1/search` 요청 바디(docs "왜 다른가" ①): `filter`/`sort`/`page_size` 고정 +
/// `start_cursor`(있을 때만 포함 — 첫 페이지 요청은 이 키 자체가 없다).
fn search_request_body(start_cursor: Option<&str>) -> Value {
    let mut body = serde_json::json!({
        "filter": { "property": "object", "value": "page" },
        "sort": { "timestamp": "last_edited_time", "direction": "descending" },
        "page_size": PAGE_SIZE,
    });
    if let Some(cursor) = start_cursor {
        body["start_cursor"] = Value::String(cursor.to_string());
    }
    body
}

/// 429 연속 실패 시 다음 백오프 대기시간(초). Slack/GitHub/Linear와 동일한 계산(모듈마다 상수만
/// 로컬로 두고 로직은 독립적으로 유지).
fn next_backoff_secs(previous_secs: u64, retry_after: Option<u64>) -> u64 {
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

/// 계정 목록에서 `account_id`가 일치하는 계정에 검증 결과(토큰 종류·사람 이름·워크스페이스)를
/// 적는다(폴러가 인증 검증에 성공한 뒤 반영 — FE 계정 목록 표시용, `capture/linear.rs::apply_resolved_viewer`와
/// 동일 패턴). 변경이 있었으면 `true`.
fn apply_resolved_identity(
    accounts: &mut [NotionAccount],
    account_id: &str,
    identity: &Identity,
) -> bool {
    let (user_name, workspace_id, workspace_name) = match identity {
        Identity::Person { name, .. } => (name.clone(), None, None),
        Identity::Bot {
            workspace_id,
            workspace_name,
        } => (None, workspace_id.clone(), workspace_name.clone()),
    };
    let mut changed = false;
    for account in accounts.iter_mut() {
        if account.id.as_deref() != Some(account_id) {
            continue;
        }
        let next = NotionAccount {
            kind: Some(identity.kind()),
            user_name: user_name.clone(),
            workspace_id: workspace_id.clone(),
            workspace_name: workspace_name.clone(),
            ..account.clone()
        };
        if *account != next {
            *account = next;
            changed = true;
        }
    }
    changed
}

// ── 네트워크 호출(단위테스트 대상 아님 — 모듈 문서 "주의" 참고) ────────────

/// `CycleOutcome::Unauthorized`는 `spawn`의 루프가 이 값을 받으면 해당 계정의 폴러 태스크 자체를
/// 종료한다(docs "Rate limit 전략" — 401은 검증 실패뿐 아니라 폴링 도중 토큰이 무효화된 경우도
/// 동일하게 취급). `Completed`는 새 항목 유무와 무관하게 사이클이 정상 종료됐다는 뜻(에러 없음).
enum CycleOutcome {
    Completed,
    Unauthorized,
}

enum PageOutcome {
    /// page 노드 + has_more + next_cursor.
    Nodes(Vec<Value>, bool, Option<String>),
    /// 429/529 — `Retry-After` 헤더 값(초, 없으면 `None`).
    RateLimited(Option<u64>),
    /// 401 — 토큰 무효화.
    Unauthorized,
}

async fn fetch_identity(client: &reqwest::Client, token: &str) -> anyhow::Result<Identity> {
    let resp = client
        .get(format!("{API_BASE}/users/me"))
        .bearer_auth(token)
        .header("Notion-Version", NOTION_VERSION)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("Notion users/me 조회 실패: HTTP {}", resp.status());
    }
    let payload: Value = resp.json().await?;
    parse_identity(&payload).ok_or_else(|| {
        anyhow::anyhow!(
            "Notion users/me 응답에서 토큰 종류를 알 수 없음(type={:?})",
            payload.get("type")
        )
    })
}

/// `POST /v1/search` 한 페이지 호출. `.json(&body)`가 `Content-Type: application/json`을 자동
/// 설정한다(docs "인증"의 헤더 목록에 있는 값이지만 reqwest가 항상 덮어써 명시적으로 다시 설정할
/// 필요가 없다).
async fn fetch_search_page(
    client: &reqwest::Client,
    token: &str,
    start_cursor: Option<&str>,
) -> anyhow::Result<PageOutcome> {
    let body = search_request_body(start_cursor);
    let resp = client
        .post(format!("{API_BASE}/search"))
        .bearer_auth(token)
        .header("Notion-Version", NOTION_VERSION)
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(PageOutcome::Unauthorized);
    }
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.as_u16() == 529 {
        return Ok(PageOutcome::RateLimited(retry_after));
    }
    if !status.is_success() {
        let snippet: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect();
        anyhow::bail!("Notion search 실패: HTTP {status} — {snippet}");
    }

    let payload: Value = resp.json().await?;
    // 결과 상한에 걸리면 `request_status.type = "incomplete"`가 온다(2026-04 도입). 첫 실행의 전량
    // 스캔이 여기서 끝날 수 있다 — has_more가 false로 오므로 루프는 정상 종료되고, 로그만 남긴다.
    if payload
        .pointer("/request_status/type")
        .and_then(Value::as_str)
        == Some("incomplete")
    {
        eprintln!(
            "[logroom] notion: search 결과 상한 도달({:?}) — 더 오래된 페이지는 확보하지 못합니다",
            payload.pointer("/request_status/reason")
        );
    }
    let nodes = payload
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_more = payload
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let next_cursor = payload
        .get("next_cursor")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(PageOutcome::Nodes(nodes, has_more, next_cursor))
}

/// 검증 결과를 설정에 되쓴다. 계정마다 태스크가 따로 돌며 거의 동시에 부르므로 반드시
/// [`config::update_config`](읽기~저장 직렬화)를 거친다 — 각자 읽고 쓰면 나중 쓰기가 앞 쓰기를 되돌린다.
fn persist_resolved_identity(account_id: &str, identity: &Identity) {
    if let Err(e) = config::update_config(|cfg| {
        apply_resolved_identity(&mut cfg.notion.accounts, account_id, identity)
    }) {
        eprintln!("[logroom] notion: 계정 정보 저장 실패({e})");
    }
}

/// id가 없는 계정(손으로 고친 설정, id 도입 전 설정)에 id를 채워 저장한다. 채운 결과를 돌려준다 —
/// 저장에 실패해도 이번 실행 동안은 그 id로 커서를 쓴다(다음 실행에서 id가 바뀌면 첫 실행 경로를
/// 다시 타지만 UNIQUE 제약으로 중복은 생기지 않는다).
fn ensure_account_ids(mut accounts: Vec<NotionAccount>) -> Vec<NotionAccount> {
    if config::assign_missing_notion_account_ids(&mut accounts) {
        // 파일의 계정 목록을 토큰 기준으로 맞춰 id만 옮겨 적는다 — 읽은 뒤 사용자가 계정을 지웠다면
        // 그 계정은 되살리지 않는다.
        let result = config::update_config(|cfg| {
            let mut changed = false;
            for stored in cfg.notion.accounts.iter_mut() {
                let assigned = accounts
                    .iter()
                    .find(|a| a.token == stored.token)
                    .and_then(|a| a.id.clone());
                if assigned.is_some() && stored.id != assigned {
                    stored.id = assigned;
                    changed = true;
                }
            }
            changed
        });
        if let Err(e) = result {
            eprintln!("[logroom] notion: 계정 id 저장 실패({e})");
        }
    }
    accounts
}

/// 계정 1개의 폴링 1주기: **일시정지 체크(최상단, 조기 반환)** → 단일 커서 로드 → desc 정렬 배치
/// 순회(첫 실행은 [`MAX_BOOTSTRAP_PAGES`] 상한까지 전량, 그 외에는 [`take_until_cursor_boundary`]로
/// 조기 중단) → 커서 후보 계산 → 편집자 필터([`keep_edits_by`], PAT만) → 페이지별
/// 그룹핑([`finalize_groups`]) → (일시정지 아니면) 스크럽+ingest → 커서 전진(새 항목이 없어도
/// `updated_at`은 갱신 — docs "커서" §폴링 성공 표시). 401을 만나면 즉시 [`CycleOutcome::Unauthorized`]를
/// 반환해 `spawn`이 이 계정의 폴러를 중단하게 한다.
#[allow(clippy::too_many_arguments)]
async fn poll_account_once(
    app: &AppHandle,
    db: &Db,
    capture_paused: &Arc<AtomicBool>,
    client: &reqwest::Client,
    token: &str,
    account_id: &str,
    project: &str,
    editor: Option<&str>,
    scrub_secrets: bool,
) -> anyhow::Result<CycleOutcome> {
    if capture_paused.load(Ordering::Relaxed) {
        return Ok(CycleOutcome::Completed);
    }

    let resource = search_resource(account_id);
    // 커서 row 자체는 "마지막 성공 폴링 시각"(헬스 판정)을 위해 결과가 0건인 사이클에도 저장되므로,
    // offset이 0인 row가 존재할 수 있다(연결된 페이지가 아직 0개인 워크스페이스). 그 상태는 아직
    // 아무것도 읽지 못한 것이라 **첫 실행과 똑같이** 다뤄야 한다 — 그러지 않으면 조기 중단 판정과
    // [`MAX_BOOTSTRAP_PAGES`] 상한이 모두 빗나간다(상한 없이 전량을 긁는다).
    let cursor = {
        let conn = db.lock().expect("db mutex poisoned");
        db::get_cursor(&conn, SOURCE, &resource)?.map(|(offset, _)| offset)
    }
    .filter(|&offset| offset > 0);

    let mut collected: Vec<MappedPage> = Vec::new();
    let mut start_cursor: Option<String> = None;
    let mut backoff_secs = 0u64;
    let mut pages_fetched = 0u32;

    loop {
        match fetch_search_page(client, token, start_cursor.as_deref()).await? {
            PageOutcome::Unauthorized => return Ok(CycleOutcome::Unauthorized),
            PageOutcome::RateLimited(retry_after) => {
                backoff_secs = next_backoff_secs(backoff_secs, retry_after);
                eprintln!(
                    "[logroom] notion({project}): rate limited, {backoff_secs}초 대기 후 재시도"
                );
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                if backoff_secs >= MAX_BACKOFF_SECS {
                    anyhow::bail!("notion({project}): 백오프 상한 도달, 이번 주기 포기");
                }
            }
            PageOutcome::Nodes(nodes, has_more, next_cursor) => {
                backoff_secs = 0;
                pages_fetched += 1;
                let mapped = map_page_nodes(&nodes);
                let (batch, hit_boundary) = take_until_cursor_boundary(mapped, cursor);
                collected.extend(batch);

                if hit_boundary || !has_more || next_cursor.is_none() {
                    break;
                }
                if cursor.is_none() && pages_fetched >= MAX_BOOTSTRAP_PAGES {
                    eprintln!(
                        "[logroom] notion({project}): 첫 실행 안전 상한({MAX_BOOTSTRAP_PAGES}페이지) 도달 — 이전 페이지는 확보하지 못합니다"
                    );
                    break;
                }
                start_cursor = next_cursor;
                tokio::time::sleep(Duration::from_millis(PAGE_REQUEST_INTERVAL_MS)).await;
            }
        }
    }

    // 커서는 버리기 전 목록으로 계산한다(모듈 문서 "커서" §offset).
    let new_cursor_ts = max_last_edited_time(&collected).or(cursor);
    let requests = finalize_groups(keep_edits_by(collected, editor), project);

    let paused = capture_paused.load(Ordering::Relaxed);
    if !paused {
        for mut req in requests {
            if scrub_secrets {
                scrub::scrub_request(&mut req);
            }
            let conn = db.lock().expect("db mutex poisoned");
            let inserted = db::ingest(&conn, &req)?;
            if inserted > 0 {
                let _ = app.emit(
                    "event-ingested",
                    serde_json::json!({ "inserted": inserted }),
                );
            }
        }
    }

    let conn = db.lock().expect("db mutex poisoned");
    db::upsert_cursor(&conn, SOURCE, &resource, new_cursor_ts.unwrap_or(0), None)?;

    Ok(CycleOutcome::Completed)
}

/// 앱 시작 시 계정별 Notion 폴러를 백그라운드 태스크로 띄운다(`enabled=false`거나 계정이 없으면
/// 조용히 종료 — 다른 캡처 소스와 동일 패턴). 계정마다 독립된 태스크·커서를 가지므로 한 계정의 토큰이
/// 무효화돼도 다른 계정에는 영향이 없다(GitHub/Linear `spawn`과 동일 설계). **따라잡기 가속이 없어**
/// 항상 `pollMinutes` 간격으로만 돈다(docs "커서" §따라잡기 가속 없음).
pub fn spawn(app: AppHandle, db: Db, capture_paused: Arc<AtomicBool>) {
    let cfg = config::load_notion_config();
    if !cfg.enabled || cfg.accounts.is_empty() {
        return;
    }
    let scrub_secrets = config::load_scrub_secrets();
    let poll_minutes = cfg.poll_minutes.max(1);
    let poll_interval = Duration::from_secs(u64::from(poll_minutes) * 60);

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            eprintln!("[logroom] notion: HTTP 클라이언트 생성 실패({e}) — 폴러 정지");
            return;
        }
    };

    for account in ensure_account_ids(cfg.accounts) {
        let app = app.clone();
        let db = db.clone();
        let capture_paused = capture_paused.clone();
        let client = client.clone();
        let Some(account_id) = account.id.clone() else {
            continue;
        };

        tauri::async_runtime::spawn(async move {
            let token = account.token.as_str();
            let identity = match fetch_identity(&client, token).await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "[logroom] notion: 계정 인증 실패({e}) — 이 계정은 건너뜁니다(토큰 만료·폐기 확인 필요)"
                    );
                    return;
                }
            };
            persist_resolved_identity(&account_id, &identity);
            let project = project_name(account.label.as_deref(), identity.workspace_name());
            let mode = match identity.kind() {
                NotionTokenKind::Personal => "내 편집만",
                NotionTokenKind::Integration => "연결된 페이지 전체",
            };
            eprintln!("[logroom] notion: 폴러 시작(계정={project}, {mode}, {poll_minutes}분 간격)");

            loop {
                match poll_account_once(
                    &app,
                    &db,
                    &capture_paused,
                    &client,
                    token,
                    &account_id,
                    &project,
                    identity.editor_filter(),
                    scrub_secrets,
                )
                .await
                {
                    Ok(CycleOutcome::Completed) => {}
                    Ok(CycleOutcome::Unauthorized) => {
                        eprintln!(
                            "[logroom] notion({project}): 토큰 무효화(401) 감지 — 폴러를 중단합니다(만료됐으면 새 토큰 필요)"
                        );
                        break;
                    }
                    Err(e) => {
                        eprintln!(
                            "[logroom] notion({project}): 폴링 실패({e}) — 다음 주기에 재시도"
                        );
                    }
                }
                tokio::time::sleep(poll_interval).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── stream_id / 커서 resource 키 ─────────────────────────────

    #[test]
    fn stream_id_formats_source_prefix() {
        assert_eq!(stream_id("page-uuid-1"), "notion:page-uuid-1");
    }

    #[test]
    fn search_resource_embeds_account_id() {
        assert_eq!(search_resource("account-1"), "search:account-1");
    }

    // ── 토큰 판별(users/me) ───────────────────────────────────

    #[test]
    fn parse_identity_person_is_personal_access_token() {
        let payload = json!({
            "object": "user",
            "id": "user-1",
            "type": "person",
            "name": "alex",
            "person": { "email": "alex@example.com" },
        });
        let identity = parse_identity(&payload).expect("PAT는 사람으로 판별돼야 함");
        assert_eq!(
            identity,
            Identity::Person {
                user_id: "user-1".to_string(),
                name: Some("alex".to_string()),
            }
        );
        assert_eq!(identity.kind(), NotionTokenKind::Personal);
        assert_eq!(identity.editor_filter(), Some("user-1"));
        assert_eq!(identity.workspace_name(), None);
    }

    #[test]
    fn parse_identity_bot_is_integration_without_editor_filter() {
        let payload = json!({
            "object": "user",
            "id": "bot-1",
            "type": "bot",
            "bot": {
                "owner": { "type": "workspace", "workspace": true },
                "workspace_name": "Acme",
                "workspace_id": "w1",
            },
        });
        let identity = parse_identity(&payload).expect("bot은 integration으로 판별돼야 함");
        assert_eq!(identity.kind(), NotionTokenKind::Integration);
        assert_eq!(identity.editor_filter(), None);
        assert_eq!(identity.workspace_name(), Some("Acme"));
    }

    #[test]
    fn parse_identity_bot_without_workspace_fields_still_parses() {
        let payload = json!({ "object": "user", "id": "bot-1", "type": "bot", "bot": {} });
        assert_eq!(
            parse_identity(&payload),
            Some(Identity::Bot {
                workspace_id: None,
                workspace_name: None,
            })
        );
    }

    #[test]
    fn parse_identity_person_without_id_is_rejected() {
        // 편집자 필터를 걸 수 없는 PAT를 필터 없이 돌리면 남의 편집이 전부 들어온다 — 거부해야 한다.
        assert_eq!(
            parse_identity(&json!({ "type": "person", "name": "alex" })),
            None
        );
        assert_eq!(parse_identity(&json!({ "type": "person", "id": "" })), None);
    }

    #[test]
    fn parse_identity_unknown_type_is_rejected() {
        assert_eq!(
            parse_identity(&json!({ "object": "error", "status": 401 })),
            None
        );
    }

    // ── project 이름 ─────────────────────────────────────────────

    #[test]
    fn project_name_prefers_label_then_workspace_then_default() {
        assert_eq!(project_name(Some(" 회사 "), Some("Acme")), "회사");
        assert_eq!(project_name(None, Some("Acme")), "Acme");
        assert_eq!(project_name(Some("  "), Some("Acme")), "Acme");
        assert_eq!(project_name(None, None), "Notion");
        assert_eq!(project_name(None, Some("")), "Notion");
    }

    // ── 타임스탬프 변환 ────────────────────────────────────────────

    #[test]
    fn parse_notion_timestamp_parses_rfc3339() {
        assert_eq!(
            parse_notion_timestamp("2021-01-01T00:00:00.000Z"),
            Some(1_609_459_200_000)
        );
    }

    #[test]
    fn parse_notion_timestamp_invalid_returns_none() {
        assert_eq!(parse_notion_timestamp("not-a-date"), None);
    }

    // ── extract_page_title ────────────────────────────────────────

    #[test]
    fn extract_page_title_joins_plain_text_segments() {
        let properties = json!({
            "Name": {
                "id": "title",
                "type": "title",
                "title": [
                    { "plain_text": "회의록 " },
                    { "plain_text": "2026-09-21" },
                ],
            },
        });
        assert_eq!(extract_page_title(&properties), "회의록 2026-09-21");
    }

    #[test]
    fn extract_page_title_finds_title_property_regardless_of_key_name() {
        // 데이터베이스 아이템은 "title" 대신 "Name" 등 임의 키를 쓴다 — type으로 찾아야 한다.
        let properties = json!({
            "Status": { "id": "s1", "type": "select", "select": { "name": "In Progress" } },
            "Page": { "id": "title", "type": "title", "title": [{ "plain_text": "작업 A" }] },
        });
        assert_eq!(extract_page_title(&properties), "작업 A");
    }

    #[test]
    fn extract_page_title_missing_title_property_returns_empty() {
        let properties = json!({ "Status": { "id": "s1", "type": "select" } });
        assert_eq!(extract_page_title(&properties), "");
    }

    #[test]
    fn extract_page_title_empty_title_array_returns_empty() {
        let properties = json!({ "Name": { "id": "title", "type": "title", "title": [] } });
        assert_eq!(extract_page_title(&properties), "");
    }

    // ── event_title_for_page ────────────────────────────────────

    #[test]
    fn event_title_for_page_uses_creation_suffix_when_timestamps_equal() {
        assert_eq!(event_title_for_page("회의록", 1000, 1000), "회의록 생성");
    }

    #[test]
    fn event_title_for_page_uses_edit_suffix_when_timestamps_differ() {
        assert_eq!(event_title_for_page("회의록", 1000, 2000), "회의록 편집");
    }

    // ── map_page_node ─────────────────────────────────────────────

    fn page_node(overrides: Value) -> Value {
        let mut base = json!({
            "object": "page",
            "id": "page-uuid-1",
            "created_time": "2021-01-01T00:00:00.000Z",
            "last_edited_time": "2021-01-01T00:00:00.000Z",
            "url": "https://www.notion.so/page-uuid-1",
            "created_by": { "object": "user", "id": "user-me" },
            "last_edited_by": { "object": "user", "id": "user-me" },
            "properties": {
                "Name": { "id": "title", "type": "title", "title": [{ "plain_text": "회의록" }] },
            },
        });
        for (k, v) in overrides.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    }

    #[test]
    fn map_page_node_maps_all_fields_as_created_when_timestamps_equal() {
        let mapped = map_page_node(&page_node(json!({}))).expect("매핑돼야 함");
        assert_eq!(mapped.page_id, "page-uuid-1");
        assert_eq!(mapped.title, "회의록");
        assert_eq!(mapped.last_edited_time, 1_609_459_200_000);
        assert_eq!(mapped.event.external_id, "nt:page-uuid-1:1609459200000");
        assert_eq!(mapped.event.stream_id, "notion:page-uuid-1");
        assert_eq!(mapped.event.event_type, "message");
        assert_eq!(mapped.event.title.as_deref(), Some("회의록 생성"));
        assert_eq!(mapped.event.body.as_deref(), Some("회의록"));
        assert_eq!(
            mapped.event.url.as_deref(),
            Some("https://www.notion.so/page-uuid-1")
        );
        assert_eq!(mapped.event.ts, 1_609_459_200_000);
        assert_eq!(mapped.last_edited_by.as_deref(), Some("user-me"));
    }

    #[test]
    fn map_page_node_missing_last_edited_by_is_none() {
        let mut node = page_node(json!({}));
        node.as_object_mut().unwrap().remove("last_edited_by");
        let mapped = map_page_node(&node).expect("편집자가 없어도 매핑은 돼야 함");
        assert_eq!(mapped.last_edited_by, None);
    }

    #[test]
    fn map_page_node_uses_edit_suffix_when_edited_after_creation() {
        let node = page_node(json!({ "last_edited_time": "2021-01-02T00:00:00.000Z" }));
        let mapped = map_page_node(&node).expect("매핑돼야 함");
        assert_eq!(mapped.event.title.as_deref(), Some("회의록 편집"));
        assert_eq!(mapped.event.ts, 1_609_545_600_000);
        assert_eq!(mapped.event.external_id, "nt:page-uuid-1:1609545600000");
    }

    #[test]
    fn map_page_node_missing_title_property_falls_back_to_default() {
        let node = page_node(json!({ "properties": {} }));
        let mapped = map_page_node(&node).expect("매핑돼야 함");
        assert_eq!(mapped.title, "제목 없음");
        assert_eq!(mapped.event.title.as_deref(), Some("제목 없음 생성"));
        assert_eq!(mapped.event.body.as_deref(), Some("제목 없음"));
    }

    #[test]
    fn map_page_node_missing_id_returns_none() {
        let mut node = page_node(json!({}));
        node.as_object_mut().unwrap().remove("id");
        assert!(map_page_node(&node).is_none());
    }

    #[test]
    fn map_page_node_missing_created_time_returns_none() {
        let mut node = page_node(json!({}));
        node.as_object_mut().unwrap().remove("created_time");
        assert!(map_page_node(&node).is_none());
    }

    #[test]
    fn map_page_node_missing_last_edited_time_returns_none() {
        let mut node = page_node(json!({}));
        node.as_object_mut().unwrap().remove("last_edited_time");
        assert!(map_page_node(&node).is_none());
    }

    #[test]
    fn map_page_node_missing_url_is_none() {
        let mut node = page_node(json!({}));
        node.as_object_mut().unwrap().remove("url");
        let mapped = map_page_node(&node).expect("매핑돼야 함");
        assert_eq!(mapped.event.url, None);
    }

    #[test]
    fn map_page_nodes_skips_malformed_entries() {
        let nodes = vec![json!({ "id": "broken" })];
        assert!(map_page_nodes(&nodes).is_empty());
    }

    // ── take_until_cursor_boundary(조기 중단, docs "커서") ──────────

    fn mapped_page(page_id: &str, last_edited_time: i64) -> MappedPage {
        let node = page_node(json!({
            "id": page_id,
            "created_time": "2021-01-01T00:00:00.000Z",
            "last_edited_time": DateTime::<Utc>::from_timestamp_millis(last_edited_time)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }));
        map_page_node(&node).expect("테스트 fixture는 항상 매핑돼야 함")
    }

    #[test]
    fn take_until_cursor_boundary_no_cursor_returns_all_without_stopping() {
        let batch = vec![
            mapped_page("a", 300),
            mapped_page("b", 200),
            mapped_page("c", 100),
        ];
        let (kept, stopped) = take_until_cursor_boundary(batch, None);
        assert_eq!(kept.len(), 3);
        assert!(!stopped);
    }

    #[test]
    fn take_until_cursor_boundary_stops_at_first_item_below_cursor() {
        // desc 정렬: 300, 200, 150, 100 — cursor=150이면 150(경계와 동일)은 계속 담고,
        // 150보다 작은 첫 항목(100)까지 포함한 뒤 중단한다.
        let batch = vec![
            mapped_page("a", 300),
            mapped_page("b", 200),
            mapped_page("c", 150),
            mapped_page("d", 100),
        ];
        let (kept, stopped) = take_until_cursor_boundary(batch, Some(150));
        assert_eq!(
            kept.iter().map(|m| m.page_id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c", "d"]
        );
        assert!(stopped);
    }

    #[test]
    fn take_until_cursor_boundary_keeps_every_item_sharing_the_cursor_timestamp() {
        // 회귀 방지: Notion의 last_edited_time은 해상도가 거칠어 서로 다른 페이지가 같은 값을 갖는다.
        // 커서와 동일한 ts 항목에서 잘라버리면 그 뒤의 같은 ts 항목을 영영 놓친다(다음 사이클에도
        // 커서가 그대로라 똑같이 잘린다). 같은 ts는 전부 담고, 더 오래된 것에서 멈춰야 한다.
        let batch = vec![
            mapped_page("a", 100),
            mapped_page("b", 100),
            mapped_page("c", 100),
            mapped_page("d", 90),
        ];
        let (kept, stopped) = take_until_cursor_boundary(batch, Some(100));
        assert_eq!(
            kept.iter().map(|m| m.page_id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c", "d"]
        );
        assert!(stopped);
    }

    #[test]
    fn take_until_cursor_boundary_continues_when_whole_batch_shares_the_cursor_timestamp() {
        // 배치 전체가 커서와 같은 ts면 경계를 못 찾은 것 — 다음 페이지를 계속 가져와야 한다.
        let batch = vec![mapped_page("a", 100), mapped_page("b", 100)];
        let (kept, stopped) = take_until_cursor_boundary(batch, Some(100));
        assert_eq!(kept.len(), 2);
        assert!(!stopped);
    }

    #[test]
    fn take_until_cursor_boundary_no_boundary_found_returns_all_and_continues() {
        // 배치 전체가 cursor보다 최신이면(경계를 못 찾음) 다음 페이지를 계속 가져와야 함.
        let batch = vec![mapped_page("a", 300), mapped_page("b", 200)];
        let (kept, stopped) = take_until_cursor_boundary(batch, Some(100));
        assert_eq!(kept.len(), 2);
        assert!(!stopped);
    }

    #[test]
    fn take_until_cursor_boundary_empty_batch_returns_empty_without_stopping() {
        let (kept, stopped) = take_until_cursor_boundary(Vec::new(), Some(100));
        assert!(kept.is_empty());
        assert!(!stopped);
    }

    // ── keep_edits_by(PAT 편집자 필터) ─────────────────────────────

    fn edited_by(page_id: &str, last_edited_time: i64, editor: Option<&str>) -> MappedPage {
        let mut page = mapped_page(page_id, last_edited_time);
        page.last_edited_by = editor.map(str::to_string);
        page
    }

    #[test]
    fn keep_edits_by_keeps_only_the_given_editor() {
        let batch = vec![
            edited_by("mine", 300, Some("user-me")),
            edited_by("colleague", 200, Some("user-other")),
            edited_by("unknown", 100, None),
        ];
        let kept = keep_edits_by(batch, Some("user-me"));
        assert_eq!(
            kept.iter().map(|m| m.page_id.as_str()).collect::<Vec<_>>(),
            vec!["mine"]
        );
    }

    #[test]
    fn keep_edits_by_without_editor_keeps_everything() {
        // integration(bot)은 "나"를 모른다 — 연결된 페이지의 모든 편집을 남긴다.
        let batch = vec![
            edited_by("a", 300, Some("user-me")),
            edited_by("b", 200, Some("user-other")),
            edited_by("c", 100, None),
        ];
        assert_eq!(keep_edits_by(batch, None).len(), 3);
    }

    #[test]
    fn cursor_is_computed_before_the_editor_filter() {
        // 회귀 방지: 남긴 것만으로 커서를 계산하면 커서가 "내 마지막 편집"(100)에 묶여, 남의 편집이
        // 많은 회사 워크스페이스에서는 매 사이클 그 지점까지 다시 훑는다. 훑은 최댓값(300)이어야 한다.
        let batch = vec![
            edited_by("colleague", 300, Some("user-other")),
            edited_by("mine", 100, Some("user-me")),
        ];
        let cursor = max_last_edited_time(&batch);
        let kept = keep_edits_by(batch, Some("user-me"));
        assert_eq!(cursor, Some(300));
        assert_eq!(max_last_edited_time(&kept), Some(100));
    }

    // ── max_last_edited_time ────────────────────────────────────

    #[test]
    fn max_last_edited_time_returns_largest_value() {
        let batch = vec![
            mapped_page("a", 100),
            mapped_page("b", 300),
            mapped_page("c", 200),
        ];
        assert_eq!(max_last_edited_time(&batch), Some(300));
    }

    #[test]
    fn max_last_edited_time_none_when_empty() {
        assert_eq!(max_last_edited_time(&[]), None);
    }

    // ── finalize_groups ──────────────────────────────────────────

    #[test]
    fn finalize_groups_maps_page_to_stream_with_workspace_as_project() {
        let mapped = map_page_nodes(&[page_node(json!({}))]);
        let requests = finalize_groups(mapped, "Acme Workspace");
        assert_eq!(requests.len(), 1);
        let stream = requests[0].stream.as_ref().expect("stream 있어야 함");
        assert_eq!(stream.id, "notion:page-uuid-1");
        assert_eq!(stream.source, "notion");
        assert_eq!(stream.kind.as_deref(), Some("session"));
        assert_eq!(stream.title.as_deref(), Some("회의록"));
        assert_eq!(stream.project.as_deref(), Some("Acme Workspace"));
        assert_eq!(requests[0].events.len(), 1);
    }

    #[test]
    fn finalize_groups_groups_by_page_id_and_sorts_events_by_ts() {
        let nodes = vec![
            page_node(json!({ "id": "page-a", "last_edited_time": "2021-01-02T00:00:00.000Z" })),
            page_node(json!({ "id": "page-b" })),
        ];
        let mapped = map_page_nodes(&nodes);
        let requests = finalize_groups(mapped, "Acme");
        assert_eq!(requests.len(), 2, "페이지별로 요청이 분리돼야 함");

        let page_a = requests
            .iter()
            .find(|r| r.stream.as_ref().unwrap().id == "notion:page-a")
            .expect("page-a 요청 있어야 함");
        assert_eq!(page_a.events.len(), 1);
    }

    #[test]
    fn finalize_groups_empty_input_returns_empty() {
        assert!(finalize_groups(Vec::new(), "Acme").is_empty());
    }

    // ── search_request_body ─────────────────────────────────────

    #[test]
    fn search_request_body_without_cursor_omits_start_cursor_key() {
        let body = search_request_body(None);
        assert_eq!(
            body,
            json!({
                "filter": { "property": "object", "value": "page" },
                "sort": { "timestamp": "last_edited_time", "direction": "descending" },
                "page_size": 100,
            })
        );
        assert!(body.get("start_cursor").is_none());
    }

    #[test]
    fn search_request_body_with_cursor_includes_start_cursor() {
        let body = search_request_body(Some("cursor-abc"));
        assert_eq!(
            body,
            json!({
                "filter": { "property": "object", "value": "page" },
                "sort": { "timestamp": "last_edited_time", "direction": "descending" },
                "page_size": 100,
                "start_cursor": "cursor-abc",
            })
        );
    }

    // ── 백오프 ───────────────────────────────────────────────────

    #[test]
    fn next_backoff_secs_starts_at_initial_and_doubles() {
        let first = next_backoff_secs(0, None);
        assert_eq!(first, INITIAL_BACKOFF_SECS);
        let second = next_backoff_secs(first, None);
        assert_eq!(second, INITIAL_BACKOFF_SECS * 2);
    }

    #[test]
    fn next_backoff_secs_caps_at_max() {
        assert_eq!(next_backoff_secs(MAX_BACKOFF_SECS, None), MAX_BACKOFF_SECS);
    }

    #[test]
    fn next_backoff_secs_uses_retry_after_when_larger() {
        assert_eq!(next_backoff_secs(0, Some(120)), 120);
    }

    // ── 검증 결과 적용(계정 매칭) ───────────────────────────────

    fn account(id: &str, token: &str) -> NotionAccount {
        NotionAccount {
            token: token.to_string(),
            id: Some(id.to_string()),
            ..NotionAccount::default()
        }
    }

    #[test]
    fn apply_resolved_identity_records_personal_token_on_matching_account_only() {
        let mut accounts = vec![
            account("id-personal", "ntn_1"),
            account("id-company", "ntn_2"),
        ];
        let identity = Identity::Person {
            user_id: "user-1".to_string(),
            name: Some("alex".to_string()),
        };
        assert!(apply_resolved_identity(
            &mut accounts,
            "id-company",
            &identity
        ));
        assert_eq!(accounts[1].kind, Some(NotionTokenKind::Personal));
        assert_eq!(accounts[1].user_name.as_deref(), Some("alex"));
        assert_eq!(accounts[1].workspace_name, None);
        assert_eq!(accounts[0].kind, None, "다른 계정은 건드리지 않아야 함");
    }

    #[test]
    fn apply_resolved_identity_records_integration_workspace() {
        let mut accounts = vec![account("id-1", "ntn_1")];
        let identity = Identity::Bot {
            workspace_id: Some("w1".to_string()),
            workspace_name: Some("Acme".to_string()),
        };
        assert!(apply_resolved_identity(&mut accounts, "id-1", &identity));
        assert_eq!(accounts[0].kind, Some(NotionTokenKind::Integration));
        assert_eq!(accounts[0].workspace_id.as_deref(), Some("w1"));
        assert_eq!(accounts[0].workspace_name.as_deref(), Some("Acme"));
        assert_eq!(accounts[0].user_name, None);
    }

    #[test]
    fn apply_resolved_identity_keeps_label_and_token() {
        let mut accounts = vec![NotionAccount {
            label: Some("회사".to_string()),
            ..account("id-1", "ntn_1")
        }];
        let identity = Identity::Person {
            user_id: "user-1".to_string(),
            name: None,
        };
        apply_resolved_identity(&mut accounts, "id-1", &identity);
        assert_eq!(accounts[0].label.as_deref(), Some("회사"));
        assert_eq!(accounts[0].token, "ntn_1");
    }

    #[test]
    fn apply_resolved_identity_no_change_when_already_up_to_date() {
        let mut accounts = vec![account("id-1", "ntn_1")];
        let identity = Identity::Bot {
            workspace_id: Some("w1".to_string()),
            workspace_name: Some("Acme".to_string()),
        };
        assert!(apply_resolved_identity(&mut accounts, "id-1", &identity));
        assert!(!apply_resolved_identity(&mut accounts, "id-1", &identity));
    }

    #[test]
    fn apply_resolved_identity_ignores_unknown_account_id() {
        let mut accounts = vec![account("id-1", "ntn_1")];
        let identity = Identity::Bot {
            workspace_id: None,
            workspace_name: Some("Acme".to_string()),
        };
        assert!(!apply_resolved_identity(
            &mut accounts,
            "id-other",
            &identity
        ));
        assert_eq!(accounts[0].kind, None);
    }
}
