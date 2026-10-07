//! GitHub 커넥터(v1) — 로컬 폴링 + 다중 계정 PAT(docs/08-connectors.md, 참조 구현
//! `capture/slack.rs`와 최대한 동일한 구조를 따른다). Slack과의 핵심 차이는 **다중 계정**과
//! **두 API를 함께 쓰는 이중 파이프라인**이다:
//! - **Events(전방 증분)**: `GET /users/{username}/events` — 계정별 `pollMinutes` 간격으로 최근
//!   활동(최대 90일 보관, GitHub 자체 제약)을 가져온다. 응답이 최신순이라 커서보다 오래된 이벤트를
//!   만나면 그 페이지에서 페이지네이션을 멈춘다([`filter_new_events`]).
//! - **Search 백필(후방)**: `GET /search/issues` + `GET /search/commits` — Events API가 보관하지
//!   않는 과거 이력을 커밋/PR/이슈**생성**만 메꾼다(코멘트·리뷰는 백필 불가, docs 명시). Search API는
//!   쿼리당 최대 1,000건만 반환하므로 [`SEARCH_SLICE_MONTHS`] 단위로 최신→과거 기간을 슬라이스하며
//!   내려가고, 연속 [`EMPTY_SLICE_STREAK_TO_STOP`]회 빈 슬라이스를 만나면 바닥(계정 생성 즈음)에
//!   도달했다고 보아 완료 처리한다.
//! - **계정별 커서**: `capture_cursors`(source=`"github"`)에 계정마다 3개의 `resource` 행을 둔다 —
//!   [`events_cursor_resource`](전방, offset=마지막 처리 이벤트 ts ms), [`search_backfill_cursor_resource`]
//!   (후방, offset=지금까지 내려간 슬라이스 경계 날짜 ms), [`search_empty_streak_resource`](후방 완료
//!   판정용 연속 빈 슬라이스 횟수 — `mtime`을 완료 플래그로 재사용하는 Slack과 달리 카운터가 필요해
//!   별도 행으로 둔다, 스키마/테이블 추가 없이 기존 `offset` 컬럼을 재사용).
//! - **커밋 dedup 자연 합류**: Events의 `PushEvent` 커밋과 Search `/search/commits` 결과 모두
//!   `externalId = "gh:<sha>"`를 쓴다 — 같은 커밋이 두 경로 모두에서 발견돼도 `(source, external_id)`
//!   UNIQUE 제약으로 자연히 하나만 남는다(Slack 이중 커서의 idempotent 합류와 같은 원리). PR/이슈
//!   생성은 이 정도로 정확히 합류하지 않아(Events는 이벤트별 id, Search는 `gh:<repo>#<number>:created`)
//!   커넥터 활성화 직후의 아주 좁은 시간창에서 드물게 중복 1건이 남을 수 있다(수동 확인 포인트 참고).
//!
//! 정규화·매핑·커서 전진·슬라이싱·백오프 같은 순수함수는 네트워크/DB 없이 단위테스트한다. 네트워크
//! 호출·폴링 루프(`poll_account_once`/`spawn`)는 이 파일의 테스트 대상이 아니다(수동 확인 필요 —
//! 완료 보고의 "수동 확인 포인트" 참고).

use crate::capture::config::{self, GithubAccount};
use crate::capture::scrub;
use crate::db;
use crate::model::{EventInput, IngestRequest, StreamInput};
use chrono::{DateTime, Months, NaiveDate, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const SOURCE: &str = "github";

const GITHUB_API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = "LogRoom";
const EVENTS_URL_TEMPLATE: &str = "https://api.github.com/users/{username}/events";
const SEARCH_ISSUES_URL: &str = "https://api.github.com/search/issues";
const SEARCH_COMMITS_URL: &str = "https://api.github.com/search/commits";
/// `/search/commits`는 preview Accept 헤더가 필요하다(GitHub 문서 "Accept 헤더 주의").
const SEARCH_COMMITS_ACCEPT: &str = "application/vnd.github.cloak-preview+json";
const SEARCH_ISSUES_ACCEPT: &str = "application/vnd.github+json";

/// `GET /users/{username}/events` 한 페이지 크기(최대값).
const EVENTS_PAGE_SIZE: u32 = 100;
/// 한 사이클에 순회할 이벤트 페이지 상한. Events API는 **총 300건**까지만 페이지네이션을
/// 허용하므로 per_page=100 기준 3페이지가 정확한 최대치다 — 초과 요청은
/// HTTP 422("pagination is limited for this resource")로 거부된다(실측 2026-07).
const MAX_EVENT_PAGES: u32 = 3;
/// Search API 한 페이지 크기(최대값).
const SEARCH_PAGE_SIZE: u32 = 100;
/// Search API는 쿼리당 최대 1,000건(10페이지)만 반환한다 — 그 이상은 페이지네이션으로 가져올 수
/// 없어 기간 슬라이싱([`SEARCH_SLICE_MONTHS`])으로 우회한다.
const MAX_SEARCH_PAGES_PER_QUERY: u32 = 10;
/// Search 백필 기간 슬라이스 폭(개월). 최신→과거로 이 폭만큼씩 내려간다.
const SEARCH_SLICE_MONTHS: u32 = 3;
/// 연속으로 이 횟수만큼 빈 슬라이스(이슈+커밋 검색 결과 모두 0건)를 만나면 바닥(계정 생성 즈음)에
/// 도달했다고 보아 백필을 완료 처리한다.
const EMPTY_SLICE_STREAK_TO_STOP: u32 = 3;
/// Search API 자체 rate limit(분당 30회, docs/08-connectors.md)을 존중하기 위한 요청 간 최소 간격(초).
const SEARCH_REQUEST_INTERVAL_SECS: u64 = 2;
/// 429/403(secondary rate limit) 연속 실패 시 백오프 상한(초, Slack과 동일 정책).
const MAX_BACKOFF_SECS: u64 = 5 * 60;
/// 429/403이 `Retry-After` 헤더를 안 주는 드문 경우의 초기 백오프(초).
const INITIAL_BACKOFF_SECS: u64 = 5;
/// 백필이 미완료인 동안 다음 사이클까지 대기하는 짧은 간격(초, Slack "따라잡기 가속"과 동일 정책).
const CATCHUP_INTERVAL_SECS: u64 = 10;

type Db = Arc<Mutex<rusqlite::Connection>>;

/// `github:<owner/repo>` 형태의 stream id.
pub fn stream_id(repo_full_name: &str) -> String {
    format!("{SOURCE}:{repo_full_name}")
}

/// 계정별 Events 전방 증분 커서(`capture_cursors.resource`): 지금까지 처리한 최신 이벤트 ts(ms).
fn events_cursor_resource(username: &str) -> String {
    format!("events:{username}:newest")
}

/// 계정별 Search 백필 커서(`capture_cursors.resource`): 지금까지 내려간 슬라이스 경계 날짜(ms,
/// 해당 날짜 00:00 UTC). 다음 사이클의 슬라이스 끝점으로 재사용된다([`next_search_slice`]).
fn search_backfill_cursor_resource(username: &str) -> String {
    format!("search:{username}:backfill")
}

/// 계정별 Search 백필 완료 판정용 연속 빈 슬라이스 카운터(`capture_cursors.offset`을 카운터로
/// 재사용 — Slack처럼 `mtime`을 불리언 플래그로 쓰는 대신, 스트릭 값 자체가 필요해 별도 행을 둔다).
fn search_empty_streak_resource(username: &str) -> String {
    format!("search:{username}:empty_streak")
}

/// RFC3339 timestamp(`"2021-01-01T12:00:00Z"`) → epoch ms. 파싱 실패 시 `None`(호출부가 해당
/// 이벤트/항목을 skip).
fn parse_github_timestamp(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).timestamp_millis())
}

