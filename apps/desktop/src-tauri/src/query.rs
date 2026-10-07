//! 읽기 커맨드 SQL 구현 (docs/02-data-model.md 시간 처리 · docs/06-roadmap.md M1a).
//! 모든 함수는 짧은 읽기 쿼리이므로 `Connection`을 잠근 채 동기로 실행한다.

use chrono::{NaiveDate, TimeZone};
use chrono_tz::Tz;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 마지막 이벤트로부터 이 시간(ms) 이내면 스트림을 "active"로 간주(docs/02-data-model.md).
const ACTIVE_THRESHOLD_MS: i64 = 15 * 60 * 1000;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("시스템 시간")
        .as_millis() as i64
}

fn compute_status(ended_at: Option<i64>, now: i64) -> &'static str {
    match ended_at {
        Some(e) if now - e <= ACTIVE_THRESHOLD_MS => "active",
        Some(_) => "done",
        None => "active",
    }
}

/// `pub(crate)`: data_admin.rs의 export도 동일한 metadata 파싱 규칙(비JSON이면 `{}`)을 공유한다.
pub(crate) fn parse_metadata(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

/// `localDate`("%Y-%m-%d") + `tz`(IANA 이름)를 로컬 자정 2개의 epoch ms 범위로 변환.
/// DST 등으로 로컬시각이 모호/존재하지 않으면 `earliest()`를 사용(docs/02-data-model.md).
/// `pub(crate)`: `summary/excerpt.rs`(M7-①, ADR-0016)도 다이제스트와 동일한 "하루" 정의를 그대로
/// 재사용한다(시간창 정의를 두 곳에 복붙하지 않기 위함).
pub(crate) fn day_range_ms(local_date: &str, tz: &str) -> anyhow::Result<(i64, i64)> {
    let zone: Tz = tz
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid timezone: {tz}"))?;
    let date = NaiveDate::parse_from_str(local_date, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("invalid localDate: {local_date}"))?;

    let start_naive = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid start time"))?;
    let end_naive = date
        .succ_opt()
        .ok_or_else(|| anyhow::anyhow!("invalid next day"))?
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid end time"))?;

    let start_ms = zone
        .from_local_datetime(&start_naive)
        .earliest()
        .ok_or_else(|| anyhow::anyhow!("ambiguous/invalid local start"))?
        .timestamp_millis();
    let end_ms = zone
        .from_local_datetime(&end_naive)
        .earliest()
        .ok_or_else(|| anyhow::anyhow!("ambiguous/invalid local end"))?
        .timestamp_millis();

    Ok((start_ms, end_ms))
}

/// `streams` 로우 → `StreamRow`(camelCase) JSON. `status`는 저장값을 무시하고 계산.
fn stream_row_to_json(row: &Row, now: i64) -> rusqlite::Result<Value> {
    let ended_at: Option<i64> = row.get("ended_at")?;
    let metadata_raw: String = row.get("metadata")?;
    Ok(json!({
        "id": row.get::<_, String>("id")?,
        "source": row.get::<_, String>("source")?,
        "kind": row.get::<_, String>("kind")?,
        "title": row.get::<_, Option<String>>("title")?,
        "project": row.get::<_, Option<String>>("project")?,
        "gitBranch": row.get::<_, Option<String>>("git_branch")?,
        "startedAt": row.get::<_, i64>("started_at")?,
        "endedAt": ended_at,
        "status": compute_status(ended_at, now),
        "metadata": parse_metadata(&metadata_raw),
    }))
}

const STREAM_COLUMNS: &str =
    "id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata";

/// `streams s` 별칭 기준 "이벤트가 하나도 없는 hub 스트림" 조건(list_events_by_day 전용).
/// hub 키가 없으면 json_extract 가 NULL 이라 `NOT (...)` 전체가 NULL(=제외)이 되므로 CASE 로 0/1 을 만든다.
const HUB_EMPTY_STREAM_SQL: &str = "CASE WHEN json_valid(s.metadata) AND json_extract(s.metadata, '$.hub')
               AND NOT EXISTS (SELECT 1 FROM events e WHERE e.stream_id = s.id) THEN 1 ELSE 0 END";

/// 로컬 하루(streams + 마커 events) 조회. `body`는 마커에서 제외(목록 전용).
pub fn list_events_by_day(conn: &Connection, local_date: &str, tz: &str) -> anyhow::Result<Value> {
    let (start_ms, end_ms) = day_range_ms(local_date, tz)?;
    let now = now_ms();

    // hub 세션(capture/hub.rs)은 이벤트가 전부 레포별 자식 스트림으로 옮겨지면 base 가 빈 껍데기로
    // 남는다(시간 범위는 그대로) — 빈 막대가 그려지지 않게 이벤트 0개인 hub 스트림은 뺀다.
    let stream_sql = format!(
        "SELECT {STREAM_COLUMNS} FROM streams s
         WHERE started_at < ?2 AND (ended_at >= ?1 OR ended_at IS NULL)
           AND NOT ({HUB_EMPTY_STREAM_SQL})
         ORDER BY started_at ASC"
    );
    let mut stream_stmt = conn.prepare(&stream_sql)?;
    let streams = stream_stmt
        .query_map(params![start_ms, end_ms], |row| stream_row_to_json(row, now))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut event_stmt = conn.prepare(
        "SELECT id, stream_id, ts, type, title FROM events
         WHERE ts >= ?1 AND ts < ?2
           AND stream_id IN (
             SELECT id FROM streams WHERE started_at < ?2 AND (ended_at >= ?1 OR ended_at IS NULL)
           )
         ORDER BY ts ASC",
    )?;
    let events = event_stmt
        .query_map(params![start_ms, end_ms], |row| {
            Ok(json!({
                "id": row.get::<_, String>("id")?,
                "streamId": row.get::<_, String>("stream_id")?,
                "ts": row.get::<_, i64>("ts")?,
                "type": row.get::<_, String>("type")?,
                "title": row.get::<_, Option<String>>("title")?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(json!({ "streams": streams, "events": events }))
}

/// 로컬 하루 동안 "내가 쓴 메시지"(Slack/GitHub/Linear 원문) 조회 — 요약 화면 우측 패널
/// (DayMessagesPanel.tsx) 전용. `source IN ('slack','github','linear') AND type = 'message'`가 곧
/// "내가 쓴 메시지"인 이유: 이 커넥터 3종은 캡처 시점부터 이벤트를 항상 `type="message"`로 남기고,
/// 캡처 정책 자체가 "내가 작성한 것만"(내가 보낸 슬랙 메시지/내가 단 GitHub 코멘트·PR/내가 남긴
/// Linear 코멘트 등)을 잡도록 설계돼 있다(채널 전체나 남이 쓴 글은 애초에 캡처 대상이 아님) — 그래서
/// 별도의 "작성자=나" 컬럼 없이도 이 소스/타입 필터만으로 충분하다. claude_code 등 나머지 소스는
/// type이 `prompt`/`response` 등으로 달라 이 조건에서 자연히 제외된다.
///
/// `INDEXED BY idx_events_ts` 하드 힌트: 하루 범위는 `ts`로 좁히는 게 압도적으로 선택적인데(전체
/// 이력 중 하루치만), 이 앱은 `ANALYZE`를 실행하지 않아 SQLite가 컬럼 통계를 갖고 있지 않다. 통계가
/// 없으면 플래너가 `type = 'message'` 조건을 보고 `idx_events_type`을 골라버릴 수 있는데, 이 경우
/// `ts` 범위와 무관하게 전체 이력 규모에 비례해 스캔 비용이 늘어난다(실측: 100만 건에서 197ms →
/// `idx_events_ts` 강제 시 <1ms). `ts` 인덱스를 하드 힌트로 고정해 이 리스크를 원천 차단한다.
pub fn list_my_messages_by_day(conn: &Connection, local_date: &str, tz: &str) -> anyhow::Result<Value> {
    let (start_ms, end_ms) = day_range_ms(local_date, tz)?;

    let mut stmt = conn.prepare(
        "SELECT e.id, e.stream_id, e.ts, e.source, e.title, e.body, e.url, s.title AS stream_title
         FROM events e INDEXED BY idx_events_ts JOIN streams s ON s.id = e.stream_id
         WHERE e.ts >= ?1 AND e.ts < ?2
           AND e.source IN ('slack','github','linear') AND e.type = 'message'
         ORDER BY e.ts ASC",
    )?;
    let messages = stmt
        .query_map(params![start_ms, end_ms], |row| {
            Ok(json!({
                "id": row.get::<_, String>("id")?,
                "streamId": row.get::<_, String>("stream_id")?,
                "ts": row.get::<_, i64>("ts")?,
                "source": row.get::<_, String>("source")?,
                "title": row.get::<_, Option<String>>("title")?,
                "body": row.get::<_, Option<String>>("body")?,
                "url": row.get::<_, Option<String>>("url")?,
                "streamTitle": row.get::<_, Option<String>>("stream_title")?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(json!({ "messages": messages }))
}

/// `e.source`/`e.type`/`e.ts` 공통 필터 조건을 "AND ..." SQL 조각 + 바인딩 값으로 만든다.
/// `sources`/`types`가 비어있으면 해당 조건은 생략(=전체). IN 절은 값 개수만큼 동적으로
/// `?` 플레이스홀더를 생성하되, 값은 항상 파라미터 바인딩만 사용해 SQL 인젝션을 막는다.
fn build_event_filter(
    sources: &[String],
    types: &[String],
    from_ts: Option<i64>,
    to_ts: Option<i64>,
) -> (String, Vec<SqlValue>) {
    let mut sql = String::new();
    let mut values: Vec<SqlValue> = Vec::new();

    if !sources.is_empty() {
        let placeholders = vec!["?"; sources.len()].join(", ");
        sql.push_str(&format!(" AND e.source IN ({placeholders})"));
        values.extend(sources.iter().cloned().map(SqlValue::from));
    }
    if !types.is_empty() {
        let placeholders = vec!["?"; types.len()].join(", ");
        sql.push_str(&format!(" AND e.type IN ({placeholders})"));
        values.extend(types.iter().cloned().map(SqlValue::from));
    }
    if let Some(from) = from_ts {
        sql.push_str(" AND e.ts >= ?");
        values.push(SqlValue::from(from));
    }
    if let Some(to) = to_ts {
        sql.push_str(" AND e.ts < ?");
        values.push(SqlValue::from(to));
    }

    (sql, values)
}

/// 전문검색. 3코드포인트 이상은 FTS5(trigram), 2글자 이하는 LIKE 폴백. 빈 쿼리는 빈 배열.
/// `sources`/`types`는 빈 슬라이스면 전체, `from_ts`/`to_ts`는 `[from_ts, to_ts)` 반열림 구간.
#[allow(clippy::too_many_arguments)]
pub fn search_events(
    conn: &Connection,
    query: &str,
    sources: &[String],
    types: &[String],
    from_ts: Option<i64>,
    to_ts: Option<i64>,
) -> anyhow::Result<Vec<Value>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }

    // trigram FTS5는 3코드포인트 미만 검색어를 토큰화하지 못해 항상 0건이 된다.
    // 2글자 이하는 LIKE 부분일치로 폴백해 사용자 오인("결과 없음")을 막는다.
    if q.chars().count() < 3 {
        return search_events_like(conn, q, sources, types, from_ts, to_ts);
    }

    // FTS5 쿼리 문법(AND/OR/컬럼 필터 등)을 사용자가 그대로 입력해도 오류/의도치 않은
    // 매칭이 나지 않도록 리터럴 phrase로 강제 이스케이프한다.
    let escaped = format!("\"{}\"", q.replace('"', "\"\""));
    let (filter_sql, filter_values) = build_event_filter(sources, types, from_ts, to_ts);

    let sql = format!(
        "SELECT e.id AS event_id, e.stream_id AS stream_id, e.ts AS ts, e.type AS type,
                e.source AS source, e.title AS title,
                COALESCE(snippet(events_fts, 1, '[', ']', '…', 12), '') AS snippet,
                s.title AS stream_title
         FROM events_fts
         JOIN events e ON e.rowid = events_fts.rowid
         JOIN streams s ON s.id = e.stream_id
         WHERE events_fts MATCH ?{filter_sql}
         ORDER BY e.ts DESC
         LIMIT 100"
    );
    let mut values: Vec<SqlValue> = vec![SqlValue::from(escaped)];
    values.extend(filter_values);

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(values), |row| {
            Ok(json!({
                "eventId": row.get::<_, String>("event_id")?,
                "streamId": row.get::<_, String>("stream_id")?,
                "ts": row.get::<_, i64>("ts")?,
                "type": row.get::<_, String>("type")?,
                "source": row.get::<_, String>("source")?,
                "title": row.get::<_, Option<String>>("title")?,
                "snippet": row.get::<_, String>("snippet")?,
                "streamTitle": row.get::<_, Option<String>>("stream_title")?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// 2글자 이하 검색어용 LIKE 부분일치 폴백. snippet은 매치 위치 기준으로 Rust에서 생성한다.
fn search_events_like(
    conn: &Connection,
    q: &str,
    sources: &[String],
    types: &[String],
    from_ts: Option<i64>,
    to_ts: Option<i64>,
) -> anyhow::Result<Vec<Value>> {
    // LIKE 와일드카드(%, _)와 이스케이프 문자(\)를 리터럴로 취급.
    let like = format!(
        "%{}%",
        q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    let (filter_sql, filter_values) = build_event_filter(sources, types, from_ts, to_ts);

    let sql = format!(
        "SELECT e.id AS event_id, e.stream_id AS stream_id, e.ts AS ts, e.type AS type,
                e.source AS source, e.title AS title, e.body AS body,
                s.title AS stream_title
         FROM events e
         JOIN streams s ON s.id = e.stream_id
         WHERE (e.title LIKE ? ESCAPE '\\' OR e.body LIKE ? ESCAPE '\\'){filter_sql}
         ORDER BY e.ts DESC
         LIMIT 100"
    );
    let mut values: Vec<SqlValue> = vec![SqlValue::from(like.clone()), SqlValue::from(like)];
    values.extend(filter_values);

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(values), |row| {
            let title: Option<String> = row.get("title")?;
            let body: Option<String> = row.get("body")?;
            Ok(json!({
                "eventId": row.get::<_, String>("event_id")?,
                "streamId": row.get::<_, String>("stream_id")?,
                "ts": row.get::<_, i64>("ts")?,
                "type": row.get::<_, String>("type")?,
                "source": row.get::<_, String>("source")?,
                "title": title,
                "snippet": like_snippet(body.as_deref().or(title.as_deref()), q),
                "streamTitle": row.get::<_, Option<String>>("stream_title")?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(rows)
}

/// LIKE 폴백 snippet: 첫 매치를 `[..]`로 감싸 주변을 발췌(문자 경계 안전, 미발견 시 앞 60자).
fn like_snippet(src: Option<&str>, q: &str) -> String {
    let Some(s) = src else {
        return String::new();
    };
    match s.find(q) {
        Some(byte_pos) => {
            let chars: Vec<char> = s.chars().collect();
            let hit = s[..byte_pos].chars().count();
            let q_len = q.chars().count();
            let start = hit.saturating_sub(20);
            let end = (hit + q_len + 40).min(chars.len());
            let mut out = String::new();
            if start > 0 {
                out.push('…');
            }
            out.extend(chars[start..hit].iter());
            out.push('[');
            out.extend(chars[hit..(hit + q_len).min(chars.len())].iter());
            out.push(']');
            out.extend(chars[(hit + q_len).min(chars.len())..end].iter());
            if end < chars.len() {
                out.push('…');
            }
            out
        }
        None => {
            let mut out: String = s.chars().take(60).collect();
            if s.chars().count() > 60 {
                out.push('…');
            }
            out
        }
    }
}

/// `events` 로우(get_stream 전용, body 포함) → `EventFull`(camelCase) JSON.
fn event_full_row_to_json(row: &Row) -> rusqlite::Result<Value> {
    let metadata_raw: String = row.get("metadata")?;
    Ok(json!({
        "id": row.get::<_, String>("id")?,
        "streamId": row.get::<_, String>("stream_id")?,
        "ts": row.get::<_, i64>("ts")?,
        "source": row.get::<_, String>("source")?,
        "type": row.get::<_, String>("type")?,
        "title": row.get::<_, Option<String>>("title")?,
        "body": row.get::<_, Option<String>>("body")?,
        "model": row.get::<_, Option<String>>("model")?,
        "tokensIn": row.get::<_, Option<i64>>("tokens_in")?,
        "tokensOut": row.get::<_, Option<i64>>("tokens_out")?,
        "url": row.get::<_, Option<String>>("url")?,
        "parentId": row.get::<_, Option<String>>("parent_id")?,
        "externalId": row.get::<_, Option<String>>("external_id")?,
        "metadata": parse_metadata(&metadata_raw),
    }))
}

const EVENT_FULL_COLUMNS: &str =
    "id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out,
     url, parent_id, external_id, metadata";

/// 스트림 상세 + 이벤트(body 포함). 없으면 `None`.
/// `from_ts`/`to_ts`가 **둘 다** `Some`이면 이벤트를 `[from_ts, to_ts)` 반열림 구간으로 좁힌다
/// (메시지형 스트림의 "그날 메시지만" 조회 — StreamDetailPanel.tsx 참고). 스트림 메타(제목/기간 등)는
/// 이 필터와 무관하게 항상 스트림 전체 기준. 하나만 `Some`이거나 둘 다 `None`이면 현행대로 전체 이벤트.
pub fn get_stream(
    conn: &Connection,
    id: &str,
    from_ts: Option<i64>,
    to_ts: Option<i64>,
) -> anyhow::Result<Option<Value>> {
    let now = now_ms();
    let stream_sql = format!("SELECT {STREAM_COLUMNS} FROM streams WHERE id = ?1");
    let stream: Option<Value> = conn
        .query_row(&stream_sql, params![id], |row| stream_row_to_json(row, now))
        .optional()?;

    let Some(stream) = stream else {
        return Ok(None);
    };

    let events = if let (Some(from), Some(to)) = (from_ts, to_ts) {
        let sql = format!(
            "SELECT {EVENT_FULL_COLUMNS} FROM events
             WHERE stream_id = ?1 AND ts >= ?2 AND ts < ?3 ORDER BY ts ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![id, from, to], event_full_row_to_json)?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    } else {
        let sql = format!("SELECT {EVENT_FULL_COLUMNS} FROM events WHERE stream_id = ?1 ORDER BY ts ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![id], event_full_row_to_json)?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };

    Ok(Some(json!({ "stream": stream, "events": events })))
}

/// 스트림 단위 하루 집계 로우(내부용). `first_ts`/`last_ts`는 스트림 전체가 아니라
/// **그날 범위 내 이벤트**의 min/max — 자정을 걸치는 스트림도 정확히 반영하기 위함.
struct StreamDigest {
    stream_id: String,
    kind: String,
    project: Option<String>,
    source: String,
    title: Option<String>,
    prompts: i64,
    responses: i64,
    tool_events: i64,
    tokens_in: i64,
    tokens_out: i64,
    first_ts: i64,
    last_ts: i64,
}

/// 로컬 하루 동안 발생한 **이벤트**를 스트림에 조인해 스트림별로 집계한다(스트림 메타의
/// started_at/ended_at이 아니라 이벤트 자체를 기준으로 삼아 자정 걸침 스트림도 정확히 반영).
fn digest_stream_rows(conn: &Connection, start_ms: i64, end_ms: i64) -> anyhow::Result<Vec<StreamDigest>> {
    let mut stmt = conn.prepare(
        "SELECT s.id AS stream_id, s.kind AS kind, s.project AS project, s.source AS source,
                s.title AS title,
                SUM(CASE WHEN e.type = 'prompt' THEN 1 ELSE 0 END) AS prompts,
                SUM(CASE WHEN e.type = 'response' THEN 1 ELSE 0 END) AS responses,
                SUM(CASE WHEN e.type IN ('tool_use', 'tool_result') THEN 1 ELSE 0 END) AS tool_events,
                COALESCE(SUM(e.tokens_in), 0) AS tokens_in,
                COALESCE(SUM(e.tokens_out), 0) AS tokens_out,
                MIN(e.ts) AS first_ts,
                MAX(e.ts) AS last_ts
         FROM events e
         JOIN streams s ON s.id = e.stream_id
         WHERE e.ts >= ?1 AND e.ts < ?2
         GROUP BY s.id",
    )?;
    let rows = stmt
        .query_map(params![start_ms, end_ms], |row| {
            Ok(StreamDigest {
                stream_id: row.get("stream_id")?,
                kind: row.get("kind")?,
                project: row.get("project")?,
                source: row.get("source")?,
                title: row.get("title")?,
                prompts: row.get("prompts")?,
                responses: row.get("responses")?,
                tool_events: row.get("tool_events")?,
                tokens_in: row.get("tokens_in")?,
                tokens_out: row.get("tokens_out")?,
                first_ts: row.get("first_ts")?,
                last_ts: row.get("last_ts")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 프로젝트별 중간 집계(내부용). `items`는 최종 반환 전 `started_at` 기준 오름차순 정렬한다.
#[derive(Default)]
struct ProjectDigest {
    streams: i64,
    prompts: i64,
    tokens_in: i64,
    tokens_out: i64,
    first_ts: i64,
    last_ts: i64,
    sources: Vec<String>,
    items: Vec<(i64, Value)>,
}

/// "오늘 한 일" 다이제스트. `totals`는 kind별 스트림 수 + 전체 이벤트/토큰 합,
/// `projects`는 `streams.project`별 그룹(NULL 포함, agent 스트림 수치도 합산)이며
/// `lastTs` 내림차순으로 정렬한다. `items`는 노이즈 방지를 위해 kind='session' 스트림만 담고
/// `startedAt` 오름차순으로 정렬한다.
pub fn get_digest(conn: &Connection, local_date: &str, tz: &str) -> anyhow::Result<Value> {
    let (start_ms, end_ms) = day_range_ms(local_date, tz)?;
    let streams = digest_stream_rows(conn, start_ms, end_ms)?;

    let mut totals_streams = 0i64;
    let mut totals_agent_streams = 0i64;
    let mut totals_prompts = 0i64;
    let mut totals_responses = 0i64;
    let mut totals_tool_events = 0i64;
    let mut totals_tokens_in = 0i64;
    let mut totals_tokens_out = 0i64;
    let mut totals_first_ts: Option<i64> = None;
    let mut totals_last_ts: Option<i64> = None;

    let mut project_order: Vec<Option<String>> = Vec::new();
    let mut projects: HashMap<Option<String>, ProjectDigest> = HashMap::new();

    for s in &streams {
        match s.kind.as_str() {
            "session" => totals_streams += 1,
            "agent" => totals_agent_streams += 1,
            _ => {}
        }
        totals_prompts += s.prompts;
        totals_responses += s.responses;
        totals_tool_events += s.tool_events;
        totals_tokens_in += s.tokens_in;
        totals_tokens_out += s.tokens_out;
        totals_first_ts = Some(totals_first_ts.map_or(s.first_ts, |v| v.min(s.first_ts)));
        totals_last_ts = Some(totals_last_ts.map_or(s.last_ts, |v| v.max(s.last_ts)));

        let key = s.project.clone();
        let entry = projects.entry(key).or_insert_with(|| {
            project_order.push(s.project.clone());
            ProjectDigest {
                first_ts: s.first_ts,
                last_ts: s.last_ts,
                ..Default::default()
            }
        });
        entry.streams += 1;
        entry.prompts += s.prompts;
        entry.tokens_in += s.tokens_in;
        entry.tokens_out += s.tokens_out;
        entry.first_ts = entry.first_ts.min(s.first_ts);
        entry.last_ts = entry.last_ts.max(s.last_ts);
        if !entry.sources.iter().any(|src| src == &s.source) {
            entry.sources.push(s.source.clone());
        }
        if s.kind == "session" {
            entry.items.push((
                s.first_ts,
                json!({
                    "streamId": s.stream_id,
                    "title": s.title,
                    "source": s.source,
                    "prompts": s.prompts,
                    "startedAt": s.first_ts,
                    "endedAt": s.last_ts,
                }),
            ));
        }
    }

    let mut project_values: Vec<(i64, Value)> = project_order
        .into_iter()
        .map(|project_key| {
            let mut p = projects
                .remove(&project_key)
                .expect("project_order는 projects에 실제로 삽입된 키만 담는다");
            p.items.sort_by_key(|(started_at, _)| *started_at);
            let items: Vec<Value> = p.items.into_iter().map(|(_, v)| v).collect();
            let last_ts = p.last_ts;
            let value = json!({
                "project": project_key,
                "streams": p.streams,
                "prompts": p.prompts,
                "tokensIn": p.tokens_in,
                "tokensOut": p.tokens_out,
                "firstTs": p.first_ts,
                "lastTs": p.last_ts,
                "sources": p.sources,
                "items": items,
            });
            (last_ts, value)
        })
        .collect();

    // projects 정렬 = lastTs DESC.
    project_values.sort_by_key(|(last_ts, _)| std::cmp::Reverse(*last_ts));
    let projects_json: Vec<Value> = project_values.into_iter().map(|(_, v)| v).collect();

    Ok(json!({
        "totals": {
            "streams": totals_streams,
            "agentStreams": totals_agent_streams,
            "prompts": totals_prompts,
            "responses": totals_responses,
            "toolEvents": totals_tool_events,
            "tokensIn": totals_tokens_in,
            "tokensOut": totals_tokens_out,
            "firstTs": totals_first_ts,
            "lastTs": totals_last_ts,
        },
        "projects": projects_json,
    }))
}

/// `db_path`(예: `logroom.db`) 옆의 WAL 파일 경로(`logroom.db-wal`). WAL은 체크포인트 전까지
/// 실제 변경분을 담고 있어 DB 실사용량을 보여주려면 본 파일 크기와 합산해야 한다.
/// `pub(crate)`: data_admin.rs::vacuum_db도 동일 로직으로 전/후 크기를 잰다.
pub(crate) fn wal_path_for(db_path: &Path) -> PathBuf {
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    PathBuf::from(wal)
}

/// 파일 크기(bytes). 파일이 없으면(WAL 체크포인트 직후 등) 0 — 에러로 취급하지 않는다.
pub(crate) fn file_size_or_zero(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// `db_path` 본체 + WAL 파일 크기 합산(bytes). 설정 다이얼로그 "데이터" 섹션(`get_db_stats`)과
/// DB 최적화 버튼(`data_admin::vacuum_db`)이 "실사용량"을 동일 기준으로 보여주기 위한 공통 헬퍼.
pub(crate) fn total_db_size_bytes(db_path: &Path) -> u64 {
    file_size_or_zero(db_path) + file_size_or_zero(&wal_path_for(db_path))
}

/// 설정 다이얼로그 "데이터" 섹션(읽기 전용)이 쓰는 DB 통계: 파일 크기(DB 본체 + WAL) +
/// 이벤트/스트림 수 + 기록 범위(가장 이른/늦은 이벤트 ts). FE 계약(camelCase)과 1:1 대응.
pub fn get_db_stats(conn: &Connection, db_path: &Path) -> anyhow::Result<Value> {
    let db_size_bytes = file_size_or_zero(db_path);
    let wal_size_bytes = file_size_or_zero(&wal_path_for(db_path));

    let events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
    let streams: i64 = conn.query_row("SELECT COUNT(*) FROM streams", [], |row| row.get(0))?;
    let (oldest_ts, newest_ts): (Option<i64>, Option<i64>) = conn.query_row(
        "SELECT MIN(ts), MAX(ts) FROM events",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    Ok(json!({
        "dbSizeBytes": db_size_bytes,
        "walSizeBytes": wal_size_bytes,
        "events": events,
        "streams": streams,
        "oldestTs": oldest_ts,
        "newestTs": newest_ts,
    }))
}

/// 전체 프로젝트 표시명 목록(요약 뷰 "재개" 스코프의 프로젝트 드롭다운, M7-② 발견성 개선). `streams`
/// 전체를 표시명(마지막 `/` 세그먼트, `summary::excerpt::last_path_segment` 재사용 — 소스마다
/// project 키 형식이 달라도 표시 이름이 같으면 하나로 묶여야 한다) 기준으로 distinct하고, 각 표시명의
/// 최근 활동(그 표시명으로 매칭되는 스트림들의 `ended_at` 최대값) 내림차순으로 정렬한다. `project`가
/// `NULL`인 스트림은 표시명이 없으므로 제외한다.
pub fn list_projects(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT project, ended_at FROM streams WHERE project IS NOT NULL AND ended_at IS NOT NULL")?;
    let rows = stmt
        .query_map([], |row| {
            let project: String = row.get("project")?;
            let ended_at: i64 = row.get("ended_at")?;
            Ok((project, ended_at))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // 표시명별 최근 활동(max ended_at) 집계 — HashMap은 순회 순서가 불안정하므로, 삽입 순서를 따로
    // 기록하지 않고 최종적으로 (표시명, 최근활동) 쌍을 벡터에 모아 값으로 정렬한다.
    let mut latest_by_name: HashMap<String, i64> = HashMap::new();
    for (project, ended_at) in rows {
        let display_name = crate::summary::excerpt::last_path_segment(&project).to_string();
        let entry = latest_by_name.entry(display_name).or_insert(ended_at);
        if ended_at > *entry {
            *entry = ended_at;
        }
    }

    let mut names: Vec<(String, i64)> = latest_by_name.into_iter().collect();
    // 최근활동 내림차순, 동률이면 이름 오름차순(결정적 순서 — 테스트/UI 안정성).
    names.sort_by(|(a_name, a_ts), (b_name, b_ts)| b_ts.cmp(a_ts).then_with(|| a_name.cmp(b_name)));

    Ok(names.into_iter().map(|(name, _)| name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// `db.rs` 테스트와 동일 패턴 — `tempfile` 크레이트 없이 임시 디렉토리를 만든다.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!(
                "logroom-query-test-{}-{n}-{nanos}",
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

    /// V1~V3 전체 마이그레이션이 적용된 빈 DB 커넥션(+ 살아있는 동안 파일을 유지할 `TempDir`).
    fn seeded_conn() -> (TempDir, Connection) {
        let tmp = TempDir::new();
        let conn = db::open_and_migrate_at(&tmp.db_path()).expect("마이그레이션 성공");
        (tmp, conn)
    }

    // 테스트 전용 시드 헬퍼 — 인자 수보다 호출부 가독성(명시적 필드명)을 우선한다.
    #[allow(clippy::too_many_arguments)]
    fn insert_stream(
        conn: &Connection,
        id: &str,
        source: &str,
        kind: &str,
        project: Option<&str>,
        title: Option<&str>,
        started_at: i64,
        ended_at: i64,
    ) {
        conn.execute(
            "INSERT INTO streams
               (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, 'active', '{}', ?8)",
            params![id, source, kind, title, project, started_at, ended_at, started_at],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_event(
        conn: &Connection,
        id: &str,
        stream_id: &str,
        ts: i64,
        event_type: &str,
        tokens_in: Option<i64>,
        tokens_out: Option<i64>,
        external_id: &str,
    ) {
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES (?1, ?2, ?3, 'claude_code', ?4, NULL, NULL, NULL, ?5, ?6, NULL, NULL, ?7, '{}', ?8)",
            params![id, stream_id, ts, event_type, tokens_in, tokens_out, external_id, ts],
        )
        .unwrap();
    }

    /// `search_events` 필터 테스트 전용 — source/type/body를 모두 지정해 검색어 매칭 대상을 만든다.
    #[allow(clippy::too_many_arguments)]
    fn insert_search_event(
        conn: &Connection,
        id: &str,
        stream_id: &str,
        ts: i64,
        source: &str,
        event_type: &str,
        body: &str,
        external_id: &str,
    ) {
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, NULL, NULL, NULL, NULL, NULL, ?7, '{}', ?3)",
            params![id, stream_id, ts, source, event_type, body, external_id],
        )
        .unwrap();
    }

    #[test]
    fn search_events_filters_by_source() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(
            &conn,
            "src:sess-1",
            "claude_code",
            "session",
            Some("proj-a"),
            Some("세션 1"),
            1_000,
            2_000,
        );
        insert_stream(
            &conn,
            "src2:sess-2",
            "kiro_cli",
            "session",
            Some("proj-a"),
            Some("세션 2"),
            1_000,
            2_000,
        );
        insert_search_event(
            &conn,
            "ev-1",
            "src:sess-1",
            1_000,
            "claude_code",
            "prompt",
            "moonlight gadget review",
            "ext-1",
        );
        insert_search_event(
            &conn,
            "ev-2",
            "src2:sess-2",
            1_500,
            "kiro_cli",
            "prompt",
            "moonlight gadget again",
            "ext-2",
        );

        let sources = vec!["claude_code".to_string()];
        let hits = search_events(&conn, "gadget", &sources, &[], None, None).unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["eventId"], json!("ev-1"));
        assert_eq!(hits[0]["source"], json!("claude_code"));
    }

    #[test]
    fn search_events_filters_by_type() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(
            &conn,
            "src:sess-1",
            "claude_code",
            "session",
            Some("proj-a"),
            Some("세션 1"),
            1_000,
            2_000,
        );
        insert_search_event(
            &conn,
            "ev-prompt",
            "src:sess-1",
            1_000,
            "claude_code",
            "prompt",
            "starlight budget plan",
            "ext-prompt",
        );
        insert_search_event(
            &conn,
            "ev-response",
            "src:sess-1",
            1_500,
            "claude_code",
            "response",
            "starlight budget answer",
            "ext-response",
        );

        let types = vec!["response".to_string()];
        let hits = search_events(&conn, "budget", &[], &types, None, None).unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["eventId"], json!("ev-response"));
        assert_eq!(hits[0]["type"], json!("response"));
    }

    #[test]
    fn search_events_filters_by_time_range() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(
            &conn,
            "src:sess-1",
            "claude_code",
            "session",
            Some("proj-a"),
            Some("세션 1"),
            1_000,
            50_000,
        );
        insert_search_event(
            &conn,
            "ev-early",
            "src:sess-1",
            1_000,
            "claude_code",
            "prompt",
            "twilight forest path",
            "ext-early",
        );
        insert_search_event(
            &conn,
            "ev-late",
            "src:sess-1",
            40_000,
            "claude_code",
            "prompt",
            "twilight forest camp",
            "ext-late",
        );

        let hits = search_events(&conn, "forest", &[], &[], Some(10_000), Some(50_000)).unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["eventId"], json!("ev-late"));
        assert_eq!(hits[0]["ts"], json!(40_000));
    }

    #[test]
    fn search_events_combines_source_type_and_period_filters_in_like_fallback() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(
            &conn,
            "src:sess-1",
            "claude_code",
            "session",
            Some("proj-a"),
            Some("세션 1"),
            1_000,
            50_000,
        );
        insert_stream(
            &conn,
            "src2:sess-2",
            "kiro_cli",
            "session",
            Some("proj-a"),
            Some("세션 2"),
            1_000,
            50_000,
        );
        // 검색어를 2글자("hi")로 둬 LIKE 폴백 경로를 태운다(build_event_filter 재사용 검증).
        insert_search_event(
            &conn,
            "ev-match",
            "src:sess-1",
            20_000,
            "claude_code",
            "prompt",
            "say hi there",
            "ext-match",
        );
        // source 불일치.
        insert_search_event(
            &conn,
            "ev-wrong-source",
            "src2:sess-2",
            20_000,
            "kiro_cli",
            "prompt",
            "say hi there",
            "ext-wrong-source",
        );
        // type 불일치.
        insert_search_event(
            &conn,
            "ev-wrong-type",
            "src:sess-1",
            20_000,
            "claude_code",
            "response",
            "say hi there",
            "ext-wrong-type",
        );
        // 기간 밖.
        insert_search_event(
            &conn,
            "ev-wrong-time",
            "src:sess-1",
            60_000,
            "claude_code",
            "prompt",
            "say hi there",
            "ext-wrong-time",
        );

        let sources = vec!["claude_code".to_string()];
        let types = vec!["prompt".to_string()];
        let hits = search_events(&conn, "hi", &sources, &types, Some(10_000), Some(50_000)).unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["eventId"], json!("ev-match"));
    }

    // ── get_stream(스트림 상세) ─────────────────────────────

    #[test]
    fn get_stream_returns_all_events_when_range_not_specified() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(
            &conn,
            "slack:C1",
            "slack",
            "session",
            None,
            Some("채널"),
            1_000,
            50_000,
        );
        insert_event(&conn, "ev-early", "slack:C1", 1_000, "message", None, None, "ext-early");
        insert_event(&conn, "ev-late", "slack:C1", 40_000, "message", None, None, "ext-late");

        let result = get_stream(&conn, "slack:C1", None, None).unwrap().unwrap();

        let events = result["events"].as_array().unwrap();
        assert_eq!(events.len(), 2, "range 미지정이면 전체 이벤트(대화형 회귀 없음)");
    }

    #[test]
    fn get_stream_scopes_events_to_from_to_range_when_both_specified() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(
            &conn,
            "slack:C1",
            "slack",
            "session",
            None,
            Some("채널"),
            1_000,
            50_000,
        );
        insert_event(&conn, "ev-early", "slack:C1", 1_000, "message", None, None, "ext-early");
        insert_event(&conn, "ev-in-range", "slack:C1", 20_000, "message", None, None, "ext-in-range");
        insert_event(&conn, "ev-late", "slack:C1", 40_000, "message", None, None, "ext-late");

        let result = get_stream(&conn, "slack:C1", Some(10_000), Some(30_000))
            .unwrap()
            .unwrap();

        let events = result["events"].as_array().unwrap();
        assert_eq!(events.len(), 1, "[from, to) 반열림 구간만 반환");
        assert_eq!(events[0]["id"], json!("ev-in-range"));

        // 스트림 메타(제목/기간)는 range와 무관하게 항상 스트림 전체 기준이어야 한다.
        assert_eq!(result["stream"]["startedAt"], json!(1_000));
        assert_eq!(result["stream"]["endedAt"], json!(50_000));
    }

    #[test]
    fn get_stream_treats_partial_range_as_unspecified() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(
            &conn,
            "slack:C1",
            "slack",
            "session",
            None,
            Some("채널"),
            1_000,
            50_000,
        );
        insert_event(&conn, "ev-early", "slack:C1", 1_000, "message", None, None, "ext-early");
        insert_event(&conn, "ev-late", "slack:C1", 40_000, "message", None, None, "ext-late");

        let result = get_stream(&conn, "slack:C1", Some(10_000), None).unwrap().unwrap();

        let events = result["events"].as_array().unwrap();
        assert_eq!(events.len(), 2, "from_ts/to_ts 중 하나만 있으면 필터 미적용(전체 반환)");
    }

    #[test]
    fn get_digest_aggregates_totals_and_sorts_projects_by_last_ts_desc() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, end_ms) = day_range_ms("2026-07-06", "Asia/Seoul").unwrap();

        // proj-a: session 스트림(프롬프트2·응답1·툴 이벤트2) + agent 스트림(프롬프트1, items 제외 대상).
        insert_stream(
            &conn,
            "src:sess-a1",
            "claude_code",
            "session",
            Some("proj-a"),
            Some("세션 A1"),
            start_ms + 1_000,
            start_ms + 5_000,
        );
        insert_event(&conn, "ev-a1-p1", "src:sess-a1", start_ms + 1_000, "prompt", None, None, "ext-a1-p1");
        insert_event(&conn, "ev-a1-p2", "src:sess-a1", start_ms + 2_000, "prompt", None, None, "ext-a1-p2");
        insert_event(
            &conn,
            "ev-a1-r1",
            "src:sess-a1",
            start_ms + 3_000,
            "response",
            Some(100),
            Some(200),
            "ext-a1-r1",
        );
        insert_event(&conn, "ev-a1-tu", "src:sess-a1", start_ms + 4_000, "tool_use", None, None, "ext-a1-tu");
        insert_event(&conn, "ev-a1-tr", "src:sess-a1", start_ms + 5_000, "tool_result", None, None, "ext-a1-tr");

        insert_stream(
            &conn,
            "src:agent-a1",
            "claude_code",
            "agent",
            Some("proj-a"),
            None,
            start_ms + 1_500,
            start_ms + 1_500,
        );
        insert_event(
            &conn,
            "ev-agent-a1-p1",
            "src:agent-a1",
            start_ms + 1_500,
            "prompt",
            None,
            None,
            "ext-agent-a1-p1",
        );

        // proj-b: proj-a보다 늦은 시각의 session 스트림 하나 — lastTs DESC 정렬 검증용.
        // tokensOut은 미기록(NULL)이라 0으로 집계돼야 한다.
        insert_stream(
            &conn,
            "src:sess-b1",
            "claude_code",
            "session",
            Some("proj-b"),
            Some("세션 B1"),
            start_ms + 10_000,
            start_ms + 20_000,
        );
        insert_event(&conn, "ev-b1-p1", "src:sess-b1", start_ms + 10_000, "prompt", None, None, "ext-b1-p1");
        insert_event(
            &conn,
            "ev-b1-r1",
            "src:sess-b1",
            start_ms + 20_000,
            "response",
            Some(50),
            None,
            "ext-b1-r1",
        );

        // 하루 범위 밖(전날) 이벤트 — 집계에서 제외돼야 함.
        insert_event(
            &conn,
            "ev-a1-out-of-range",
            "src:sess-a1",
            start_ms - 1_000,
            "prompt",
            None,
            None,
            "ext-a1-out",
        );

        let digest = get_digest(&conn, "2026-07-06", "Asia/Seoul").unwrap();

        assert_eq!(digest["totals"]["streams"], json!(2));
        assert_eq!(digest["totals"]["agentStreams"], json!(1));
        assert_eq!(digest["totals"]["prompts"], json!(4));
        assert_eq!(digest["totals"]["responses"], json!(2));
        assert_eq!(digest["totals"]["toolEvents"], json!(2));
        assert_eq!(digest["totals"]["tokensIn"], json!(150));
        assert_eq!(digest["totals"]["tokensOut"], json!(200));
        assert_eq!(digest["totals"]["firstTs"], json!(start_ms + 1_000));
        assert_eq!(digest["totals"]["lastTs"], json!(start_ms + 20_000));
        assert!(end_ms > start_ms + 20_000, "테스트 시각이 하루 범위 안에 있어야 함");

        let projects = digest["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0]["project"], json!("proj-b"), "lastTs가 더 늦은 proj-b가 먼저");
        assert_eq!(projects[1]["project"], json!("proj-a"));

        let proj_a = &projects[1];
        assert_eq!(proj_a["streams"], json!(2), "session + agent 스트림 모두 카운트");
        assert_eq!(proj_a["prompts"], json!(3), "agent 프롬프트도 합산");
        let proj_a_items = proj_a["items"].as_array().unwrap();
        assert_eq!(proj_a_items.len(), 1, "items는 session 스트림만");
        assert_eq!(proj_a_items[0]["streamId"], json!("src:sess-a1"));
        assert_eq!(proj_a_items[0]["prompts"], json!(2));
        assert_eq!(proj_a_items[0]["startedAt"], json!(start_ms + 1_000));
        assert_eq!(proj_a_items[0]["endedAt"], json!(start_ms + 5_000));

        let proj_b = &projects[0];
        assert_eq!(proj_b["streams"], json!(1));
        assert_eq!(proj_b["tokensIn"], json!(50));
        assert_eq!(proj_b["tokensOut"], json!(0), "NULL tokensOut은 0으로 집계");
    }

    #[test]
    fn get_digest_scopes_to_day_events_and_groups_null_project() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, end_ms) = day_range_ms("2026-07-06", "Asia/Seoul").unwrap();

        // 자정 걸침 스트림: DB상 started_at/ended_at은 하루 범위 밖이지만 이벤트는
        // 전날 1건 + 당일 2건 + 다음날 1건 → 당일 이벤트만 집계돼야 함.
        insert_stream(
            &conn,
            "src:sess-mid",
            "claude_code",
            "session",
            None,
            Some("자정 걸침"),
            start_ms - 60_000,
            end_ms + 60_000,
        );
        insert_event(&conn, "ev-mid-before", "src:sess-mid", start_ms - 1_000, "prompt", None, None, "ext-mid-before");
        insert_event(&conn, "ev-mid-in1", "src:sess-mid", start_ms + 500, "prompt", None, None, "ext-mid-in1");
        insert_event(&conn, "ev-mid-in2", "src:sess-mid", end_ms - 500, "response", None, None, "ext-mid-in2");
        insert_event(&conn, "ev-mid-after", "src:sess-mid", end_ms + 1_000, "prompt", None, None, "ext-mid-after");

        // 같은 NULL 프로젝트, 다른 source 스트림 — sources 고유 목록/정렬 검증용.
        insert_stream(
            &conn,
            "src2:sess-null2",
            "kiro_cli",
            "session",
            None,
            Some("두번째"),
            start_ms + 100,
            start_ms + 100,
        );
        insert_event(&conn, "ev-null2-p1", "src2:sess-null2", start_ms + 100, "prompt", None, None, "ext-null2-p1");

        let digest = get_digest(&conn, "2026-07-06", "Asia/Seoul").unwrap();

        assert_eq!(digest["totals"]["streams"], json!(2));
        assert_eq!(digest["totals"]["prompts"], json!(2), "자정 밖 이벤트는 제외");
        assert_eq!(digest["totals"]["responses"], json!(1));
        assert_eq!(digest["totals"]["firstTs"], json!(start_ms + 100));
        assert_eq!(digest["totals"]["lastTs"], json!(end_ms - 500));

        let projects = digest["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 1);
        assert!(projects[0]["project"].is_null());

        let mut sources: Vec<&str> = projects[0]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        sources.sort_unstable();
        assert_eq!(sources, vec!["claude_code", "kiro_cli"]);

        let items = projects[0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        // items 정렬 = startedAt ASC.
        assert_eq!(items[0]["streamId"], json!("src2:sess-null2"));
        assert_eq!(items[1]["streamId"], json!("src:sess-mid"));
        assert_eq!(items[1]["prompts"], json!(1), "당일 범위 내 prompt만(전날/익일 제외)");
        assert_eq!(items[1]["startedAt"], json!(start_ms + 500), "스트림 컬럼이 아니라 당일 이벤트 min");
        assert_eq!(items[1]["endedAt"], json!(end_ms - 500), "스트림 컬럼이 아니라 당일 이벤트 max");
    }

    // ── get_db_stats(설정 다이얼로그 "데이터" 섹션) ─────────────

    #[test]
    fn get_db_stats_empty_db_returns_zero_counts_and_null_ts() {
        let (tmp, conn) = seeded_conn();
        let stats = get_db_stats(&conn, &tmp.db_path()).unwrap();

        assert_eq!(stats["events"], json!(0));
        assert_eq!(stats["streams"], json!(0));
        assert!(stats["oldestTs"].is_null());
        assert!(stats["newestTs"].is_null());
        // 마이그레이션이 이미 적용된 실제 DB 파일이므로 크기는 0보다 커야 함.
        assert!(stats["dbSizeBytes"].as_u64().unwrap() > 0);
        // WAL 파일이 없을 수도 있는 상태(체크포인트 직후) — 필드 존재만 확인, 값은 0 이상이면 충분.
        assert!(stats["walSizeBytes"].as_u64().is_some());
    }

    #[test]
    fn get_db_stats_counts_events_streams_and_ts_range() {
        let (tmp, conn) = seeded_conn();
        insert_stream(&conn, "src:sess-1", "claude_code", "session", Some("proj-a"), Some("세션 1"), 1_000, 5_000);
        insert_stream(&conn, "src2:sess-2", "kiro_cli", "session", Some("proj-b"), Some("세션 2"), 2_000, 3_000);
        insert_event(&conn, "ev-1", "src:sess-1", 1_000, "prompt", None, None, "ext-1");
        insert_event(&conn, "ev-2", "src:sess-1", 5_000, "response", None, None, "ext-2");
        insert_event(&conn, "ev-3", "src2:sess-2", 3_000, "prompt", None, None, "ext-3");

        let stats = get_db_stats(&conn, &tmp.db_path()).unwrap();

        assert_eq!(stats["events"], json!(3));
        assert_eq!(stats["streams"], json!(2));
        assert_eq!(stats["oldestTs"], json!(1_000));
        assert_eq!(stats["newestTs"], json!(5_000));
        assert!(stats["dbSizeBytes"].as_u64().unwrap() > 0);
    }

    #[test]
    fn get_db_stats_missing_db_file_returns_zero_size_without_erroring() {
        let (tmp, conn) = seeded_conn();
        let missing_path = tmp.path.join("does-not-exist.db");

        let stats = get_db_stats(&conn, &missing_path).unwrap();

        assert_eq!(stats["dbSizeBytes"], json!(0));
        assert_eq!(stats["walSizeBytes"], json!(0));
    }

    // ── list_projects(요약 뷰 "재개" 스코프 프로젝트 드롭다운, M7-② 발견성) ──────

    #[test]
    fn list_projects_dedupes_by_display_name_across_sources() {
        // Claude(절대경로) / GitHub(owner/repo) 형식이 달라도 표시명("logroom")이 같으면 하나로
        // 묶여야 한다.
        let (_tmp, conn) = seeded_conn();
        insert_stream(&conn, "claude_code:s1", "claude_code", "session", Some("/Users/x/git/logroom"), Some("세션"), 1_000, 5_000);
        insert_stream(&conn, "github:owner/logroom", "github", "session", Some("owner/logroom"), Some("owner/logroom"), 1_000, 3_000);

        let projects = list_projects(&conn).unwrap();
        assert_eq!(projects, vec!["logroom".to_string()]);
    }

    #[test]
    fn list_projects_orders_by_most_recent_activity_desc() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(&conn, "src:older", "claude_code", "session", Some("/x/older-project"), Some("세션"), 1_000, 5_000);
        insert_stream(&conn, "src:newer", "claude_code", "session", Some("/x/newer-project"), Some("세션"), 1_000, 10_000);

        let projects = list_projects(&conn).unwrap();
        assert_eq!(projects, vec!["newer-project".to_string(), "older-project".to_string()]);
    }

    #[test]
    fn list_projects_excludes_null_project_streams() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(&conn, "src:no-project", "slack", "session", None, Some("채널"), 1_000, 5_000);

        let projects = list_projects(&conn).unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn list_projects_empty_db_returns_empty_vec() {
        let (_tmp, conn) = seeded_conn();
        assert_eq!(list_projects(&conn).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn list_my_messages_by_day_excludes_events_outside_day_range() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, end_ms) = day_range_ms("2026-07-06", "Asia/Seoul").unwrap();
        insert_stream(&conn, "slack:C1", "slack", "session", None, Some("채널"), start_ms, end_ms);
        insert_search_event(
            &conn,
            "ev-in-range",
            "slack:C1",
            start_ms + 1_000,
            "slack",
            "message",
            "오늘 쓴 메시지",
            "ext-in-range",
        );
        insert_search_event(
            &conn,
            "ev-before",
            "slack:C1",
            start_ms - 1_000,
            "slack",
            "message",
            "전날 쓴 메시지",
            "ext-before",
        );
        insert_search_event(
            &conn,
            "ev-after",
            "slack:C1",
            end_ms,
            "slack",
            "message",
            "다음날 쓴 메시지",
            "ext-after",
        );

        let result = list_my_messages_by_day(&conn, "2026-07-06", "Asia/Seoul").unwrap();

        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "[start, end) 하루 범위 밖 이벤트는 제외");
        assert_eq!(messages[0]["id"], json!("ev-in-range"));
    }

    #[test]
    fn list_my_messages_by_day_excludes_claude_code_prompts() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, end_ms) = day_range_ms("2026-07-06", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "src:sess-1",
            "claude_code",
            "session",
            Some("proj-a"),
            Some("세션 1"),
            start_ms,
            end_ms,
        );
        insert_stream(&conn, "slack:C1", "slack", "session", None, Some("채널"), start_ms, end_ms);
        insert_event(&conn, "ev-prompt", "src:sess-1", start_ms + 1_000, "prompt", None, None, "ext-prompt");
        insert_search_event(
            &conn,
            "ev-slack",
            "slack:C1",
            start_ms + 2_000,
            "slack",
            "message",
            "슬랙 메시지",
            "ext-slack",
        );

        let result = list_my_messages_by_day(&conn, "2026-07-06", "Asia/Seoul").unwrap();

        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "claude_code(type=prompt)는 '내가 쓴 메시지'가 아니라 제외");
        assert_eq!(messages[0]["id"], json!("ev-slack"));
        assert_eq!(messages[0]["streamTitle"], json!("채널"));
    }

    #[test]
    fn list_events_by_day_hides_empty_hub_streams_only() {
        let (_tmp, conn) = seeded_conn();
        let (start, _) = day_range_ms("2026-10-01", "UTC").unwrap();
        let t = start + 1_000;
        // 이벤트가 모두 자식으로 옮겨진 hub base(빈 껍데기) / 이벤트 있는 hub 자식 / 이벤트 없는 일반 스트림.
        insert_stream(&conn, "claude_code:hub", "claude_code", "session", Some("/h"), None, t, t + 10);
        insert_stream(&conn, "claude_code:hub@/r", "claude_code", "session", Some("/r"), None, t, t + 10);
        insert_stream(&conn, "claude_code:plain", "claude_code", "session", Some("/p"), None, t, t + 10);
        conn.execute(
            "UPDATE streams SET metadata = '{\"hub\":true}' WHERE id IN ('claude_code:hub', 'claude_code:hub@/r')",
            [],
        )
        .unwrap();
        insert_event(&conn, "e1", "claude_code:hub@/r", t, "prompt", None, None, "x1");

        let out = list_events_by_day(&conn, "2026-10-01", "UTC").unwrap();
        let mut ids: Vec<&str> = out["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["claude_code:hub@/r", "claude_code:plain"]);
    }
}
