//! Linear 커넥터(v1) — 로컬 폴링 + 다중 워크스페이스 Personal API Key(docs/08-connectors.md,
//! 참조 구현 `capture/github.rs`의 다중 계정·이중 커서 + `capture/slack.rs`의 이중 커서 백필을
//! 최대한 재사용한다). Linear는 GitHub/Slack의 REST 검색 API와 달리 **GraphQL 단일 엔드포인트**
//! (`https://api.linear.app/graphql`)만 제공한다 — 이 모듈은 REST 페이지네이션(`page`/`per_page`) 대신
//! GraphQL 자체의 커서 페이지네이션(`after`/`pageInfo.hasNextPage`/`pageInfo.endCursor`)을 쓴다.
//!
//! ## v1 스코프
//! ① 내가 생성했거나 배정된 이슈, ② 내가 작성한 코멘트만 캡처한다("회고 중심 저장" 철학,
//! ADR-0012와 동일 정신 — Slack의 "내가 보낸 메시지만"과 대응). **상태 변경(status/assignee 등)
//! 히스토리는 v1 스코프 밖**이다(후속, docs/08-connectors.md 한계 명시) — 다만 이슈 노드에 실려
//! 오는 `startedAt`/`completedAt`/`canceledAt` "지금 상태로의 전이 시각"만은 각각 이벤트 1건으로
//! 함께 기록한다(과거 상태 이력 전체가 아니라 "언제 시작/완료/취소됐는지"만 — 아래 "매핑" 참고).
//!
//! ## 인증 — 워크스페이스(계정) 배열 + `viewer` 검증
//! GitHub와 동일하게 계정(워크스페이스)마다 독립된 API 키·커서·폴러 태스크를 둔다. 부팅 시
//! `query { viewer { id name email } }`로 키를 검증하고 `id`(이후 필터링 키)/`name`(표시명)을 확인한다
//! — 실패하면 그 계정만 폴러를 시작하지 않는다. **인증 헤더는 `Authorization: <key>`(Bearer 아님)**
//! — OAuth access token과 달리 Personal API Key는 접두어 없이 그대로 헤더 값에 넣는다(Linear 문서
//! 근거, 실측 전이라 수동 확인 포인트로 남긴다).
//!
//! ## 이중 커서(전방 증분 + 후방 백필) — 엔티티(issues/comments)마다 독립
//! **cursor 값 자체는 GitHub Events 커서와 동일하게 `updatedAt`(epoch ms)** 로 둔다(`capture_cursors`의
//! `offset` 컬럼이 `i64`라 GraphQL의 불투명(opaque) `endCursor` 문자열을 그대로 영속 저장할 수 없다
//! — 대신 한 번의 폴링 사이클 **안에서** 여러 페이지를 순회할 때만 GraphQL 네이티브 `after` 커서를
//! 쓰고, 사이클 **사이**의 진행 상태는 `updatedAt` 경계값으로 표현한다. 이렇게 하면 GitHub Search
//! API처럼 기간을 3개월 단위로 슬라이스할 필요 없이 — Linear는 쿼리당 결과 상한이 없어 `after`로
//! 계속 더 가져올 수 있으므로 — 임의 깊이까지 자연스럽게 페이지네이션된다).
//! - **전방 증분**(`issues:<viewerId>:newest` / `comments:<viewerId>:newest`): `updatedAt: { gt: <cursor
//!   ISO> }` 필터로 매 사이클 새 항목을 전부 가져와 배치 최댓값으로 전진시킨다(GitHub Events와 동일
//!   정책). 이슈가 "생성"이 아니라 단순 업데이트(상태 변경 등)로 다시 걸려도 `externalId`(`ln:<uuid>`)가
//!   같아 재수집은 idempotent하다(UNIQUE 제약 자연 dedup — v1이 상태변경을 수집하지 않는 이유이자
//!   근거).
//! - **후방 백필**(`issues:<viewerId>:backfill` / `comments:<viewerId>:backfill`, `mtime`을 "완료"
//!   플래그로 재사용 — Slack과 동일 패턴): `updatedAt: { lt: <floor ISO> }` 필터 + 사이클당 페이지
//!   상한([`MAX_PAGES_PER_FETCH`])으로 한 배치씩 내려가고, 배치가 비면 바닥에 도달했다고 보아 완료
//!   마킹한다(Slack `advance_backfill`과 동일 원리).
//! - **첫 실행**(두 커서 모두 없음): `updatedAt` 필터 없이 desc 방향 배치 1회로 newest=배치 최댓값·
//!   backfill=배치 최솟값을 동시에 초기화한다(Slack `init_cursors_from_bootstrap_batch`와 동일).
//! - **따라잡기 가속**: 두 엔티티(issues/comments) 중 하나라도 백필 미완료면 평소 간격 대신
//!   [`CATCHUP_INTERVAL_SECS`]로 다음 사이클을 즉시 돈다(GitHub/Slack과 동일 정책).
//!
//! ## 매핑
//! **스트림 = 이슈**(`linear:<identifier>`, title=`<identifier> <이슈 제목>`), **project = 팀명** —
//! Claude(project=레포, stream=세션)와 같은 결. 팀=스트림으로 하면 팀이 이슈 수백 개를 담는
//! 컨테이너라 하루치가 한 덩어리로 뭉쳐 "오늘 어떤 이슈를 작업했나"가 안 보인다(실사용 피드백).
//! 이벤트 type은 항상 `"message"` — 이슈 생성: title=`<identifier> 생성: <제목>`, body=description
//! 앞부분(essential 정책과 별개로 항상 [`DESCRIPTION_MAX_CHARS`]자 절단). 코멘트: title=
//! `<identifier> 코멘트`, body=코멘트 원문. 코멘트 쿼리도 이슈 `title`을 가져온다 — 남의 이슈에 내가
//! 코멘트만 단 경우(가장 흔한 케이스) 스트림 제목을 코멘트 쪽에서 채워야 하기 때문.
//! `externalId = "ln:<entity uuid>"`, `ts = createdAt`(업데이트 시각이 아니라 생성 시각 — v1이 "내가
//! 한 일"을 생성 시점 기준으로 기록한다는 스코프와 일치).
//!
//! **에픽(Linear 프로젝트)·상태 — 스트림 metadata**: 이슈 쿼리가 함께 가져오는 `project.name`
//! (에픽)·`state.name`/`state.type`(워크플로 상태)은 이벤트가 아니라 스트림(`StreamInput.metadata`)
//! JSON(`{"linearProject":..,"state":..,"stateType":..}`, 값 없는 키는 생략)에 실어 보낸다 — `project`
//! 필드(위 문단의 팀명)와는 다른 층위라서다. 같은 배치에 같은 이슈가 여러 노드로 와도 `issue_title`
//! 과 동일하게 "비어있지 않은 쪽 채택" 규칙으로 합친다([`finalize_groups`]).
//!
//! **전이 이벤트(시작/완료/취소)**: 이슈 노드에 `startedAt`/`completedAt`/`canceledAt`이 있으면
//! "생성" 이벤트와 별개로 각각 이벤트 1건(`title = "<identifier> 시작/완료/취소"`, `ts` = 해당
//! 타임스탬프, `body`/`url`은 생성 이벤트와 동일 규칙)을 추가로 만든다. `externalId`에
//! `#started`/`#completed`/`#canceled` 접미사를 붙여 생성 이벤트와 구분하면서, 매 사이클 재조회돼도
//! UNIQUE(source, external_id) 제약으로 idempotent하게 dedup된다.
//!
//! ## Rate limit
//! Linear Personal API Key는 시간당 1,500회 제한이다(docs/08-connectors.md). `429` 또는 `Retry-After`
//! 헤더는 GitHub/Slack과 동일한 지수 백오프(최대 5분 상한)로 재시도한다.
//!
//! **주의**: 이 모듈의 GraphQL 쿼리 필드/필터 shape(`IssueFilter`/`CommentFilter`의 `creator`/`user`
//! 서브필드, `orderBy` 등)는 Linear 공개 문서 근거로만 작성됐고 실제 API 응답으로 검증되지 않았다
//! (완료 보고의 "수동 확인 포인트" 참고) — GitHub/Slack의 "실측 전 문서 근거" 관례와 동일하게, 예상과
//! 다른 응답 shape을 만나도 각 매핑 함수가 `Option`/빈 배열로 조용히 skip해 크래시 없이 넘어간다.
//!
//! 정규화·커서 전진·쿼리 문자열 조립·백오프 같은 순수함수는 네트워크/DB 없이 단위테스트한다. 네트워크
//! 호출·폴링 루프(`poll_account_once`/`spawn`)는 이 파일의 테스트 대상이 아니다(수동 확인 필요).