fn ms_to_utc_date(ms: i64) -> NaiveDate {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.date_naive())
        .unwrap_or_else(|| Utc::now().date_naive())
}

fn date_to_ms(date: NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0)
        .and_then(|dt| dt.and_utc().timestamp_millis().into())
        .unwrap_or(0)
}

/// 커밋 html url 조립: `https://github.com/<repo>/commit/<sha>`.
fn commit_html_url(repo_full_name: &str, sha: &str) -> String {
    format!("https://github.com/{repo_full_name}/commit/{sha}")
}

/// Search 응답의 `repository_url`(`"https://api.github.com/repos/owner/repo"`)에서 `owner/repo`를
/// 뽑는다. 형식이 예상과 다르면 `None`(호출부가 해당 항목을 skip).
fn repo_full_name_from_repository_url(url: &str) -> Option<String> {
    const MARKER: &str = "/repos/";
    let idx = url.find(MARKER)?;
    let rest = &url[idx + MARKER.len()..];
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

fn base_event_fields(event: &Value) -> Option<(&str, &str, i64)> {
    let repo = event.get("repo")?.get("name")?.as_str()?;
    let event_id = event.get("id")?.as_str()?;
    let ts = event.get("created_at")?.as_str().and_then(parse_github_timestamp)?;
    Some((repo, event_id, ts))
}

fn new_message_event(
    repo: &str,
    external_id: String,
    ts: i64,
    title: Option<String>,
    body: Option<String>,
    url: Option<String>,
) -> EventInput {
    EventInput {
        external_id,
        stream_id: stream_id(repo),
        ts,
        source: SOURCE.to_string(),
        event_type: "message".to_string(),
        title,
        body,
        model: None,
        tokens_in: None,
        tokens_out: None,
        url,
        parent_id: None,
        metadata: None,
    }
}

/// PushEvent → 커밋별 이벤트(그 외 타입은 이 함수를 타지 않음). `externalId = "gh:<sha>"`라 같은
/// 커밋이 여러 번(force-push 등) 나타나거나 Search 백필과 겹쳐도 idempotent하다.
fn map_push_event(event: &Value) -> Vec<EventInput> {
    let Some((repo, _event_id, ts)) = base_event_fields(event) else {
        return Vec::new();
    };
    let Some(commits) = event.pointer("/payload/commits").and_then(Value::as_array) else {
        return Vec::new();
    };
    commits
        .iter()
        .filter_map(|commit| {
            let sha = commit.get("sha").and_then(Value::as_str)?;
            let message = commit.get("message").and_then(Value::as_str).unwrap_or_default();
            let title = message.lines().next().unwrap_or_default().to_string();
            Some(new_message_event(
                repo,
                format!("gh:{sha}"),
                ts,
                Some(title),
                Some(message.to_string()),
                Some(commit_html_url(repo, sha)),
            ))
        })
        .collect()
}

/// PullRequestEvent(opened/closed[merged]/reopened만 — 그 외 action은 skip).
/// `"<접두어>: <제목>"` 형태의 이벤트 title — 제목이 비면 접미 콜론 없이 접두어만 반환한다.
/// Events API가 PR/이슈 title을 비워 보내는 케이스가 실측됐고(`"PR #27 opened: "` 같은 잘린 제목),
/// 다이제스트/상세 행에서 그대로 노출돼 가독성을 해쳤다.
fn labeled_title(prefix: String, title: &str) -> String {
    if title.is_empty() {
        prefix
    } else {
        format!("{prefix}: {title}")
    }
}

fn map_pull_request_event(event: &Value) -> Option<EventInput> {
    let (repo, event_id, ts) = base_event_fields(event)?;
    let action = event.pointer("/payload/action")?.as_str()?;
    if !matches!(action, "opened" | "closed" | "reopened") {
        return None;
    }
    let number = event.pointer("/payload/number")?.as_i64()?;
    let pr = event.pointer("/payload/pull_request")?;
    let title = pr.get("title").and_then(Value::as_str).unwrap_or_default();
    let merged = pr.get("merged").and_then(Value::as_bool).unwrap_or(false);
    let url = pr.get("html_url").and_then(Value::as_str).map(str::to_string);
    let action_label = if action == "closed" && merged { "merged" } else { action };
    Some(new_message_event(
        repo,
        format!("gh:{event_id}"),
        ts,
        Some(labeled_title(format!("PR #{number} {action_label}"), title)),
        None,
        url,
    ))
}

/// IssuesEvent(opened/closed만 — 그 외 action은 skip).
fn map_issues_event(event: &Value) -> Option<EventInput> {
    let (repo, event_id, ts) = base_event_fields(event)?;
    let action = event.pointer("/payload/action")?.as_str()?;
    if !matches!(action, "opened" | "closed") {
        return None;
    }
    let issue = event.pointer("/payload/issue")?;
    let number = issue.get("number")?.as_i64()?;
    let title = issue.get("title").and_then(Value::as_str).unwrap_or_default();
    let url = issue.get("html_url").and_then(Value::as_str).map(str::to_string);
    Some(new_message_event(
        repo,
        format!("gh:{event_id}"),
        ts,
        Some(labeled_title(format!("Issue #{number} {action}"), title)),
        None,
        url,
    ))
}

/// PullRequestReviewEvent → 리뷰 코멘트(`payload.review`).
fn map_pull_request_review_event(event: &Value) -> Option<EventInput> {
    let (repo, event_id, ts) = base_event_fields(event)?;
    let review = event.pointer("/payload/review")?;
    let body = review
        .get("body")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let url = review.get("html_url").and_then(Value::as_str).map(str::to_string);
    let target = event
        .pointer("/payload/pull_request/title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(new_message_event(
        repo,
        format!("gh:{event_id}"),
        ts,
        Some(labeled_title("코멘트/리뷰".to_string(), target)),
        body,
        url,
    ))
}

/// PullRequestReviewCommentEvent/IssueCommentEvent 공용(`payload.comment` + 대상 title 포인터만 다름).
fn map_comment_event(event: &Value, target_title_pointer: &str) -> Option<EventInput> {
    let (repo, event_id, ts) = base_event_fields(event)?;
    let comment = event.pointer("/payload/comment")?;
    let body = comment
        .get("body")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let url = comment.get("html_url").and_then(Value::as_str).map(str::to_string);
    let target = event.pointer(target_title_pointer).and_then(Value::as_str).unwrap_or_default();
    Some(new_message_event(
        repo,
        format!("gh:{event_id}"),
        ts,
        Some(labeled_title("코멘트/리뷰".to_string(), target)),
        body,
        url,
    ))
}

fn map_pull_request_review_comment_event(event: &Value) -> Option<EventInput> {
    map_comment_event(event, "/payload/pull_request/title")
}

fn map_issue_comment_event(event: &Value) -> Option<EventInput> {
    map_comment_event(event, "/payload/issue/title")
}

/// CreateEvent(branch/tag만 — repository 생성은 "매핑" 대상 밖).
fn map_create_event(event: &Value) -> Option<EventInput> {
    let (repo, event_id, ts) = base_event_fields(event)?;
    let ref_type = event.pointer("/payload/ref_type")?.as_str()?;
    if !matches!(ref_type, "branch" | "tag") {
        return None;
    }
    let git_ref = event.pointer("/payload/ref").and_then(Value::as_str).unwrap_or_default();
    Some(new_message_event(
        repo,
        format!("gh:{event_id}"),
        ts,
        Some(labeled_title(format!("{ref_type} 생성"), git_ref)),
        None,
        None,
    ))
}

/// GitHub Events API 이벤트 1건(JSON) → `EventInput` 목록(PushEvent는 커밋 수만큼, 그 외는 0~1건).
/// 지원하지 않는 타입/필수 필드 누락은 조용히 skip(방어적 — 실측 전 문서 근거로만 작성됐으므로
/// 예상 밖 shape도 크래시 없이 넘어간다).
fn normalize_github_event(event: &Value) -> Vec<EventInput> {
    match event.get("type").and_then(Value::as_str) {
        Some("PushEvent") => map_push_event(event),
        Some("PullRequestEvent") => map_pull_request_event(event).into_iter().collect(),
        Some("IssuesEvent") => map_issues_event(event).into_iter().collect(),
        Some("PullRequestReviewEvent") => map_pull_request_review_event(event).into_iter().collect(),
        Some("PullRequestReviewCommentEvent") => {
            map_pull_request_review_comment_event(event).into_iter().collect()
        }
        Some("IssueCommentEvent") => map_issue_comment_event(event).into_iter().collect(),
        Some("CreateEvent") => map_create_event(event).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// `EventInput`의 `stream_id`(`"github:<repo>"`)에서 repo full name을 되돌린다(그룹핑 키 산출용).
fn repo_of(event: &EventInput) -> String {
    event
        .stream_id
        .strip_prefix(&format!("{SOURCE}:"))
        .unwrap_or(&event.stream_id)
        .to_string()
}

/// repo별로 그룹핑된 `EventInput` 목록 → `IngestRequest` 목록(채널당 1건 그룹핑하는
/// `capture/slack.rs::normalize_search_results`와 동일 계약 — `db::ingest()`가 요청 1건당 하나의
/// `stream` upsert를 전제하기 때문). 이벤트는 repo 내 ts 오름차순으로 정렬한다. `project`는 repo full
/// name 그대로 쓴다(로컬 프로젝트 매칭은 후속 — docs/08-connectors.md).
fn finalize_groups(events: Vec<EventInput>) -> Vec<IngestRequest> {
    let mut groups: BTreeMap<String, Vec<EventInput>> = BTreeMap::new();
    for event in events {
        groups.entry(repo_of(&event)).or_default().push(event);
    }
    groups
        .into_iter()
        .map(|(repo, mut events)| {
            events.sort_by_key(|e| e.ts);
            let stream = StreamInput {
                id: stream_id(&repo),
                source: SOURCE.to_string(),
                kind: Some("session".to_string()),
                title: Some(repo.clone()),
                project: Some(repo),
                git_branch: None,
                started_at: None,
                ended_at: None,
                status: None,
                metadata: None,
            };
            IngestRequest { stream: Some(stream), events }
        })
        .collect()
}

/// `GET /users/{username}/events` 응답(JSON 배열) → repo별로 그룹핑된 `IngestRequest` 목록.
pub fn normalize_events(events: &[Value]) -> Vec<IngestRequest> {
    let mapped: Vec<EventInput> = events.iter().flat_map(normalize_github_event).collect();
    finalize_groups(mapped)
}

/// `IngestRequest` 목록 전체에서 가장 최근 이벤트 ts(ms). 전방 증분 커서 전진에 사용(없으면 `None`).
pub fn max_event_ts(requests: &[IngestRequest]) -> Option<i64> {
    requests.iter().flat_map(|r| r.events.iter()).map(|e| e.ts).max()
}

/// 이번 사이클에서 새로 발견된 이벤트만 남긴다(최신순 페이지 가정). `cursor_ts_ms`보다 오래되거나
/// 같은 이벤트를 만나면 그 지점에서 멈춘다(반환값 `bool` = `true`) — 그 이벤트와 이후(더 오래된)
/// 이벤트는 결과에서 제외된다. `cursor_ts_ms`가 `None`이면(첫 실행) 페이지 전체를 그대로 채택한다.
/// ts를 파싱할 수 없는 이벤트는 보수적으로 포함시킨다(정규화 단계에서 각 매핑 함수가 자체적으로
/// 다시 검증하므로 여기서 드롭하지 않아도 안전하다).
pub fn filter_new_events(events: &[Value], cursor_ts_ms: Option<i64>) -> (Vec<Value>, bool) {
    let mut kept = Vec::with_capacity(events.len());
    for event in events {
        let ts = event.get("created_at").and_then(Value::as_str).and_then(parse_github_timestamp);
        if let (Some(ts), Some(cursor)) = (ts, cursor_ts_ms) {
            if ts <= cursor {
                return (kept, true);
            }
        }
        kept.push(event.clone());
    }
    (kept, false)
}

/// Search 백필의 다음 기간 슬라이스 `[start, end]`(둘 다 포함, GitHub 날짜 range 쿼리 문법과 동일).
/// `floor_cursor_date`(지금까지 내려간 슬라이스 경계 날짜)가 있으면 그 날짜를 다음 슬라이스의 끝점
/// (`end`)으로 재사용해 하루를 의도적으로 겹치게 한다(day 경계에서의 누락을 피하기 위함 — 겹치는
/// 하루의 중복 결과는 idempotent해 무해하다, Slack 이중 커서의 day-margin과 같은 원리). 없으면(첫
/// 배치) `today`를 끝점으로 삼는다. `start`는 `end`에서 [`SEARCH_SLICE_MONTHS`]만큼 뺀 날짜.
pub fn next_search_slice(floor_cursor_date: Option<NaiveDate>, today: NaiveDate) -> (NaiveDate, NaiveDate) {
    let end = floor_cursor_date.unwrap_or(today);
    let start = end
        .checked_sub_months(Months::new(SEARCH_SLICE_MONTHS))
        .unwrap_or(end);
    (start, end)
}

/// `search/issues` 쿼리(PR+이슈): `author:<username> created:<start>..<end>`.
pub fn build_issue_search_query(username: &str, start: NaiveDate, end: NaiveDate) -> String {
    format!(
        "author:{username} created:{}..{}",
        start.format("%Y-%m-%d"),
        end.format("%Y-%m-%d")
    )
}

/// `search/commits` 쿼리: `author:<username> committer-date:<start>..<end>`.
pub fn build_commit_search_query(username: &str, start: NaiveDate, end: NaiveDate) -> String {
    format!(
        "author:{username} committer-date:{}..{}",
        start.format("%Y-%m-%d"),
        end.format("%Y-%m-%d")
    )
}

/// 연속 빈 슬라이스 스트릭 전진: 이번 슬라이스에 결과가 있었으면 0으로 리셋, 없었으면 1 증가.
pub fn advance_empty_streak(previous_streak: u32, slice_had_results: bool) -> u32 {
    if slice_had_results {
        0
    } else {
        previous_streak.saturating_add(1)
    }
}

/// 연속 빈 슬라이스 스트릭이 상한([`EMPTY_SLICE_STREAK_TO_STOP`])에 도달했는지 — 도달하면 바닥
/// (계정 생성 즈음)에 닿았다고 보아 백필을 완료 처리한다.
pub fn backfill_reached_floor(streak: u32) -> bool {
    streak >= EMPTY_SLICE_STREAK_TO_STOP
}

/// 429/403(secondary rate limit) 연속 실패 시 다음 백오프 대기시간(초). Slack
/// (`capture/slack.rs::next_backoff_secs`)과 동일한 계산 — 커넥터마다 자체 rate limit 특성이 달라
/// 상수만 로컬로 두고 로직은 독립적으로 유지한다(모듈 간 결합 최소화).
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

/// `search/issues` 항목 1건(PR 또는 이슈) → "생성" 이벤트. 코멘트/리뷰는 이 API로 알 수 없어 백필
/// 대상이 아니다(docs/08-connectors.md 한계 명시). `externalId = "gh:<repo>#<number>:created"` —
/// Events가 만드는 `gh:<event id>`와 이름공간이 달라 자연 dedup되지는 않는다(모듈 문서 "커밋 dedup
/// 자연 합류" 참고, 드문 중복 가능성은 알려진 한계).
fn map_search_issue_item(item: &Value, username: &str) -> Option<EventInput> {
    // 엄격 검증: 작성자 GitHub 계정(user.login)이 폴링 계정과 일치하는 것만 — search의
    // 느슨한 매칭(이메일 미연결 등)으로 남의 항목이 섞이는 것을 차단(실측 2026-07).
    let login = item.pointer("/user/login").and_then(Value::as_str)?;
    if !login.eq_ignore_ascii_case(username) {
        return None;
    }
    let repo = item
        .get("repository_url")
        .and_then(Value::as_str)
        .and_then(repo_full_name_from_repository_url)?;
    let number = item.get("number")?.as_i64()?;
    let title = item.get("title").and_then(Value::as_str).unwrap_or_default();
    let ts = item.get("created_at")?.as_str().and_then(parse_github_timestamp)?;
    let url = item.get("html_url").and_then(Value::as_str).map(str::to_string);
    let kind_label = if item.get("pull_request").is_some() { "PR" } else { "Issue" };
    Some(new_message_event(
        &repo,
        format!("gh:{repo}#{number}:created"),
        ts,
        Some(format!("{kind_label} #{number} 생성: {title}")),
        None,
        url,
    ))
}

/// `search/commits` 항목 1건 → 커밋 이벤트. `externalId = "gh:<sha>"`로 Events의 PushEvent 커밋과
/// 동일한 이름공간을 써서 자연스럽게 중복을 흡수한다(모듈 문서 참고).
fn map_search_commit_item(item: &Value, username: &str) -> Option<EventInput> {
    // 엄격 검증 1: 커밋의 GitHub 계정(author.login — commit.author.name 아님)이 폴링 계정과
    // 일치해야 한다. author가 null(계정 미연결 커밋)이면 skip — search/commits의 느슨한
    // 매칭으로 남이 fork한 레포의 커밋("Initial commit" 등)이 유입되던 실측 버그 차단.
    let login = item.pointer("/author/login").and_then(Value::as_str)?;
    if !login.eq_ignore_ascii_case(username) {
        return None;
    }
    // 엄격 검증 2: fork 레포의 커밋은 skip — 같은 sha가 원본/fork에 중복 존재해 스트림이
    // 비결정적으로 붙는 문제 방어(fork에서의 내 작업은 PR 머지/Events로 커버, docs/08 한계).
    if item.pointer("/repository/fork").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let repo = item.pointer("/repository/full_name").and_then(Value::as_str)?.to_string();
    let sha = item.get("sha").and_then(Value::as_str)?;
    let message = item.pointer("/commit/message").and_then(Value::as_str).unwrap_or_default();
    let title = message.lines().next().unwrap_or_default().to_string();
    let ts = item
        .pointer("/commit/author/date")
        .or_else(|| item.pointer("/commit/committer/date"))
        .and_then(Value::as_str)
        .and_then(parse_github_timestamp)?;
    let url = item.get("html_url").and_then(Value::as_str).map(str::to_string);
    Some(new_message_event(
        &repo,
        format!("gh:{sha}"),
        ts,
        Some(title),
        Some(message.to_string()),
        url,
    ))
}

/// `search/issues` 응답의 `items[]` → repo별로 그룹핑된 `IngestRequest` 목록.
/// `username` = 폴링 계정 — 작성자 login 불일치 항목은 걸러진다(엄격 검증).
pub fn normalize_search_issues(items: &[Value], username: &str) -> Vec<IngestRequest> {
    finalize_groups(
        items
            .iter()
            .filter_map(|item| map_search_issue_item(item, username))
            .collect(),
    )
}

/// `search/commits` 응답의 `items[]` → repo별로 그룹핑된 `IngestRequest` 목록.
/// `username` = 폴링 계정 — author.login 불일치/미연결/fork 레포 커밋은 걸러진다(엄격 검증).
pub fn normalize_search_commits(items: &[Value], username: &str) -> Vec<IngestRequest> {
    finalize_groups(
        items
            .iter()
            .filter_map(|item| map_search_commit_item(item, username))
            .collect(),
    )
}

/// 계정 목록에서 `token`과 일치하는 계정의 `username`을 갱신한다(폴러가 `/user` 검증에 성공한 뒤
/// 반영 — FE 계정 목록 표시용, docs/08-connectors.md). 변경이 있었으면 `true`(호출부가 이 값을 보고
/// config 저장 여부를 결정 — 파일 IO 자체는 순수함수 밖에서 처리한다).
pub fn apply_resolved_username(accounts: &mut [GithubAccount], token: &str, username: &str) -> bool {
    let mut changed = false;
    for account in accounts.iter_mut() {
        if account.token == token && account.username.as_deref() != Some(username) {
            account.username = Some(username.to_string());
            changed = true;
        }
    }
    changed
}

// ── 네트워크 호출 (단위테스트 대상 아님 — 모듈 문서 "수동 확인 포인트" 참고) ────────────

async fn fetch_user_login(client: &reqwest::Client, token: &str) -> anyhow::Result<String> {
    let resp = client
        .get("https://api.github.com/user")
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("GET /user 실패: HTTP {}", resp.status());
    }
    let body: Value = resp.json().await?;
    body.get("login")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("/user 응답에 login 없음"))
}