use crate::capture::config::{self, LinearAccount};
use crate::capture::policy::truncate_with_ellipsis;
use crate::capture::scrub;
use crate::db;
use crate::model::{EventInput, IngestRequest, StreamInput};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

pub const SOURCE: &str = "linear";

const GRAPHQL_URL: &str = "https://api.linear.app/graphql";
const USER_AGENT: &str = "LogRoom";
/// GraphQL 한 페이지 크기.
const PAGE_SIZE: u32 = 50;
/// 한 번의 fetch(`fetch_all_pages`)에서 순회할 페이지 상한 — 전방 증분/후방 백필/첫 실행 배치
/// 모두 공용으로 쓴다(Slack `MAX_PAGES_PER_CYCLE`과 동일 정책, 사이클당 최대
/// `MAX_PAGES_PER_FETCH * PAGE_SIZE`(500)건).
const MAX_PAGES_PER_FETCH: u32 = 10;
/// 페이지 요청 사이 최소 간격(초) — rate limit 존중(GitHub Search/Slack과 동일 정신).
const PAGE_REQUEST_INTERVAL_SECS: u64 = 1;
/// 429 연속 실패 시 백오프 상한(초, GitHub/Slack과 동일 정책).
const MAX_BACKOFF_SECS: u64 = 5 * 60;
/// 429가 `Retry-After` 헤더를 안 주는 드문 경우의 초기 백오프(초).
const INITIAL_BACKOFF_SECS: u64 = 5;
/// 백필이 미완료인 동안 다음 사이클까지 대기하는 짧은 간격(초, GitHub/Slack "따라잡기 가속"과 동일).
const CATCHUP_INTERVAL_SECS: u64 = 10;
/// 이슈 description 저장 최대 문자(코드포인트) 수 — bodyPolicy(essential/full)와 무관하게 항상
/// 적용되는 고정 절단(docs/08-connectors.md, `capture/policy.rs::truncate_with_ellipsis` 재사용).
const DESCRIPTION_MAX_CHARS: usize = 2000;

type Db = Arc<Mutex<rusqlite::Connection>>;

/// `linear:<issue identifier>` 형태의 stream id(이슈=스트림, 예 `linear:TICKET-838`).
pub fn stream_id(identifier: &str) -> String {
    format!("{SOURCE}:{identifier}")
}

/// 스트림 title: `<identifier> <이슈 제목>`(제목이 비면 identifier만).
fn stream_title(identifier: &str, issue_title: &str) -> String {
    if issue_title.is_empty() {
        identifier.to_string()
    } else {
        format!("{identifier} {issue_title}")
    }
}

fn issues_newest_resource(viewer_id: &str) -> String {
    format!("issues:{viewer_id}:newest")
}

fn issues_backfill_resource(viewer_id: &str) -> String {
    format!("issues:{viewer_id}:backfill")
}

fn comments_newest_resource(viewer_id: &str) -> String {
    format!("comments:{viewer_id}:newest")
}

fn comments_backfill_resource(viewer_id: &str) -> String {
    format!("comments:{viewer_id}:backfill")
}

/// RFC3339 timestamp(`"2021-01-01T12:00:00.000Z"`) → epoch ms. 파싱 실패 시 `None`(호출부가 해당
/// 노드를 skip).
fn parse_linear_timestamp(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).timestamp_millis())
}