async fn fetch_events_page(
    client: &reqwest::Client,
    token: &str,
    username: &str,
    page: u32,
) -> anyhow::Result<Vec<Value>> {
    let url = EVENTS_URL_TEMPLATE.replace("{username}", username);
    let resp = client
        .get(url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .query(&[("per_page", EVENTS_PAGE_SIZE.to_string()), ("page", page.to_string())])
        .send()
        .await?;
    // Events API의 페이지네이션 상한(총 300건) 초과는 422로 거부된다 — 에러가 아니라
    // "더 이상 없음" 신호로 취급해 지금까지 수집한 페이지를 유실하지 않는다(방어).
    if resp.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        return Ok(Vec::new());
    }
    if !resp.status().is_success() {
        // 진단 편의: GitHub 4xx는 본문에 실패 사유(message)를 담아준다 — 앞 200자만 로그에 포함.
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(200).collect();
        anyhow::bail!("GET /users/{username}/events 실패: HTTP {status} — {snippet}");
    }
    let body: Value = resp.json().await?;
    Ok(body.as_array().cloned().unwrap_or_default())
}

/// 계정의 새 이벤트를 가져온다 — 최신순 페이지를 순회하며 [`filter_new_events`]로 커서 이전 이벤트를
/// 만나면 멈춘다.
async fn fetch_new_events(
    client: &reqwest::Client,
    token: &str,
    username: &str,
    cursor_ts_ms: Option<i64>,
) -> anyhow::Result<Vec<Value>> {
    let mut collected = Vec::new();
    for page in 1..=MAX_EVENT_PAGES {
        let events = fetch_events_page(client, token, username, page).await?;
        if events.is_empty() {
            break;
        }
        let page_len = events.len() as u32;
        let (kept, reached_cursor) = filter_new_events(&events, cursor_ts_ms);
        collected.extend(kept);
        if reached_cursor || page_len < EVENTS_PAGE_SIZE {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(collected)
}

enum SearchOutcome {
    Items(Vec<Value>),
    RateLimited(Option<u64>),
}

async fn fetch_search_page(
    client: &reqwest::Client,
    token: &str,
    endpoint: &str,
    query: &str,
    page: u32,
    accept: &str,
) -> anyhow::Result<SearchOutcome> {
    let resp = client
        .get(endpoint)
        .bearer_auth(token)
        .header("Accept", accept)
        .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .query(&[("q", query), ("per_page", &SEARCH_PAGE_SIZE.to_string()), ("page", &page.to_string())])
        .send()
        .await?;

    let status = resp.status();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(SearchOutcome::RateLimited(retry_after));
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        // secondary rate limit은 Retry-After를 동반한다 — 없으면 권한/스코프 문제로 보아 즉시 실패시킨다.
        if retry_after.is_some() {
            return Ok(SearchOutcome::RateLimited(retry_after));
        }
        anyhow::bail!("GitHub search 403(권한/스코프 확인 필요): {endpoint}");
    }
    if !status.is_success() {
        anyhow::bail!("GitHub search 실패: HTTP {status} ({endpoint})");
    }

    let body: Value = resp.json().await?;
    let items = body.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(SearchOutcome::Items(items))
}

async fn fetch_all_search_items(
    client: &reqwest::Client,
    token: &str,
    endpoint: &str,
    query: &str,
    accept: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut all = Vec::new();
    let mut page = 1u32;
    let mut backoff_secs = 0u64;

    loop {
        match fetch_search_page(client, token, endpoint, query, page, accept).await? {
            SearchOutcome::Items(items) => {
                backoff_secs = 0;
                let len = items.len() as u32;
                all.extend(items);
                if len < SEARCH_PAGE_SIZE || page >= MAX_SEARCH_PAGES_PER_QUERY {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(SEARCH_REQUEST_INTERVAL_SECS)).await;
                page += 1;
            }
            SearchOutcome::RateLimited(retry_after) => {
                backoff_secs = next_backoff_secs(backoff_secs, retry_after);
                eprintln!("[logroom] github: search rate limited, {backoff_secs}초 대기 후 재시도");
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                if backoff_secs >= MAX_BACKOFF_SECS {
                    anyhow::bail!("github: search 백오프 상한 도달, 이번 주기 포기");
                }
            }
        }
    }

    Ok(all)
}

fn persist_resolved_username(token: &str, username: &str) {
    // 계정별 태스크가 동시에 부르므로 읽기~저장을 직렬화한다(`config::update_config` 참고).
    if let Err(e) = config::update_config(|cfg| {
        apply_resolved_username(&mut cfg.github.accounts, token, username)
    }) {
        eprintln!("[logroom] github: username 저장 실패({e})");
    }
}

/// 계정 1개의 폴링 1주기: **일시정지 체크(최상단, 조기 반환)** → Events 전방 증분(매 사이클) →
/// Search 백필(미완료인 동안만, 슬라이스 1개) → (일시정지 아니면) 스크럽+ingest → 커서 전진.
/// 반환값(`bool`)은 이번 사이클 종료 시점의 백필 완료 여부 — `spawn`의 루프가 `false`면
/// [`CATCHUP_INTERVAL_SECS`]로, `true`면 평소 간격(`pollMinutes`)으로 다음 사이클을 돈다(Slack과
/// 동일 정책). 일시정지로 조기 반환하는 경우는 `true`를 돌려준다(원인 불명 에러 시 폴백과 동일 근거).
async fn poll_account_once(
    app: &AppHandle,
    db: &Db,
    capture_paused: &Arc<AtomicBool>,
    client: &reqwest::Client,
    token: &str,
    username: &str,
    scrub_secrets: bool,
) -> anyhow::Result<bool> {
    if capture_paused.load(Ordering::Relaxed) {
        return Ok(true);
    }

    let events_resource = events_cursor_resource(username);
    let cursor_ts_ms = {
        let conn = db.lock().expect("db mutex poisoned");
        db::get_cursor(&conn, SOURCE, &events_resource)?.map(|(offset, _)| offset)
    };
    let raw_events = fetch_new_events(client, token, username, cursor_ts_ms).await?;
    let mut requests = normalize_events(&raw_events);
    let new_events_ts = max_event_ts(&requests).or(cursor_ts_ms);

    let backfill_resource = search_backfill_cursor_resource(username);
    let streak_resource = search_empty_streak_resource(username);
    let (backfill_floor_ts, streak) = {
        let conn = db.lock().expect("db mutex poisoned");
        let floor = db::get_cursor(&conn, SOURCE, &backfill_resource)?.map(|(offset, _)| offset);
        let streak = db::get_cursor(&conn, SOURCE, &streak_resource)?
            .map(|(offset, _)| offset.max(0) as u32)
            .unwrap_or(0);
        (floor, streak)
    };
    let already_done = backfill_reached_floor(streak);
    let mut new_streak = streak;
    let mut new_backfill_floor_ts = backfill_floor_ts;

    if !already_done {
        let today = Utc::now().date_naive();
        let floor_date = backfill_floor_ts.map(ms_to_utc_date);
        let (slice_start, slice_end) = next_search_slice(floor_date, today);

        let issue_query = build_issue_search_query(username, slice_start, slice_end);
        let issue_items =
            fetch_all_search_items(client, token, SEARCH_ISSUES_URL, &issue_query, SEARCH_ISSUES_ACCEPT).await?;
        tokio::time::sleep(Duration::from_secs(SEARCH_REQUEST_INTERVAL_SECS)).await;
        let commit_query = build_commit_search_query(username, slice_start, slice_end);
        let commit_items =
            fetch_all_search_items(client, token, SEARCH_COMMITS_URL, &commit_query, SEARCH_COMMITS_ACCEPT).await?;

        let had_results = !issue_items.is_empty() || !commit_items.is_empty();
        requests.extend(normalize_search_issues(&issue_items, username));
        requests.extend(normalize_search_commits(&commit_items, username));

        new_streak = advance_empty_streak(streak, had_results);
        new_backfill_floor_ts = Some(date_to_ms(slice_start));
    }
    let backfill_done_now = backfill_reached_floor(new_streak);

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

    let conn = db.lock().expect("db mutex poisoned");
    db::upsert_cursor(&conn, SOURCE, &events_resource, new_events_ts.unwrap_or(0), None)?;
    if !already_done {
        db::upsert_cursor(&conn, SOURCE, &backfill_resource, new_backfill_floor_ts.unwrap_or(0), None)?;
        db::upsert_cursor(&conn, SOURCE, &streak_resource, i64::from(new_streak), None)?;
    }

    Ok(backfill_done_now)
}

/// 앱 시작 시 계정별 GitHub 폴러를 백그라운드 태스크로 띄운다(`enabled=false`거나 계정이 없으면
/// 조용히 종료 — 다른 캡처 소스와 동일 패턴). 계정마다 독립된 태스크·커서를 가지므로 한 계정의
/// 토큰이 무효화돼도 다른 계정에는 영향이 없다(무효 계정은 `/user` 검증 실패 시 로그만 남기고 그
/// 태스크만 종료 — `capture/health.rs`의 헬스는 전체 계정 커서의 최신값을 집계하므로 일부 계정만
/// 실패해도 다른 계정이 살아있으면 stale로 잡히지 않는다, 모듈 문서 참고).
pub fn spawn(app: AppHandle, db: Db, capture_paused: Arc<AtomicBool>) {
    let cfg = config::load_github_config();
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
            eprintln!("[logroom] github: HTTP 클라이언트 생성 실패({e}) — 폴러 정지");
            return;
        }
    };

    for account in cfg.accounts {
        let app = app.clone();
        let db = db.clone();
        let capture_paused = capture_paused.clone();
        let client = client.clone();
        let token = account.token.clone();

        tauri::async_runtime::spawn(async move {
            let username = match fetch_user_login(&client, &token).await {
                Ok(username) => username,
                Err(e) => {
                    eprintln!(
                        "[logroom] github: 계정 인증 실패({e}) — 이 계정은 건너뜁니다(토큰 확인 필요)"
                    );
                    return;
                }
            };
            persist_resolved_username(&token, &username);
            eprintln!("[logroom] github: 폴러 시작(계정={username}, {poll_minutes}분 간격)");

            loop {
                let backfill_done = match poll_account_once(
                    &app,
                    &db,
                    &capture_paused,
                    &client,
                    &token,
                    &username,
                    scrub_secrets,
                )
                .await
                {
                    Ok(done) => done,
                    Err(e) => {
                        eprintln!("[logroom] github({username}): 폴링 실패({e}) — 다음 주기에 재시도");
                        true
                    }
                };

                let sleep_duration = if backfill_done {
                    poll_interval
                } else {
                    eprintln!(
                        "[logroom] github({username}): 백필 미완료(밀린 이력 있음) — {CATCHUP_INTERVAL_SECS}초 뒤 다음 배치"
                    );
                    Duration::from_secs(CATCHUP_INTERVAL_SECS)
                };
                tokio::time::sleep(sleep_duration).await;
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
        assert_eq!(stream_id("octocat/Hello-World"), "github:octocat/Hello-World");
    }

    #[test]
    fn cursor_resource_keys_embed_username() {
        assert_eq!(events_cursor_resource("octocat"), "events:octocat:newest");
        assert_eq!(search_backfill_cursor_resource("octocat"), "search:octocat:backfill");
        assert_eq!(search_empty_streak_resource("octocat"), "search:octocat:empty_streak");
    }

    // ── parse_github_timestamp / repo_full_name_from_repository_url ──────

    #[test]
    fn parse_github_timestamp_parses_rfc3339() {
        assert_eq!(
            parse_github_timestamp("2021-01-01T00:00:00Z"),
            Some(1_609_459_200_000)
        );
    }

    #[test]
    fn parse_github_timestamp_invalid_returns_none() {
        assert_eq!(parse_github_timestamp("not-a-date"), None);
    }

    #[test]
    fn repo_full_name_from_repository_url_extracts_owner_repo() {
        assert_eq!(
            repo_full_name_from_repository_url("https://api.github.com/repos/octocat/Hello-World"),
            Some("octocat/Hello-World".to_string())
        );
    }

    #[test]
    fn repo_full_name_from_repository_url_missing_marker_returns_none() {
        assert_eq!(repo_full_name_from_repository_url("https://example.com/foo"), None);
    }

    #[test]
    fn commit_html_url_assembles_github_commit_link() {
        assert_eq!(
            commit_html_url("octocat/Hello-World", "abc123"),
            "https://github.com/octocat/Hello-World/commit/abc123"
        );
    }

    // ── PushEvent 매핑 ───────────────────────────────────────────

    fn push_event(repo: &str, ts: &str, commits: Vec<Value>) -> Value {
        json!({
            "id": "1",
            "type": "PushEvent",
            "repo": { "name": repo },
            "payload": { "commits": commits },
            "created_at": ts,
        })
    }

    #[test]
    fn map_push_event_creates_one_event_per_commit() {
        let event = push_event(
            "octocat/Hello-World",
            "2021-01-01T00:00:00Z",
            vec![
                json!({ "sha": "sha1", "message": "Fix bug\n\nDetails" }),
                json!({ "sha": "sha2", "message": "Add feature" }),
            ],
        );
        let events = map_push_event(&event);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].external_id, "gh:sha1");
        assert_eq!(events[0].title.as_deref(), Some("Fix bug"));
        assert_eq!(events[0].body.as_deref(), Some("Fix bug\n\nDetails"));
        assert_eq!(events[0].stream_id, "github:octocat/Hello-World");
        assert_eq!(
            events[0].url.as_deref(),
            Some("https://github.com/octocat/Hello-World/commit/sha1")
        );
        assert_eq!(events[1].external_id, "gh:sha2");
        assert_eq!(events[1].title.as_deref(), Some("Add feature"));
    }

    #[test]
    fn map_push_event_missing_commits_returns_empty() {
        let event = json!({
            "id": "1",
            "type": "PushEvent",
            "repo": { "name": "octocat/Hello-World" },
            "payload": {},
            "created_at": "2021-01-01T00:00:00Z",
        });
        assert!(map_push_event(&event).is_empty());
    }

    // ── PullRequestEvent 매핑 ────────────────────────────────────

    fn pr_event(action: &str, merged: bool) -> Value {
        json!({
            "id": "10",
            "type": "PullRequestEvent",
            "repo": { "name": "octocat/Hello-World" },
            "payload": {
                "action": action,
                "number": 42,
                "pull_request": {
                    "title": "Add feature",
                    "html_url": "https://github.com/octocat/Hello-World/pull/42",
                    "merged": merged,
                }
            },
            "created_at": "2021-01-02T00:00:00Z",
        })
    }

    #[test]
    fn map_pull_request_event_opened() {
        let event = pr_event("opened", false);
        let mapped = map_pull_request_event(&event).expect("매핑돼야 함");
        assert_eq!(mapped.external_id, "gh:10");
        assert_eq!(mapped.title.as_deref(), Some("PR #42 opened: Add feature"));
        assert_eq!(mapped.url.as_deref(), Some("https://github.com/octocat/Hello-World/pull/42"));
    }

    #[test]
    fn map_pull_request_event_closed_and_merged_uses_merged_label() {
        let event = pr_event("closed", true);
        let mapped = map_pull_request_event(&event).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("PR #42 merged: Add feature"));
    }

    #[test]
    fn map_pull_request_event_closed_without_merge_keeps_closed_label() {
        let event = pr_event("closed", false);
        let mapped = map_pull_request_event(&event).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("PR #42 closed: Add feature"));
    }

    #[test]
    fn map_pull_request_event_reopened() {
        let event = pr_event("reopened", false);
        let mapped = map_pull_request_event(&event).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("PR #42 reopened: Add feature"));
    }

    #[test]
    fn map_pull_request_event_skips_unsupported_action() {
        let event = pr_event("labeled", false);
        assert!(map_pull_request_event(&event).is_none());
    }

    #[test]
    fn map_pull_request_event_empty_title_omits_trailing_colon() {
        // Events API가 title을 비워 보내는 케이스 실측 — "PR #42 opened: "로 잘리지 않아야 한다.
        let mut event = pr_event("opened", false);
        event["payload"]["pull_request"]["title"] = json!("");
        let mapped = map_pull_request_event(&event).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("PR #42 opened"));
    }

    #[test]
    fn labeled_title_joins_or_returns_prefix_only() {
        assert_eq!(labeled_title("PR #1 opened".to_string(), "제목"), "PR #1 opened: 제목");
        assert_eq!(labeled_title("코멘트/리뷰".to_string(), ""), "코멘트/리뷰");
    }

    // ── IssuesEvent 매핑 ─────────────────────────────────────────

    fn issue_event(action: &str) -> Value {
        json!({
            "id": "20",
            "type": "IssuesEvent",
            "repo": { "name": "octocat/Hello-World" },
            "payload": {
                "action": action,
                "issue": {
                    "number": 7,
                    "title": "Bug report",
                    "html_url": "https://github.com/octocat/Hello-World/issues/7",
                }
            },
            "created_at": "2021-01-03T00:00:00Z",
        })
    }

    #[test]
    fn map_issues_event_opened() {
        let mapped = map_issues_event(&issue_event("opened")).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("Issue #7 opened: Bug report"));
        assert_eq!(mapped.external_id, "gh:20");
    }

    #[test]
    fn map_issues_event_closed() {
        let mapped = map_issues_event(&issue_event("closed")).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("Issue #7 closed: Bug report"));
    }

    #[test]
    fn map_issues_event_skips_unsupported_action() {
        assert!(map_issues_event(&issue_event("assigned")).is_none());
    }

    // ── 코멘트/리뷰 매핑 ──────────────────────────────────────────

    #[test]
    fn map_pull_request_review_event_maps_body_and_url() {
        let event = json!({
            "id": "30",
            "type": "PullRequestReviewEvent",
            "repo": { "name": "octocat/Hello-World" },
            "payload": {
                "review": { "body": "LGTM", "html_url": "https://github.com/x/y/pull/1#review-1" },
                "pull_request": { "title": "Add feature" },
            },
            "created_at": "2021-01-04T00:00:00Z",
        });
        let mapped = map_pull_request_review_event(&event).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("코멘트/리뷰: Add feature"));
        assert_eq!(mapped.body.as_deref(), Some("LGTM"));
        assert_eq!(mapped.external_id, "gh:30");
    }

    #[test]
    fn map_pull_request_review_comment_event_uses_pull_request_title() {
        let event = json!({
            "id": "31",
            "type": "PullRequestReviewCommentEvent",
            "repo": { "name": "octocat/Hello-World" },
            "payload": {
                "comment": { "body": "nit: typo", "html_url": "https://github.com/x/y/pull/1#comment-1" },
                "pull_request": { "title": "Add feature" },
            },
            "created_at": "2021-01-04T00:00:00Z",
        });
        let mapped = map_pull_request_review_comment_event(&event).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("코멘트/리뷰: Add feature"));
        assert_eq!(mapped.body.as_deref(), Some("nit: typo"));
    }

    #[test]
    fn map_issue_comment_event_uses_issue_title() {
        let event = json!({
            "id": "32",
            "type": "IssueCommentEvent",
            "repo": { "name": "octocat/Hello-World" },
            "payload": {
                "comment": { "body": "me too", "html_url": "https://github.com/x/y/issues/7#comment-1" },
                "issue": { "title": "Bug report" },
            },
            "created_at": "2021-01-04T00:00:00Z",
        });
        let mapped = map_issue_comment_event(&event).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("코멘트/리뷰: Bug report"));
        assert_eq!(mapped.body.as_deref(), Some("me too"));
    }

    // ── CreateEvent 매핑 ─────────────────────────────────────────

    fn create_event(ref_type: &str, git_ref: Option<&str>) -> Value {
        json!({
            "id": "40",
            "type": "CreateEvent",
            "repo": { "name": "octocat/Hello-World" },
            "payload": { "ref_type": ref_type, "ref": git_ref },
            "created_at": "2021-01-05T00:00:00Z",
        })
    }

    #[test]
    fn map_create_event_branch() {
        let mapped = map_create_event(&create_event("branch", Some("feature-x"))).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("branch 생성: feature-x"));
    }

    #[test]
    fn map_create_event_tag() {
        let mapped = map_create_event(&create_event("tag", Some("v1.0"))).expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("tag 생성: v1.0"));
    }

    #[test]
    fn map_create_event_repository_is_skipped() {
        assert!(map_create_event(&create_event("repository", None)).is_none());
    }

    // ── normalize_github_event(디스패치) / normalize_events(그룹핑) ──────

    #[test]
    fn normalize_github_event_skips_unsupported_type() {
        let event = json!({
            "id": "99",
            "type": "ForkEvent",
            "repo": { "name": "octocat/Hello-World" },
            "payload": {},
            "created_at": "2021-01-01T00:00:00Z",
        });
        assert!(normalize_github_event(&event).is_empty());
    }

    #[test]
    fn normalize_events_groups_by_repo_and_sorts_by_ts() {
        let events = vec![
            pr_event("opened", false),
            {
                let mut e = issue_event("opened");
                e["repo"]["name"] = json!("octocat/Other-Repo");
                e["created_at"] = json!("2021-01-01T00:00:00Z");
                e
            },
        ];
        let requests = normalize_events(&events);
        assert_eq!(requests.len(), 2, "repo별로 요청이 분리돼야 함");
        let hello_world = requests
            .iter()
            .find(|r| r.stream.as_ref().unwrap().id == "github:octocat/Hello-World")
            .expect("Hello-World 요청 있어야 함");
        assert_eq!(hello_world.stream.as_ref().unwrap().project.as_deref(), Some("octocat/Hello-World"));
        assert_eq!(hello_world.events.len(), 1);
    }

    #[test]
    fn normalize_events_sorts_events_within_repo_by_ts_ascending() {
        let events = vec![
            push_event("octocat/Hello-World", "2021-01-02T00:00:00Z", vec![json!({"sha": "late", "message": "late"})]),
            push_event("octocat/Hello-World", "2021-01-01T00:00:00Z", vec![json!({"sha": "early", "message": "early"})]),
        ];
        let requests = normalize_events(&events);
        assert_eq!(requests.len(), 1);
        let events = &requests[0].events;
        assert!(events[0].ts <= events[1].ts);
        assert_eq!(events[0].external_id, "gh:early");
    }

    // ── max_event_ts ─────────────────────────────────────────────

    #[test]
    fn max_event_ts_returns_largest_ts_across_requests() {
        let events = vec![
            push_event("a/b", "2021-01-01T00:00:00Z", vec![json!({"sha": "s1", "message": "m"})]),
            push_event("c/d", "2021-01-05T00:00:00Z", vec![json!({"sha": "s2", "message": "m"})]),
        ];
        let requests = normalize_events(&events);
        assert_eq!(max_event_ts(&requests), Some(parse_github_timestamp("2021-01-05T00:00:00Z").unwrap()));
    }

    #[test]
    fn max_event_ts_none_when_no_requests() {
        assert_eq!(max_event_ts(&[]), None);
    }

    // ── filter_new_events ────────────────────────────────────────

    #[test]
    fn filter_new_events_bootstrap_keeps_all() {
        let events = vec![
            json!({ "created_at": "2021-01-02T00:00:00Z" }),
            json!({ "created_at": "2021-01-01T00:00:00Z" }),
        ];
        let (kept, reached) = filter_new_events(&events, None);
        assert_eq!(kept.len(), 2);
        assert!(!reached);
    }

    #[test]
    fn filter_new_events_stops_at_cursor() {
        let cursor = parse_github_timestamp("2021-01-01T12:00:00Z").unwrap();
        let events = vec![
            json!({ "created_at": "2021-01-02T00:00:00Z" }), // newer, keep
            json!({ "created_at": "2021-01-01T00:00:00Z" }), // older than cursor, stop here
            json!({ "created_at": "2020-12-31T00:00:00Z" }), // never reached
        ];
        let (kept, reached) = filter_new_events(&events, Some(cursor));
        assert_eq!(kept.len(), 1);
        assert!(reached);
    }

    #[test]
    fn filter_new_events_equal_to_cursor_is_not_kept() {
        let cursor = parse_github_timestamp("2021-01-01T00:00:00Z").unwrap();
        let events = vec![json!({ "created_at": "2021-01-01T00:00:00Z" })];
        let (kept, reached) = filter_new_events(&events, Some(cursor));
        assert!(kept.is_empty());
        assert!(reached);
    }

    // ── next_search_slice(기간 슬라이싱) ──────────────────────────

    #[test]
    fn next_search_slice_bootstrap_uses_today_as_end() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
        let (start, end) = next_search_slice(None, today);
        assert_eq!(end, today);
        assert_eq!(start, today.checked_sub_months(Months::new(3)).unwrap());
    }

    #[test]
    fn next_search_slice_continues_from_previous_floor_with_one_day_overlap() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
        let floor = NaiveDate::from_ymd_opt(2026, 4, 15).unwrap();
        let (start, end) = next_search_slice(Some(floor), today);
        assert_eq!(end, floor, "겹치는 하루로 이전 슬라이스의 시작일을 끝점으로 재사용");
        assert_eq!(start, floor.checked_sub_months(Months::new(3)).unwrap());
    }

    // ── build_issue_search_query / build_commit_search_query ─────

    #[test]
    fn build_issue_search_query_formats_author_and_created_range() {
        let start = NaiveDate::from_ymd_opt(2026, 4, 15).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
        assert_eq!(
            build_issue_search_query("octocat", start, end),
            "author:octocat created:2026-04-15..2026-07-15"
        );
    }

    #[test]
    fn build_commit_search_query_formats_author_and_committer_date_range() {
        let start = NaiveDate::from_ymd_opt(2026, 4, 15).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
        assert_eq!(
            build_commit_search_query("octocat", start, end),
            "author:octocat committer-date:2026-04-15..2026-07-15"
        );
    }

    // ── advance_empty_streak / backfill_reached_floor ────────────

    #[test]
    fn advance_empty_streak_resets_on_results() {
        assert_eq!(advance_empty_streak(2, true), 0);
    }

    #[test]
    fn advance_empty_streak_increments_when_empty() {
        assert_eq!(advance_empty_streak(2, false), 3);
    }

    #[test]
    fn backfill_reached_floor_true_at_threshold() {
        assert!(backfill_reached_floor(3));
        assert!(backfill_reached_floor(4));
    }

    #[test]
    fn backfill_reached_floor_false_below_threshold() {
        assert!(!backfill_reached_floor(0));
        assert!(!backfill_reached_floor(2));
    }

    // ── next_backoff_secs(429/403 백오프) ─────────────────────────

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
    fn next_backoff_secs_caps_at_max_backoff() {
        assert_eq!(next_backoff_secs(MAX_BACKOFF_SECS, None), MAX_BACKOFF_SECS);
        assert_eq!(next_backoff_secs(1000, Some(10_000)), MAX_BACKOFF_SECS);
    }

    // ── search 백필 매핑(map_search_issue_item / map_search_commit_item) ──

    #[test]
    fn map_search_issue_item_pull_request_uses_pr_label() {
        let item = json!({
            "number": 42,
            "title": "Add feature",
            "created_at": "2021-01-01T00:00:00Z",
            "html_url": "https://github.com/octocat/Hello-World/pull/42",
            "pull_request": { "html_url": "..." },
            "repository_url": "https://api.github.com/repos/octocat/Hello-World",
            "user": { "login": "octocat" },
        });
        let mapped = map_search_issue_item(&item, "octocat").expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("PR #42 생성: Add feature"));
        assert_eq!(mapped.external_id, "gh:octocat/Hello-World#42:created");
        assert_eq!(mapped.stream_id, "github:octocat/Hello-World");
    }

    #[test]
    fn map_search_issue_item_plain_issue_uses_issue_label() {
        let item = json!({
            "number": 7,
            "title": "Bug report",
            "created_at": "2021-01-01T00:00:00Z",
            "html_url": "https://github.com/octocat/Hello-World/issues/7",
            "repository_url": "https://api.github.com/repos/octocat/Hello-World",
            "user": { "login": "OctoCat" },
        });
        // login 비교는 대소문자 무시(GitHub username 규칙).
        let mapped = map_search_issue_item(&item, "octocat").expect("매핑돼야 함");
        assert_eq!(mapped.title.as_deref(), Some("Issue #7 생성: Bug report"));
    }

    #[test]
    fn map_search_issue_item_skips_other_users_and_missing_login() {
        let other = json!({
            "number": 7, "title": "x", "created_at": "2021-01-01T00:00:00Z",
            "repository_url": "https://api.github.com/repos/octocat/Hello-World",
            "user": { "login": "someone-else" },
        });
        assert!(map_search_issue_item(&other, "octocat").is_none());
        let missing = json!({
            "number": 8, "title": "y", "created_at": "2021-01-01T00:00:00Z",
            "repository_url": "https://api.github.com/repos/octocat/Hello-World",
        });
        assert!(map_search_issue_item(&missing, "octocat").is_none());
    }

    #[test]
    fn map_search_commit_item_maps_sha_and_message() {
        let item = json!({
            "sha": "abc123",
            "html_url": "https://github.com/octocat/Hello-World/commit/abc123",
            "author": { "login": "octocat" },
            "commit": {
                "message": "Fix bug\n\nDetails",
                "author": { "date": "2021-01-01T00:00:00Z" },
            },
            "repository": { "full_name": "octocat/Hello-World", "fork": false },
        });
        let mapped = map_search_commit_item(&item, "octocat").expect("매핑돼야 함");
        assert_eq!(mapped.external_id, "gh:abc123");
        assert_eq!(mapped.title.as_deref(), Some("Fix bug"));
        assert_eq!(mapped.body.as_deref(), Some("Fix bug\n\nDetails"));
    }

    #[test]
    fn map_search_commit_item_skips_mismatched_null_author_and_forks() {
        let base = json!({
            "sha": "s",
            "commit": { "message": "m", "author": { "date": "2021-01-01T00:00:00Z" } },
            "repository": { "full_name": "o/r", "fork": false },
        });
        // author.login 불일치 → skip
        let mut mismatch = base.clone();
        mismatch["author"] = json!({ "login": "someone-else" });
        assert!(map_search_commit_item(&mismatch, "octocat").is_none());
        // author null(계정 미연결 커밋) → skip
        assert!(map_search_commit_item(&base, "octocat").is_none());
        // fork 레포 → skip
        let mut forked = base.clone();
        forked["author"] = json!({ "login": "octocat" });
        forked["repository"]["fork"] = json!(true);
        assert!(map_search_commit_item(&forked, "octocat").is_none());
    }

    #[test]
    fn normalize_search_issues_groups_by_repo() {
        let items = vec![
            json!({
                "number": 1, "title": "a", "created_at": "2021-01-01T00:00:00Z",
                "repository_url": "https://api.github.com/repos/octocat/Hello-World",
                "user": { "login": "octocat" },
            }),
            json!({
                "number": 2, "title": "b", "created_at": "2021-01-02T00:00:00Z",
                "repository_url": "https://api.github.com/repos/octocat/Other",
                "user": { "login": "octocat" },
            }),
        ];
        let requests = normalize_search_issues(&items, "octocat");
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn normalize_search_commits_groups_by_repo() {
        let items = vec![json!({
            "sha": "s1",
            "author": { "login": "octocat" },
            "commit": { "message": "m", "author": { "date": "2021-01-01T00:00:00Z" } },
            "repository": { "full_name": "octocat/Hello-World", "fork": false },
        })];
        let requests = normalize_search_commits(&items, "octocat");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].events[0].external_id, "gh:s1");
    }

    // ── apply_resolved_username ───────────────────────────────────

    #[test]
    fn apply_resolved_username_updates_matching_token() {
        let mut accounts = vec![GithubAccount { token: "ghp_a".to_string(), username: None }];
        let changed = apply_resolved_username(&mut accounts, "ghp_a", "octocat");
        assert!(changed);
        assert_eq!(accounts[0].username.as_deref(), Some("octocat"));
    }

    #[test]
    fn apply_resolved_username_no_change_when_already_matching() {
        let mut accounts = vec![GithubAccount {
            token: "ghp_a".to_string(),
            username: Some("octocat".to_string()),
        }];
        let changed = apply_resolved_username(&mut accounts, "ghp_a", "octocat");
        assert!(!changed);
    }

    #[test]
    fn apply_resolved_username_no_match_returns_false() {
        let mut accounts = vec![GithubAccount { token: "ghp_a".to_string(), username: None }];
        let changed = apply_resolved_username(&mut accounts, "ghp_other", "octocat");
        assert!(!changed);
        assert_eq!(accounts[0].username, None);
    }
}