/// epoch ms → RFC3339 문자열(밀리초 정밀도, `Z` 접미). GraphQL `updatedAt` 비교 필터(`gt`/`lt`)의
/// 값으로 쓰인다.
fn ms_to_iso(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// 정규화된 이벤트 1건 + 그룹핑에 필요한 이슈/팀 정보(identifier·이슈 제목은 스트림 id·title 산출에,
/// 팀명은 project에 쓰이고 이벤트 자체에는 남지 않아 별도로 들고 다닌다 —
/// `capture/slack.rs::normalize_search_results`가 채널 객체를 이벤트와 함께 들고 다니는 것과 동일한
/// 이유).
struct MappedEvent {
    identifier: String,
    issue_title: String,
    team_name: String,
    /// Linear 프로젝트(에픽) 이름 — 스트림 `metadata.linearProject`로 실려 나간다(모듈 문서 "매핑"
    /// 참고). 이슈 노드에 `project`가 없으면 `None`.
    linear_project: Option<String>,
    /// 워크플로 상태 표시명(`WorkflowState.name`, 예 "In Progress") — `metadata.state`.
    state_name: Option<String>,
    /// 워크플로 상태 타입(`WorkflowState.type`, `triage`/`backlog`/`unstarted`/`started`/`completed`/
    /// `canceled`/`duplicate`) — `metadata.stateType`.
    state_type: Option<String>,
    event: EventInput,
}

/// 이슈 노드(GraphQL `issues.nodes[]` 항목)에서 "생성" 이벤트와 전이 이벤트([`map_issue_transition_events`])
/// 가 공유하는 필드를 한 곳에서 파싱한다 — 두 매핑 함수가 각자 파싱하면 필드 추출 로직이 조용히
/// 드리프트할 위험이 있어서다. 필수 필드(`id`/`identifier`/`team.key`)가 없으면 `None`(방어적 skip
/// — 모듈 문서 "주의" 참고). `identifier`는 스트림 키라 빈 문자열도 skip한다(`linear:` 같은 무의미한
/// 스트림 id 방지).
struct IssueCommonFields {
    id: String,
    identifier: String,
    issue_title: String,
    team_name: String,
    linear_project: Option<String>,
    state_name: Option<String>,
    state_type: Option<String>,
    /// description 앞부분([`DESCRIPTION_MAX_CHARS`]자 절단) — 생성 이벤트와 전이 이벤트가 동일하게
    /// 쓴다(명세: "body/url은 이슈 이벤트와 동일 규칙").
    body: Option<String>,
    url: Option<String>,
}

fn parse_issue_common_fields(node: &Value) -> Option<IssueCommonFields> {
    let id = node.get("id").and_then(Value::as_str)?.to_string();
    let identifier = node
        .get("identifier")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    let issue_title = node.get("title").and_then(Value::as_str).unwrap_or_default().to_string();
    let description = node.get("description").and_then(Value::as_str).unwrap_or_default();
    let url = node.get("url").and_then(Value::as_str).map(str::to_string);
    let team_key = node.pointer("/team/key").and_then(Value::as_str)?;
    let team_name = node
        .pointer("/team/name")
        .and_then(Value::as_str)
        .unwrap_or(team_key)
        .to_string();
    let linear_project = node.pointer("/project/name").and_then(Value::as_str).map(str::to_string);
    let state_name = node.pointer("/state/name").and_then(Value::as_str).map(str::to_string);
    let state_type = node.pointer("/state/type").and_then(Value::as_str).map(str::to_string);
    let body = if description.is_empty() {
        None
    } else {
        Some(truncate_with_ellipsis(description, DESCRIPTION_MAX_CHARS))
    };

    Some(IssueCommonFields {
        id,
        identifier,
        issue_title,
        team_name,
        linear_project,
        state_name,
        state_type,
        body,
        url,
    })
}

/// 이슈 1건(GraphQL `issues.nodes[]` 항목) → "생성" 이벤트. `createdAt`이 없으면 `None`(그 외 필수
/// 필드 판정은 [`parse_issue_common_fields`] 참고).
fn map_issue_node(node: &Value) -> Option<MappedEvent> {
    let common = parse_issue_common_fields(node)?;
    let created_at = node
        .get("createdAt")
        .and_then(Value::as_str)
        .and_then(parse_linear_timestamp)?;

    let event = EventInput {
        external_id: format!("ln:{}", common.id),
        stream_id: stream_id(&common.identifier),
        ts: created_at,
        source: SOURCE.to_string(),
        event_type: "message".to_string(),
        title: Some(format!("{} 생성: {}", common.identifier, common.issue_title)),
        body: common.body,
        model: None,
        tokens_in: None,
        tokens_out: None,
        url: common.url,
        parent_id: None,
        metadata: None,
    };
    Some(MappedEvent {
        identifier: common.identifier,
        issue_title: common.issue_title,
        team_name: common.team_name,
        linear_project: common.linear_project,
        state_name: common.state_name,
        state_type: common.state_type,
        event,
    })
}

/// `startedAt`/`completedAt`/`canceledAt` 전이 시각 필드 → (JSON 필드명, `externalId` 접미사, 이벤트
/// title 접미사) 매핑(모듈 문서 "매핑" 참고).
const ISSUE_TRANSITION_FIELDS: [(&str, &str, &str); 3] = [
    ("startedAt", "started", "시작"),
    ("completedAt", "completed", "완료"),
    ("canceledAt", "canceled", "취소"),
];

/// 이슈 노드의 `startedAt`/`completedAt`/`canceledAt` 각각을 이벤트 1건으로 만든다(있는 것만, 최대
/// 3건). "생성" 이벤트([`map_issue_node`])와 동일한 그룹 정보(identifier/issue_title/team_name/
/// linear_project/state)를 공유해 같은 스트림으로 묶이고, `externalId`에 전이 종류별 접미사를 붙여
/// 재수집 시에도 idempotent하다(모듈 문서 "매핑" 참고). 공통 필드가 없으면(`parse_issue_common_fields`
/// 가 `None`) 빈 벡터.
fn map_issue_transition_events(node: &Value) -> Vec<MappedEvent> {
    let Some(common) = parse_issue_common_fields(node) else {
        return Vec::new();
    };

    ISSUE_TRANSITION_FIELDS
        .iter()
        .filter_map(|(field, external_suffix, title_suffix)| {
            let ts = node.get(*field).and_then(Value::as_str).and_then(parse_linear_timestamp)?;
            let event = EventInput {
                external_id: format!("ln:{}#{external_suffix}", common.id),
                stream_id: stream_id(&common.identifier),
                ts,
                source: SOURCE.to_string(),
                event_type: "message".to_string(),
                title: Some(format!("{} {title_suffix}", common.identifier)),
                body: common.body.clone(),
                model: None,
                tokens_in: None,
                tokens_out: None,
                url: common.url.clone(),
                parent_id: None,
                metadata: None,
            };
            Some(MappedEvent {
                identifier: common.identifier.clone(),
                issue_title: common.issue_title.clone(),
                team_name: common.team_name.clone(),
                linear_project: common.linear_project.clone(),
                state_name: common.state_name.clone(),
                state_type: common.state_type.clone(),
                event,
            })
        })
        .collect()
}

/// 코멘트 1건(GraphQL `comments.nodes[]` 항목) → 코멘트 이벤트. `url`은 코멘트 자체 url이 있으면
/// 우선하고, 없으면 이슈 url로 폴백한다. 필수 필드(`id`/`createdAt`/`issue`/`issue.identifier`/
/// `issue.team.key`)가 없으면 `None`.
fn map_comment_node(node: &Value) -> Option<MappedEvent> {
    let id = node.get("id").and_then(Value::as_str)?;
    let body_text = node.get("body").and_then(Value::as_str).unwrap_or_default();
    let created_at = node
        .get("createdAt")
        .and_then(Value::as_str)
        .and_then(parse_linear_timestamp)?;
    let issue = node.get("issue")?;
    let identifier = issue
        .get("identifier")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    let issue_title = issue.get("title").and_then(Value::as_str).unwrap_or_default().to_string();
    let issue_url = issue.get("url").and_then(Value::as_str);
    let url = node.get("url").and_then(Value::as_str).or(issue_url).map(str::to_string);
    let team_key = issue.pointer("/team/key").and_then(Value::as_str)?;
    let team_name = issue
        .pointer("/team/name")
        .and_then(Value::as_str)
        .unwrap_or(team_key)
        .to_string();

    let event = EventInput {
        external_id: format!("ln:{id}"),
        stream_id: stream_id(&identifier),
        ts: created_at,
        source: SOURCE.to_string(),
        event_type: "message".to_string(),
        title: Some(format!("{identifier} 코멘트")),
        body: if body_text.is_empty() { None } else { Some(body_text.to_string()) },
        model: None,
        tokens_in: None,
        tokens_out: None,
        url,
        parent_id: None,
        metadata: None,
    };
    // 코멘트 쿼리는 project/state를 가져오지 않는다(1-1 범위 밖) — 그룹핑 시 `finalize_groups`가
    // 같은 이슈의 다른(이슈) 노드에서 채워진 값을 그대로 유지한다.
    Some(MappedEvent {
        identifier,
        issue_title,
        team_name,
        linear_project: None,
        state_name: None,
        state_type: None,
        event,
    })
}

/// 이슈 노드 목록 → "생성" 이벤트 + (있으면) 전이 이벤트. 한 노드가 최대 4건(생성 1 + 전이 최대 3)을
/// 만들 수 있다 — 노드 순서를 유지한 채 각 노드의 생성 이벤트 다음에 그 노드의 전이 이벤트들이
/// 온다(`finalize_groups`가 최종적으로 ts 오름차순 재정렬하므로 순서 자체는 결과에 영향 없음).
fn map_issue_nodes(nodes: &[Value]) -> Vec<MappedEvent> {
    nodes
        .iter()
        .flat_map(|node| map_issue_node(node).into_iter().chain(map_issue_transition_events(node)))
        .collect()
}

fn map_comment_nodes(nodes: &[Value]) -> Vec<MappedEvent> {
    nodes.iter().filter_map(map_comment_node).collect()
}

/// 이슈 하나로 그룹핑되는 동안 누적되는 상태(`finalize_groups` 내부 전용) — `issue_title`과 동일한
/// "비어있지 않은 쪽 채택" 규칙을 `linear_project`/`state_name`/`state_type`에도 적용한다.
struct GroupAccumulator {
    issue_title: String,
    team_name: String,
    linear_project: Option<String>,
    state_name: Option<String>,
    state_type: Option<String>,
    events: Vec<EventInput>,
}

/// 스트림 `metadata`(`StreamInput.metadata`) JSON — 값이 있는 키만 넣는다(명세: "값이 없는 키는
/// 넣지 마라"). 세 값이 모두 없으면 `None`(빈 `{}` 객체를 굳이 저장하지 않음).
/// `summary::excerpt::fetch_linear_issues`가 이 JSON을 다시 읽어 발췌 라인에 에픽·상태를 노출한다.
fn issue_stream_metadata(
    linear_project: Option<&str>,
    state_name: Option<&str>,
    state_type: Option<&str>,
) -> Option<Value> {
    let mut map = serde_json::Map::new();
    if let Some(v) = linear_project {
        map.insert("linearProject".to_string(), Value::String(v.to_string()));
    }
    if let Some(v) = state_name {
        map.insert("state".to_string(), Value::String(v.to_string()));
    }
    if let Some(v) = state_type {
        map.insert("stateType".to_string(), Value::String(v.to_string()));
    }
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

/// 이슈별로 그룹핑된 `MappedEvent` 목록 → `IngestRequest` 목록(이슈당 1건 그룹핑 —
/// `capture/slack.rs::normalize_search_results`/`capture/github.rs::finalize_groups`와 동일 계약,
/// `db::ingest()`가 요청 1건당 하나의 `stream` upsert를 전제하기 때문). 이벤트는 이슈 내 ts 오름차순
/// 정렬한다. 스트림 title=`<identifier> <이슈 제목>`, project=팀명(그대로 유지 — 에픽은 다른 층위라
/// `metadata.linearProject`로 별도 전달, 모듈 문서 "매핑" 참고) — 같은 배치에 이슈 제목/에픽/상태가
/// 비어 들어온 노드가 섞여 있으면 비어있지 않은 쪽을 채택한다.
fn finalize_groups(mapped: Vec<MappedEvent>) -> Vec<IngestRequest> {
    let mut groups: BTreeMap<String, GroupAccumulator> = BTreeMap::new();
    for m in mapped {
        let MappedEvent { identifier, issue_title, team_name, linear_project, state_name, state_type, event } = m;
        let entry = groups.entry(identifier).or_insert_with(|| GroupAccumulator {
            issue_title: issue_title.clone(),
            team_name,
            linear_project: None,
            state_name: None,
            state_type: None,
            events: Vec::new(),
        });
        if entry.issue_title.is_empty() && !issue_title.is_empty() {
            entry.issue_title = issue_title;
        }
        if entry.linear_project.is_none() && linear_project.is_some() {
            entry.linear_project = linear_project;
        }
        if entry.state_name.is_none() && state_name.is_some() {
            entry.state_name = state_name;
        }
        if entry.state_type.is_none() && state_type.is_some() {
            entry.state_type = state_type;
        }
        entry.events.push(event);
    }
    groups
        .into_iter()
        .map(|(identifier, accum)| {
            let GroupAccumulator { issue_title, team_name, linear_project, state_name, state_type, mut events } =
                accum;
            events.sort_by_key(|e| e.ts);
            let metadata =
                issue_stream_metadata(linear_project.as_deref(), state_name.as_deref(), state_type.as_deref());
            let stream = StreamInput {
                id: stream_id(&identifier),
                source: SOURCE.to_string(),
                kind: Some("session".to_string()),
                title: Some(stream_title(&identifier, &issue_title)),
                project: Some(team_name),
                git_branch: None,
                started_at: None,
                ended_at: None,
                status: None,
                metadata,
            };
            IngestRequest { stream: Some(stream), events }
        })
        .collect()
}

fn max_ts(mapped: &[MappedEvent]) -> Option<i64> {
    mapped.iter().map(|m| m.event.ts).max()
}

fn min_ts(mapped: &[MappedEvent]) -> Option<i64> {
    mapped.iter().map(|m| m.event.ts).min()
}

/// 엔티티(issues 또는 comments) 1개의 이중 커서 상태 — `capture_cursors`에서 읽은 값을 표현하는
/// 순수 구조체(`capture/slack.rs::CursorState`와 동일한 설계).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorState {
    /// 지금까지 본 **최신** updatedAt(ms). `None`이면 아직 전방 증분이 시작되지 않음(첫 실행).
    newest_ts: Option<i64>,
    /// 지금까지 내려간 **가장 오래된** updatedAt(ms). `None`이면 백필이 아직 시작되지 않음.
    backfill_ts: Option<i64>,
    /// 백필이 바닥(더 이상 과거 항목 없음)에 도달해 완료됐는지.
    backfill_done: bool,
}

/// 두 커서가 모두 없는 "첫 실행" 상태인지 판정한다(Slack `is_bootstrap`과 동일).
fn is_bootstrap(state: &CursorState) -> bool {
    state.newest_ts.is_none() && state.backfill_ts.is_none()
}

/// 첫 실행(`updatedAt` 필터 없는 배치) 결과로부터 두 커서의 초기값을 계산한다:
/// `(newest, backfill, backfill_done)`(Slack `init_cursors_from_bootstrap_batch`와 동일 원리).
fn init_cursors_from_bootstrap_batch(
    batch_min_ts: Option<i64>,
    batch_max_ts: Option<i64>,
) -> (Option<i64>, Option<i64>, bool) {
    match (batch_min_ts, batch_max_ts) {
        (Some(min_ts), Some(max_ts)) => (Some(max_ts), Some(min_ts), false),
        _ => (None, None, true),
    }
}

/// 전방 증분 커서 전진: 배치 최댓값이 있으면 그 값으로, 없으면 이전 값을 유지한다.
fn advance_newest(previous_newest_ts: Option<i64>, batch_max_ts: Option<i64>) -> Option<i64> {
    batch_max_ts.or(previous_newest_ts)
}

/// 후방 백필 커서 전진: `(새 backfill_ts, backfill_done)`. 배치 최솟값이 있으면 그 값으로 전진시키고
/// 미완료를 유지하며, 배치가 비어있으면 바닥에 도달했다는 뜻이라 완료 처리한다(Slack
/// `advance_backfill`과 동일).
fn advance_backfill(previous_backfill_ts: Option<i64>, batch_min_ts: Option<i64>) -> (Option<i64>, bool) {
    match batch_min_ts {
        Some(min_ts) => (Some(min_ts), false),
        None => (previous_backfill_ts, true),
    }
}

/// 429 연속 실패 시 다음 백오프 대기시간(초). GitHub/Slack과 동일한 계산(모듈마다 상수만 로컬로
/// 두고 로직은 독립적으로 유지).
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

/// `IssueFilter` 조각: `or: [creator.id.eq, assignee.id.eq]`(폴링 계정의 viewer id가 생성했거나
/// 배정된 이슈 — v1 스코프 정정, 모듈 문서 "v1 스코프" 참고) + 선택적 `updatedAt` 비교(`gt`/`lt`).
/// `updatedAt`은 `or`와 같은 객체 레벨(형제 키)에 둬 AND로 결합된다. GraphQL 리터럴 객체 문법으로
/// 직접 조립한다(변수 타입 이름을 몰라도 되도록 — 모듈 문서 "주의" 참고).
fn issue_filter(viewer_id: &str, updated_at: Option<(&str, &str)>) -> String {
    format!(
        r#"{{or: [{{creator: {{id: {{eq: "{viewer_id}"}}}}}}, {{assignee: {{id: {{eq: "{viewer_id}"}}}}}}]{}}}"#,
        updated_at_clause(updated_at)
    )
}

/// `CommentFilter` 조각: `user.id.eq` + 선택적 `updatedAt` 비교.
fn comment_filter(viewer_id: &str, updated_at: Option<(&str, &str)>) -> String {
    format!(r#"{{user: {{id: {{eq: "{viewer_id}"}}}}{}}}"#, updated_at_clause(updated_at))
}

fn updated_at_clause(updated_at: Option<(&str, &str)>) -> String {
    match updated_at {
        Some((comparator, iso)) => format!(r#", updatedAt: {{{comparator}: "{iso}"}}"#),
        None => String::new(),
    }
}

fn issues_page_query(filter: &str) -> String {
    format!(
        r#"query($after: String) {{ issues(first: {PAGE_SIZE}, after: $after, orderBy: updatedAt, filter: {filter}) {{ nodes {{ id identifier title description url createdAt team {{ key name }} project {{ name }} state {{ name type }} startedAt completedAt canceledAt }} pageInfo {{ hasNextPage endCursor }} }} }}"#
    )
}

fn comments_page_query(filter: &str) -> String {
    format!(
        r#"query($after: String) {{ comments(first: {PAGE_SIZE}, after: $after, orderBy: updatedAt, filter: {filter}) {{ nodes {{ id body url createdAt issue {{ identifier title url team {{ key name }} }} }} pageInfo {{ hasNextPage endCursor }} }} }}"#
    )
}

/// 계정 목록에서 `token`과 일치하는 계정의 `viewerId`/`viewerName`을 갱신한다(폴러가 `viewer` 검증에
/// 성공한 뒤 반영 — FE 계정 목록 표시용, `capture/github.rs::apply_resolved_username`과 동일 패턴).
/// 변경이 있었으면 `true`.
fn apply_resolved_viewer(accounts: &mut [LinearAccount], token: &str, viewer_id: &str, viewer_name: &str) -> bool {
    let mut changed = false;
    for account in accounts.iter_mut() {
        if account.token == token
            && (account.viewer_id.as_deref() != Some(viewer_id)
                || account.viewer_name.as_deref() != Some(viewer_name))
        {
            account.viewer_id = Some(viewer_id.to_string());
            account.viewer_name = Some(viewer_name.to_string());
            changed = true;
        }
    }
    changed
}

// ── 네트워크 호출 (단위테스트 대상 아님 — 모듈 문서 "수동 확인 포인트" 참고) ────────────

fn first_graphql_error(payload: &Value) -> Option<String> {
    payload
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|errors| errors.first())
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

async fn fetch_viewer(client: &reqwest::Client, token: &str) -> anyhow::Result<(String, String)> {
    let body = serde_json::json!({ "query": "query { viewer { id name email } }" });
    let resp = client
        .post(GRAPHQL_URL)
        .header(reqwest::header::AUTHORIZATION, token)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("Linear viewer 조회 실패: HTTP {}", resp.status());
    }
    let payload: Value = resp.json().await?;
    if let Some(message) = first_graphql_error(&payload) {
        anyhow::bail!("Linear viewer 조회 GraphQL 에러: {message}");
    }
    let viewer = payload.pointer("/data/viewer").ok_or_else(|| anyhow::anyhow!("viewer 응답 없음"))?;
    let id = viewer
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("viewer.id 없음"))?
        .to_string();
    let name = viewer.get("name").and_then(Value::as_str).unwrap_or("Linear").to_string();
    Ok((id, name))
}

enum PageOutcome {
    /// nodes + hasNextPage + endCursor.
    Nodes(Vec<Value>, bool, Option<String>),
    /// 429 — `Retry-After` 헤더 값(초, 없으면 `None`).
    RateLimited(Option<u64>),
}

async fn fetch_page(
    client: &reqwest::Client,
    token: &str,
    query: &str,
    after: Option<&str>,
    field: &str,
) -> anyhow::Result<PageOutcome> {
    let body = serde_json::json!({ "query": query, "variables": { "after": after } });
    let resp = client
        .post(GRAPHQL_URL)
        .header(reqwest::header::AUTHORIZATION, token)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(PageOutcome::RateLimited(retry_after));
    }
    if !status.is_success() {
        let snippet: String = resp.text().await.unwrap_or_default().chars().take(200).collect();
        anyhow::bail!("Linear GraphQL 실패: HTTP {status} — {snippet}");
    }

    let payload: Value = resp.json().await?;
    if let Some(message) = first_graphql_error(&payload) {
        anyhow::bail!("Linear GraphQL 에러: {message}");
    }
    let root = payload.pointer(&format!("/data/{field}"));
    let nodes = root.and_then(|r| r.get("nodes")).and_then(Value::as_array).cloned().unwrap_or_default();
    let has_next = root
        .and_then(|r| r.pointer("/pageInfo/hasNextPage"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let end_cursor = root
        .and_then(|r| r.pointer("/pageInfo/endCursor"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(PageOutcome::Nodes(nodes, has_next, end_cursor))
}

/// `query`(이미 filter가 조립된 완성 쿼리 문자열) 전체를 페이지네이션 순회해 노드를 모두 모은다.
/// 페이지 사이 GraphQL 네이티브 `after` 커서를 쓰고([`MAX_PAGES_PER_FETCH`] 상한), 429는
/// `Retry-After` 준수 + 지수 백오프(최대 5분)로 재시도한다. 그 외 에러는 상위로 전파해 이번 폴링
/// 주기를 포기한다(커서가 전진하지 않으므로 다음 주기에 안전하게 재시도).
async fn fetch_all_pages(client: &reqwest::Client, token: &str, query: &str, field: &str) -> anyhow::Result<Vec<Value>> {
    let mut all = Vec::new();
    let mut after: Option<String> = None;
    let mut backoff_secs = 0u64;
    let mut pages = 0u32;

    loop {
        match fetch_page(client, token, query, after.as_deref(), field).await? {
            PageOutcome::Nodes(nodes, has_next, end_cursor) => {
                backoff_secs = 0;
                pages += 1;
                all.extend(nodes);
                if !has_next || pages >= MAX_PAGES_PER_FETCH || end_cursor.is_none() {
                    break;
                }
                after = end_cursor;
                tokio::time::sleep(Duration::from_secs(PAGE_REQUEST_INTERVAL_SECS)).await;
            }
            PageOutcome::RateLimited(retry_after) => {
                backoff_secs = next_backoff_secs(backoff_secs, retry_after);
                eprintln!("[logroom] linear: rate limited, {backoff_secs}초 대기 후 재시도");
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                if backoff_secs >= MAX_BACKOFF_SECS {
                    anyhow::bail!("linear: 백오프 상한 도달, 이번 주기 포기");
                }
            }
        }
    }
    Ok(all)
}

/// 엔티티(issues 또는 comments) 1개의 이번 사이클 처리 결과.
struct EntityOutcome {
    mapped: Vec<MappedEvent>,
    new_newest_ts: Option<i64>,
    new_backfill_ts: Option<i64>,
    backfill_done: bool,
}

/// 엔티티 1개(issues 또는 comments)의 이중 커서 폴링 1주기 — 첫 실행(bootstrap) / 전방 증분 /
/// 후방 백필 분기는 `capture/slack.rs::poll_once`의 이중 커서 로직과 동일한 원리를 GraphQL 커서
/// 페이지네이션에 맞춰 재구성한 것이다. `filter_fn`/`page_query_fn`/`node_mapper`로 issues/comments
/// 차이만 주입받는다(순수 함수 포인터라 클로저 캡처 없이 안전하게 재사용 가능).
#[allow(clippy::too_many_arguments)]
async fn poll_entity(
    db: &Db,
    client: &reqwest::Client,
    token: &str,
    viewer_id: &str,
    newest_resource: &str,
    backfill_resource: &str,
    field: &str,
    filter_fn: fn(&str, Option<(&str, &str)>) -> String,
    page_query_fn: fn(&str) -> String,
    node_mapper: fn(&[Value]) -> Vec<MappedEvent>,
) -> anyhow::Result<EntityOutcome> {
    let (newest_ts, backfill_row) = {
        let conn = db.lock().expect("db mutex poisoned");
        let newest = db::get_cursor(&conn, SOURCE, newest_resource)?.map(|(offset, _)| offset);
        let backfill = db::get_cursor(&conn, SOURCE, backfill_resource)?;
        (newest, backfill)
    };
    let backfill_ts = backfill_row.map(|(offset, _)| offset);
    let backfill_done_flag = backfill_row.is_some_and(|(_, mtime)| mtime.is_some());
    let state = CursorState { newest_ts, backfill_ts, backfill_done: backfill_done_flag };

    let mut mapped = Vec::new();
    let new_newest_ts;
    let new_backfill_ts;
    let backfill_done;

    if is_bootstrap(&state) {
        let filter = filter_fn(viewer_id, None);
        let query = page_query_fn(&filter);
        let nodes = fetch_all_pages(client, token, &query, field).await?;
        let batch = node_mapper(&nodes);
        let (newest, backfill, done) = init_cursors_from_bootstrap_batch(min_ts(&batch), max_ts(&batch));
        new_newest_ts = newest;
        new_backfill_ts = backfill;
        backfill_done = done;
        mapped.extend(batch);
    } else {
        let fwd_iso = ms_to_iso(state.newest_ts.unwrap_or(0));
        let fwd_filter = filter_fn(viewer_id, Some(("gt", &fwd_iso)));
        let fwd_query = page_query_fn(&fwd_filter);
        let fwd_nodes = fetch_all_pages(client, token, &fwd_query, field).await?;
        let fwd_batch = node_mapper(&fwd_nodes);
        new_newest_ts = advance_newest(state.newest_ts, max_ts(&fwd_batch));
        mapped.extend(fwd_batch);

        if state.backfill_done {
            new_backfill_ts = state.backfill_ts;
            backfill_done = true;
        } else {
            let floor_iso = ms_to_iso(state.backfill_ts.unwrap_or(0));
            let bwd_filter = filter_fn(viewer_id, Some(("lt", &floor_iso)));
            let bwd_query = page_query_fn(&bwd_filter);
            let bwd_nodes = fetch_all_pages(client, token, &bwd_query, field).await?;
            let bwd_batch = node_mapper(&bwd_nodes);
            let (backfill, done) = advance_backfill(state.backfill_ts, min_ts(&bwd_batch));
            new_backfill_ts = backfill;
            backfill_done = done;
            mapped.extend(bwd_batch);
        }
    }

    Ok(EntityOutcome { mapped, new_newest_ts, new_backfill_ts, backfill_done })
}

fn persist_resolved_viewer(token: &str, viewer_id: &str, viewer_name: &str) {
    // 계정별 태스크가 동시에 부르므로 읽기~저장을 직렬화한다(`config::update_config` 참고).
    if let Err(e) = config::update_config(|cfg| {
        apply_resolved_viewer(&mut cfg.linear.accounts, token, viewer_id, viewer_name)
    }) {
        eprintln!("[logroom] linear: viewer 정보 저장 실패({e})");
    }
}

/// 계정(워크스페이스) 1개의 폴링 1주기: **일시정지 체크(최상단, 조기 반환)** → issues/comments 각각
/// 이중 커서 폴링([`poll_entity`]) → 합쳐서 팀별 그룹핑 → (일시정지 아니면) 스크럽+ingest → 커서
/// 전진. 반환값(`bool`)은 이번 사이클 종료 시점의 두 엔티티 백필 완료 여부(AND) — `spawn`의 루프가
/// `false`면 [`CATCHUP_INTERVAL_SECS`]로, `true`면 평소 간격(`pollMinutes`)으로 다음 사이클을 돈다
/// (GitHub/Slack과 동일 정책). 일시정지로 조기 반환하는 경우는 `true`를 돌려준다(원인 불명 에러 시
/// 폴백과 동일한 근거).
async fn poll_account_once(
    app: &AppHandle,
    db: &Db,
    capture_paused: &Arc<AtomicBool>,
    client: &reqwest::Client,
    token: &str,
    viewer_id: &str,
    scrub_secrets: bool,
) -> anyhow::Result<bool> {
    if capture_paused.load(Ordering::Relaxed) {
        return Ok(true);
    }

    let issues_newest_res = issues_newest_resource(viewer_id);
    let issues_backfill_res = issues_backfill_resource(viewer_id);
    let comments_newest_res = comments_newest_resource(viewer_id);
    let comments_backfill_res = comments_backfill_resource(viewer_id);

    let issues_outcome = poll_entity(
        db,
        client,
        token,
        viewer_id,
        &issues_newest_res,
        &issues_backfill_res,
        "issues",
        issue_filter,
        issues_page_query,
        map_issue_nodes,
    )
    .await?;
    let comments_outcome = poll_entity(
        db,
        client,
        token,
        viewer_id,
        &comments_newest_res,
        &comments_backfill_res,
        "comments",
        comment_filter,
        comments_page_query,
        map_comment_nodes,
    )
    .await?;

    let mut mapped = issues_outcome.mapped;
    mapped.extend(comments_outcome.mapped);
    let requests = finalize_groups(mapped);

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
    db::upsert_cursor(&conn, SOURCE, &issues_newest_res, issues_outcome.new_newest_ts.unwrap_or(0), None)?;
    db::upsert_cursor(
        &conn,
        SOURCE,
        &issues_backfill_res,
        issues_outcome.new_backfill_ts.unwrap_or(0),
        if issues_outcome.backfill_done { Some(1) } else { None },
    )?;
    db::upsert_cursor(&conn, SOURCE, &comments_newest_res, comments_outcome.new_newest_ts.unwrap_or(0), None)?;
    db::upsert_cursor(
        &conn,
        SOURCE,
        &comments_backfill_res,
        comments_outcome.new_backfill_ts.unwrap_or(0),
        if comments_outcome.backfill_done { Some(1) } else { None },
    )?;

    Ok(issues_outcome.backfill_done && comments_outcome.backfill_done)
}

/// 앱 시작 시 계정(워크스페이스)별 Linear 폴러를 백그라운드 태스크로 띄운다(`enabled=false`거나
/// 계정이 없으면 조용히 종료 — 다른 캡처 소스와 동일 패턴). 계정마다 독립된 태스크·커서를 가지므로
/// 한 계정의 키가 무효화돼도 다른 계정에는 영향이 없다(GitHub `spawn`과 동일 설계).
pub fn spawn(app: AppHandle, db: Db, capture_paused: Arc<AtomicBool>) {
    let cfg = config::load_linear_config();
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
            eprintln!("[logroom] linear: HTTP 클라이언트 생성 실패({e}) — 폴러 정지");
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
            let (viewer_id, viewer_name) = match fetch_viewer(&client, &token).await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "[logroom] linear: 계정 인증 실패({e}) — 이 계정은 건너뜁니다(API 키 확인 필요)"
                    );
                    return;
                }
            };
            persist_resolved_viewer(&token, &viewer_id, &viewer_name);
            eprintln!("[logroom] linear: 폴러 시작(계정={viewer_name}, {poll_minutes}분 간격)");

            loop {
                let backfill_done = match poll_account_once(
                    &app,
                    &db,
                    &capture_paused,
                    &client,
                    &token,
                    &viewer_id,
                    scrub_secrets,
                )
                .await
                {
                    Ok(done) => done,
                    Err(e) => {
                        eprintln!("[logroom] linear({viewer_name}): 폴링 실패({e}) — 다음 주기에 재시도");
                        true
                    }
                };

                let sleep_duration = if backfill_done {
                    poll_interval
                } else {
                    eprintln!(
                        "[logroom] linear({viewer_name}): 백필 미완료(밀린 이력 있음) — {CATCHUP_INTERVAL_SECS}초 뒤 다음 배치"
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
        assert_eq!(stream_id("TICKET-838"), "linear:TICKET-838");
    }

    #[test]
    fn stream_title_joins_identifier_and_title() {
        assert_eq!(stream_title("TICKET-1", "버그 수정"), "TICKET-1 버그 수정");
        assert_eq!(stream_title("TICKET-1", ""), "TICKET-1");
    }

    #[test]
    fn cursor_resource_keys_embed_viewer_id() {
        assert_eq!(issues_newest_resource("v1"), "issues:v1:newest");
        assert_eq!(issues_backfill_resource("v1"), "issues:v1:backfill");
        assert_eq!(comments_newest_resource("v1"), "comments:v1:newest");
        assert_eq!(comments_backfill_resource("v1"), "comments:v1:backfill");
    }

    // ── 타임스탬프 변환 ────────────────────────────────────────────

    #[test]
    fn parse_linear_timestamp_parses_rfc3339() {
        assert_eq!(
            parse_linear_timestamp("2021-01-01T00:00:00.000Z"),
            Some(1_609_459_200_000)
        );
    }

    #[test]
    fn parse_linear_timestamp_invalid_returns_none() {
        assert_eq!(parse_linear_timestamp("not-a-date"), None);
    }

    #[test]
    fn ms_to_iso_formats_epoch_with_millis_and_z_suffix() {
        assert_eq!(ms_to_iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(ms_to_iso(1_609_459_200_000), "2021-01-01T00:00:00.000Z");
    }

    // ── map_issue_node ───────────────────────────────────────────

    fn issue_node(overrides: Value) -> Value {
        let mut base = json!({
            "id": "issue-uuid-1",
            "identifier": "TICKET-1",
            "title": "버그 수정",
            "description": "상세 설명",
            "url": "https://linear.app/acme/issue/TICKET-1",
            "createdAt": "2021-01-01T00:00:00.000Z",
            "team": { "key": "TICKET", "name": "Maintenance" },
        });
        for (k, v) in overrides.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    }

    #[test]
    fn map_issue_node_maps_all_fields() {
        let mapped = map_issue_node(&issue_node(json!({}))).expect("매핑돼야 함");
        assert_eq!(mapped.identifier, "TICKET-1");
        assert_eq!(mapped.issue_title, "버그 수정");
        assert_eq!(mapped.team_name, "Maintenance");
        assert_eq!(mapped.event.external_id, "ln:issue-uuid-1");
        assert_eq!(mapped.event.stream_id, "linear:TICKET-1");
        assert_eq!(mapped.event.event_type, "message");
        assert_eq!(mapped.event.title.as_deref(), Some("TICKET-1 생성: 버그 수정"));
        assert_eq!(mapped.event.body.as_deref(), Some("상세 설명"));
        assert_eq!(mapped.event.url.as_deref(), Some("https://linear.app/acme/issue/TICKET-1"));
        assert_eq!(mapped.event.ts, 1_609_459_200_000);
    }

    #[test]
    fn map_issue_node_missing_identifier_returns_none() {
        // identifier는 스트림 키 — 없거나 비면 `linear:` 같은 무의미한 스트림이 생기므로 skip.
        let mut node = issue_node(json!({ "identifier": "" }));
        assert!(map_issue_node(&node).is_none());
        node.as_object_mut().unwrap().remove("identifier");
        assert!(map_issue_node(&node).is_none());
    }

    #[test]
    fn map_issue_node_missing_team_key_returns_none() {
        let mut node = issue_node(json!({}));
        node["team"] = json!({});
        assert!(map_issue_node(&node).is_none());
    }

    #[test]
    fn map_issue_node_missing_id_returns_none() {
        let mut node = issue_node(json!({}));
        node.as_object_mut().unwrap().remove("id");
        assert!(map_issue_node(&node).is_none());
    }

    #[test]
    fn map_issue_node_empty_description_has_no_body() {
        let mapped = map_issue_node(&issue_node(json!({ "description": "" }))).expect("매핑돼야 함");
        assert_eq!(mapped.event.body, None);
    }

    #[test]
    fn map_issue_node_truncates_long_description() {
        let long_description = "가".repeat(2100);
        let mapped =
            map_issue_node(&issue_node(json!({ "description": long_description }))).expect("매핑돼야 함");
        let body = mapped.event.body.expect("body 있어야 함");
        assert_eq!(body.chars().count(), 2001); // 2000 + '…'
        assert!(body.ends_with('…'));
    }

    #[test]
    fn map_issue_node_missing_team_name_falls_back_to_key() {
        let mut node = issue_node(json!({}));
        node["team"] = json!({ "key": "TICKET" });
        let mapped = map_issue_node(&node).expect("매핑돼야 함");
        assert_eq!(mapped.team_name, "TICKET");
    }

    #[test]
    fn map_issue_node_maps_project_and_state() {
        let node = issue_node(json!({
            "project": { "name": "결제 화면" },
            "state": { "name": "QA", "type": "started" },
        }));
        let mapped = map_issue_node(&node).expect("매핑돼야 함");
        assert_eq!(mapped.linear_project.as_deref(), Some("결제 화면"));
        assert_eq!(mapped.state_name.as_deref(), Some("QA"));
        assert_eq!(mapped.state_type.as_deref(), Some("started"));
    }

    #[test]
    fn map_issue_node_missing_project_and_state_are_none() {
        let mapped = map_issue_node(&issue_node(json!({}))).expect("매핑돼야 함");
        assert_eq!(mapped.linear_project, None);
        assert_eq!(mapped.state_name, None);
        assert_eq!(mapped.state_type, None);
    }

    // ── map_issue_transition_events(시작/완료/취소) ────────────────

    #[test]
    fn map_issue_transition_events_empty_when_no_transition_fields() {
        // startedAt/completedAt/canceledAt이 모두 없으면 전이 이벤트가 생기지 않아야 한다.
        assert!(map_issue_transition_events(&issue_node(json!({}))).is_empty());
    }

    #[test]
    fn map_issue_transition_events_completed_at_creates_completed_event_with_suffix() {
        let node = issue_node(json!({ "completedAt": "2021-01-03T00:00:00.000Z" }));
        let events = map_issue_transition_events(&node);
        assert_eq!(events.len(), 1);
        let completed = &events[0];
        assert_eq!(completed.event.external_id, "ln:issue-uuid-1#completed");
        assert_eq!(completed.event.stream_id, "linear:TICKET-1");
        assert_eq!(completed.event.event_type, "message");
        assert_eq!(completed.event.title.as_deref(), Some("TICKET-1 완료"));
        assert_eq!(
            completed.event.ts,
            parse_linear_timestamp("2021-01-03T00:00:00.000Z").unwrap()
        );
    }

    #[test]
    fn map_issue_transition_events_creates_one_event_per_present_field() {
        let node = issue_node(json!({
            "startedAt": "2021-01-02T00:00:00.000Z",
            "completedAt": "2021-01-03T00:00:00.000Z",
            "canceledAt": "2021-01-04T00:00:00.000Z",
        }));
        let events = map_issue_transition_events(&node);
        assert_eq!(events.len(), 3);
        let external_ids: Vec<&str> = events.iter().map(|m| m.event.external_id.as_str()).collect();
        assert_eq!(
            external_ids,
            vec!["ln:issue-uuid-1#started", "ln:issue-uuid-1#completed", "ln:issue-uuid-1#canceled"]
        );
        let titles: Vec<&str> = events.iter().filter_map(|m| m.event.title.as_deref()).collect();
        assert_eq!(titles, vec!["TICKET-1 시작", "TICKET-1 완료", "TICKET-1 취소"]);
    }

    #[test]
    fn map_issue_transition_events_missing_required_fields_returns_empty() {
        let mut node = issue_node(json!({ "completedAt": "2021-01-03T00:00:00.000Z" }));
        node.as_object_mut().unwrap().remove("identifier");
        assert!(map_issue_transition_events(&node).is_empty());
    }

    #[test]
    fn map_issue_nodes_includes_creation_and_transition_events() {
        let node = issue_node(json!({ "completedAt": "2021-01-03T00:00:00.000Z" }));
        let mapped = map_issue_nodes(&[node]);
        assert_eq!(mapped.len(), 2, "생성 이벤트 1건 + 완료 이벤트 1건");
        let external_ids: Vec<&str> = mapped.iter().map(|m| m.event.external_id.as_str()).collect();
        assert_eq!(external_ids, vec!["ln:issue-uuid-1", "ln:issue-uuid-1#completed"]);
    }

    // ── map_comment_node ─────────────────────────────────────────

    fn comment_node(overrides: Value) -> Value {
        let mut base = json!({
            "id": "comment-uuid-1",
            "body": "동의합니다",
            "createdAt": "2021-01-02T00:00:00.000Z",
            "issue": {
                "identifier": "TICKET-2",
                "title": "세션 만료 버그",
                "url": "https://linear.app/acme/issue/TICKET-2",
                "team": { "key": "TICKET", "name": "Maintenance" },
            },
        });
        for (k, v) in overrides.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    }

    #[test]
    fn map_comment_node_maps_all_fields_and_falls_back_to_issue_url() {
        let mapped = map_comment_node(&comment_node(json!({}))).expect("매핑돼야 함");
        assert_eq!(mapped.identifier, "TICKET-2");
        assert_eq!(mapped.issue_title, "세션 만료 버그");
        assert_eq!(mapped.team_name, "Maintenance");
        assert_eq!(mapped.event.external_id, "ln:comment-uuid-1");
        assert_eq!(mapped.event.stream_id, "linear:TICKET-2");
        assert_eq!(mapped.event.title.as_deref(), Some("TICKET-2 코멘트"));
        assert_eq!(mapped.event.body.as_deref(), Some("동의합니다"));
        // 코멘트 자체 url이 없으면 이슈 url로 폴백.
        assert_eq!(mapped.event.url.as_deref(), Some("https://linear.app/acme/issue/TICKET-2"));
    }

    #[test]
    fn map_comment_node_missing_issue_identifier_returns_none() {
        let mut node = comment_node(json!({}));
        node["issue"]["identifier"] = json!("");
        assert!(map_comment_node(&node).is_none());
    }

    #[test]
    fn map_comment_node_prefers_own_url_over_issue_url() {
        let node = comment_node(json!({ "url": "https://linear.app/acme/issue/TICKET-2#comment-1" }));
        let mapped = map_comment_node(&node).expect("매핑돼야 함");
        assert_eq!(mapped.event.url.as_deref(), Some("https://linear.app/acme/issue/TICKET-2#comment-1"));
    }

    #[test]
    fn map_comment_node_missing_issue_returns_none() {
        let mut node = comment_node(json!({}));
        node.as_object_mut().unwrap().remove("issue");
        assert!(map_comment_node(&node).is_none());
    }

    #[test]
    fn map_comment_node_empty_body_has_no_body() {
        let mapped = map_comment_node(&comment_node(json!({ "body": "" }))).expect("매핑돼야 함");
        assert_eq!(mapped.event.body, None);
    }

    // ── normalize_issue_nodes / normalize_comment_nodes(그룹핑) ───

    #[test]
    fn finalize_groups_groups_by_issue_and_sorts_by_ts() {
        // 같은 이슈(TICKET-1)의 생성+코멘트는 한 스트림으로 묶이고, 다른 이슈는 분리돼야 한다.
        let nodes = vec![
            issue_node(json!({ "id": "i-2", "createdAt": "2021-01-02T00:00:00.000Z" })),
            issue_node(json!({
                "id": "i-other",
                "identifier": "OTHER-9",
                "title": "다른 이슈",
                "team": { "key": "OTHER", "name": "Other Team" },
            })),
            issue_node(json!({ "id": "i-1", "createdAt": "2021-01-01T00:00:00.000Z" })),
        ];
        let requests = finalize_groups(map_issue_nodes(&nodes));
        assert_eq!(requests.len(), 2, "이슈별로 요청이 분리돼야 함");

        let maint = requests
            .iter()
            .find(|r| r.stream.as_ref().unwrap().id == "linear:TICKET-1")
            .expect("TICKET-1 요청 있어야 함");
        assert_eq!(maint.stream.as_ref().unwrap().title.as_deref(), Some("TICKET-1 버그 수정"));
        assert_eq!(maint.stream.as_ref().unwrap().project.as_deref(), Some("Maintenance"));
        assert_eq!(maint.events.len(), 2);
        assert!(maint.events[0].ts <= maint.events[1].ts);

        let other = requests
            .iter()
            .find(|r| r.stream.as_ref().unwrap().id == "linear:OTHER-9")
            .expect("OTHER-9 요청 있어야 함");
        assert_eq!(other.stream.as_ref().unwrap().title.as_deref(), Some("OTHER-9 다른 이슈"));
        assert_eq!(other.stream.as_ref().unwrap().project.as_deref(), Some("Other Team"));
        assert_eq!(other.events.len(), 1);
    }

    #[test]
    fn finalize_groups_comment_stream_carries_issue_title() {
        // 남의 이슈에 코멘트만 단 경우(생성 이벤트 없음) — 코멘트 쿼리의 이슈 title이 스트림 제목을 채운다.
        let nodes = vec![comment_node(json!({}))];
        let requests = finalize_groups(map_comment_nodes(&nodes));
        assert_eq!(requests.len(), 1);
        let stream = requests[0].stream.as_ref().unwrap();
        assert_eq!(stream.id, "linear:TICKET-2");
        assert_eq!(stream.title.as_deref(), Some("TICKET-2 세션 만료 버그"));
        assert_eq!(stream.project.as_deref(), Some("Maintenance"));
    }

    #[test]
    fn finalize_groups_prefers_non_empty_issue_title() {
        // 같은 배치에 제목이 빈 노드가 먼저 와도 비어있지 않은 쪽이 스트림 제목이 돼야 한다.
        let nodes = vec![
            issue_node(json!({ "id": "i-a", "title": "" })),
            issue_node(json!({ "id": "i-b", "createdAt": "2021-01-02T00:00:00.000Z" })),
        ];
        let requests = finalize_groups(map_issue_nodes(&nodes));
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].stream.as_ref().unwrap().title.as_deref(),
            Some("TICKET-1 버그 수정")
        );
    }

    #[test]
    fn finalize_groups_stream_metadata_carries_epic_and_state() {
        // state/project가 있는 이슈 노드 → StreamInput.metadata에 linearProject/state/stateType이
        // 올바른 JSON으로 담겨야 한다(값 없는 키는 생략).
        let node = issue_node(json!({
            "project": { "name": "결제 화면" },
            "state": { "name": "QA", "type": "started" },
        }));
        let requests = finalize_groups(map_issue_nodes(&[node]));
        assert_eq!(requests.len(), 1);
        let metadata = requests[0].stream.as_ref().unwrap().metadata.clone().expect("metadata 있어야 함");
        assert_eq!(
            metadata,
            json!({ "linearProject": "결제 화면", "state": "QA", "stateType": "started" })
        );
    }

    #[test]
    fn finalize_groups_stream_metadata_omits_missing_keys() {
        // project/state가 전혀 없으면 metadata 자체가 None이어야 한다(빈 `{}` 저장 방지).
        let requests = finalize_groups(map_issue_nodes(&[issue_node(json!({}))]));
        assert_eq!(requests[0].stream.as_ref().unwrap().metadata, None);
    }

    #[test]
    fn finalize_groups_stream_metadata_prefers_non_empty_value_across_batch() {
        // 같은 이슈가 여러 노드로 왔을 때 project/state가 있는 쪽을 채택(issue_title과 동일 규칙).
        let nodes = vec![
            issue_node(json!({ "id": "i-a" })),
            issue_node(json!({
                "id": "i-b",
                "createdAt": "2021-01-02T00:00:00.000Z",
                "project": { "name": "결제 화면" },
                "state": { "name": "QA", "type": "started" },
            })),
        ];
        let requests = finalize_groups(map_issue_nodes(&nodes));
        assert_eq!(requests.len(), 1);
        let metadata = requests[0].stream.as_ref().unwrap().metadata.clone().expect("metadata 있어야 함");
        assert_eq!(
            metadata,
            json!({ "linearProject": "결제 화면", "state": "QA", "stateType": "started" })
        );
    }

    #[test]
    fn normalize_issue_nodes_skips_malformed_entries() {
        let nodes = vec![json!({ "id": "broken" })];
        assert!(finalize_groups(map_issue_nodes(&nodes)).is_empty());
    }

    // ── max_ts / min_ts ──────────────────────────────────────────

    #[test]
    fn max_ts_and_min_ts_across_mapped_events() {
        let mapped = map_issue_nodes(&[
            issue_node(json!({ "id": "a", "createdAt": "2021-01-01T00:00:00.000Z" })),
            issue_node(json!({ "id": "b", "createdAt": "2021-01-05T00:00:00.000Z" })),
        ]);
        assert_eq!(max_ts(&mapped), Some(parse_linear_timestamp("2021-01-05T00:00:00.000Z").unwrap()));
        assert_eq!(min_ts(&mapped), Some(parse_linear_timestamp("2021-01-01T00:00:00.000Z").unwrap()));
    }

    #[test]
    fn max_ts_and_min_ts_none_when_empty() {
        assert_eq!(max_ts(&[]), None);
        assert_eq!(min_ts(&[]), None);
    }

    // ── 이중 커서 상태 판정 ────────────────────────────────────────

    #[test]
    fn is_bootstrap_true_only_when_both_cursors_absent() {
        assert!(is_bootstrap(&CursorState { newest_ts: None, backfill_ts: None, backfill_done: false }));
        assert!(!is_bootstrap(&CursorState { newest_ts: Some(1), backfill_ts: None, backfill_done: false }));
        assert!(!is_bootstrap(&CursorState { newest_ts: None, backfill_ts: Some(1), backfill_done: false }));
    }

    #[test]
    fn init_cursors_from_bootstrap_batch_with_results_seeds_both_cursors() {
        let (newest, backfill, done) = init_cursors_from_bootstrap_batch(Some(100), Some(500));
        assert_eq!(newest, Some(500));
        assert_eq!(backfill, Some(100));
        assert!(!done);
    }

    #[test]
    fn init_cursors_from_bootstrap_batch_empty_marks_done_without_seeding() {
        let (newest, backfill, done) = init_cursors_from_bootstrap_batch(None, None);
        assert_eq!(newest, None);
        assert_eq!(backfill, None);
        assert!(done);
    }

    #[test]
    fn advance_newest_uses_batch_max_or_keeps_previous() {
        assert_eq!(advance_newest(Some(100), Some(200)), Some(200));
        assert_eq!(advance_newest(Some(100), None), Some(100));
        assert_eq!(advance_newest(None, None), None);
    }

    #[test]
    fn advance_backfill_advances_or_marks_done_on_empty_batch() {
        assert_eq!(advance_backfill(Some(100), Some(50)), (Some(50), false));
        assert_eq!(advance_backfill(Some(50), None), (Some(50), true));
    }

    // ── GraphQL 필터/쿼리 조립(순수 문자열) ─────────────────────────

    #[test]
    fn issue_filter_without_updated_at_omits_clause() {
        assert_eq!(
            issue_filter("v1", None),
            r#"{or: [{creator: {id: {eq: "v1"}}}, {assignee: {id: {eq: "v1"}}}]}"#
        );
    }

    #[test]
    fn issue_filter_with_gt_comparator() {
        assert_eq!(
            issue_filter("v1", Some(("gt", "2021-01-01T00:00:00.000Z"))),
            r#"{or: [{creator: {id: {eq: "v1"}}}, {assignee: {id: {eq: "v1"}}}], updatedAt: {gt: "2021-01-01T00:00:00.000Z"}}"#
        );
    }

    #[test]
    fn issue_filter_combines_creator_and_assignee_with_or() {
        // 명세: "생성 OR 배정"으로 넓힌 필터 — creator/assignee가 or로 묶이는지 문자열 포함 검사.
        let filter = issue_filter("v1", None);
        assert!(filter.contains("or: ["));
        assert!(filter.contains(r#"creator: {id: {eq: "v1"}}"#));
        assert!(filter.contains(r#"assignee: {id: {eq: "v1"}}"#));
    }

    #[test]
    fn issue_filter_combines_or_and_updated_at_with_and() {
        // updatedAt 커서 조건은 or와 같은 객체 레벨(형제 키)에 있어야 AND로 결합된다(명세).
        let filter = issue_filter("v1", Some(("gt", "2021-01-01T00:00:00.000Z")));
        assert!(filter.ends_with(r#"], updatedAt: {gt: "2021-01-01T00:00:00.000Z"}}"#));
    }

    #[test]
    fn comment_filter_with_lt_comparator_uses_user_field() {
        // comment_filter는 명세대로 건드리지 않는다 — 코멘트는 여전히 "내가 쓴 것만".
        assert_eq!(
            comment_filter("v1", Some(("lt", "2021-01-01T00:00:00.000Z"))),
            r#"{user: {id: {eq: "v1"}}, updatedAt: {lt: "2021-01-01T00:00:00.000Z"}}"#
        );
    }

    #[test]
    fn issues_page_query_embeds_filter_and_page_size() {
        let filter = issue_filter("v1", None);
        let query = issues_page_query(&filter);
        assert!(query.contains(&format!("first: {PAGE_SIZE}")));
        assert!(query.contains(r#"filter: {or: [{creator: {id: {eq: "v1"}}}, {assignee: {id: {eq: "v1"}}}]}"#));
        assert!(query.contains("orderBy: updatedAt"));
    }

    #[test]
    fn issues_page_query_embeds_project_state_and_transition_fields() {
        // 1-1: 상태·에픽·전이 시각 필드가 추가돼야 한다(기존 필드는 그대로 유지).
        let query = issues_page_query(&issue_filter("v1", None));
        assert!(query.contains("id identifier title description url createdAt team { key name }"));
        assert!(query.contains("project { name }"));
        assert!(query.contains("state { name type }"));
        assert!(query.contains("startedAt completedAt canceledAt"));
    }

    #[test]
    fn comments_page_query_embeds_filter() {
        let filter = comment_filter("v1", None);
        let query = comments_page_query(&filter);
        assert!(query.contains(r#"filter: {user: {id: {eq: "v1"}}}"#));
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

    // ── viewer 정보 적용(계정 매칭) ──────────────────────────────

    #[test]
    fn apply_resolved_viewer_updates_matching_account() {
        let mut accounts = vec![LinearAccount {
            token: "lin_api_1".to_string(),
            viewer_id: None,
            viewer_name: None,
        }];
        let changed = apply_resolved_viewer(&mut accounts, "lin_api_1", "v1", "Ada");
        assert!(changed);
        assert_eq!(accounts[0].viewer_id.as_deref(), Some("v1"));
        assert_eq!(accounts[0].viewer_name.as_deref(), Some("Ada"));
    }

    #[test]
    fn apply_resolved_viewer_no_change_when_already_up_to_date() {
        let mut accounts = vec![LinearAccount {
            token: "lin_api_1".to_string(),
            viewer_id: Some("v1".to_string()),
            viewer_name: Some("Ada".to_string()),
        }];
        let changed = apply_resolved_viewer(&mut accounts, "lin_api_1", "v1", "Ada");
        assert!(!changed);
    }

    #[test]
    fn apply_resolved_viewer_ignores_non_matching_token() {
        let mut accounts = vec![LinearAccount {
            token: "lin_api_other".to_string(),
            viewer_id: None,
            viewer_name: None,
        }];
        let changed = apply_resolved_viewer(&mut accounts, "lin_api_1", "v1", "Ada");
        assert!(!changed);
        assert_eq!(accounts[0].viewer_id, None);
    }
}
