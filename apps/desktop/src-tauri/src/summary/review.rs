//! 업무평가서(ADR-0017) — 분기 평가(`quarter`, "YYYY-Qn")와 월간 점검(`month`, "YYYY-MM").
//!
//! 요약 계층 위에 한 칸 더 얹는 구조다. 분기 평가는 그 분기 **월간 요약 3개**를, 월간 점검은 그 달에
//! 걸친 **주간 요약들**을 입력으로 쓰고(원본 이벤트 재발췌 없음 — `period.rs`의 "계층 롤업" 원칙),
//! 여기에 DB가 직접 센 **사실 신호**(Linear 완료·취소·미완료·정체, GitHub PR·이슈·리뷰)와 사용자가
//! 준 **평가 프로필·기간 목표**를 붙여 발췌를 만든다. 숫자는 DB, 판단은 LLM(요약과 같은 분업).
//!
//! 하위 요약 조달 규칙([`lower_summary`]):
//! - 캐시가 없으면 한 단계만 생성한다 — 분기는 빠진 월간을, 월간 점검은 빠진 주간을 만들되 그 아래는
//!   `Cascade::CachedOnly`로 내려보낸다. 평가서 한 번이 수십 회 LLM 호출로 번지지 않게 하는 요약의
//!   팬아웃 방지 계약을 그대로 따른다(분기 최대 3회 + 평가 1회).
//! - 캐시가 있어도 **그 기간이 끝나기 전에 만든 것**이면 다시 만든다([`is_incomplete`]). 9/10에 만든
//!   9월 요약으로 3분기를 평가하면 9월 후반이 통째로 빠진다. 진행 중인 기간은 하루 이상 지난 캐시만
//!   다시 만든다(같은 날 여러 번 누를 때 비용 방지).
//!
//! 프로젝트 필터: 사용자가 고른 프로젝트의 `## {프로젝트}` 섹션만 하위 요약에서 남기고
//! ([`filter_project_sections`]), 사실 신호·통계도 같은 프로젝트로 좁힌다. 회사 평가에 낼 때 개인
//! 프로젝트를 빼려는 용도다. 프로젝트 이름은 요약과 같은 표시명(`excerpt::last_path_segment`)이다.

use super::excerpt::{self, MAX_EXCERPT_CHARS, NO_DATA_ERROR_CODE};
use super::{engine, period, prompts};
use crate::capture::config::{ReviewProfile, SummaryConfig};
use crate::capture::policy::truncate_with_ellipsis;
use crate::capture::scrub;
use crate::query::day_range_ms;
use chrono::{Datelike, NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

pub const REVIEW_TYPE_QUARTER: &str = "quarter";
pub const REVIEW_TYPE_MONTH: &str = "month";

/// 기간 목표 입력 상한(문자 수). FE textarea `maxLength`와 같다 — BE에서도 한 번 더 자른다.
pub const MAX_GOALS_CHARS: usize = 2_000;

const MONTHS_PER_QUARTER: u32 = 3;
const DAY_MS: i64 = 86_400_000;
/// 이 일수 이상 아무 활동이 없는 미완료 이슈를 "정체"로 표시한다.
const STALL_DAYS: i64 = 30;
/// 사실 신호에 나열하는 미완료 이슈 최대 개수(정체가 먼저, 그다음 오래된 순).
const MAX_CARRY_OVER_LINES: usize = 12;
const ISSUE_TITLE_MAX_CHARS: usize = 70;
/// 활동일 집계용 버킷(15분). 실존하는 모든 UTC 오프셋이 15분의 배수라 한 버킷은 항상 로컬 하루
/// 안에 들어간다 — 이벤트 수십만 건을 전부 읽지 않고 로컬 날짜를 셀 수 있다.
const ACTIVITY_BUCKET_MS: i64 = 15 * 60 * 1000;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("시스템 시간")
        .as_millis() as i64
}

// ── 기간 계산(순수함수) ───────────────────────────────────────────

/// "YYYY-Qn" → (연도, 분기 1~4).
fn parse_quarter_key(period_key: &str) -> anyhow::Result<(i32, u32)> {
    let invalid = || anyhow::anyhow!("invalid quarter key: {period_key}");
    let (year, quarter) = period_key.split_once("-Q").ok_or_else(invalid)?;
    let year: i32 = year.parse().map_err(|_| invalid())?;
    let quarter: u32 = quarter.parse().map_err(|_| invalid())?;
    if !(1..=4).contains(&quarter) {
        return Err(invalid());
    }
    Ok((year, quarter))
}

/// 분기 기간: `[start_ms, end_ms)`(첫 달 1일 00:00 ~ 다음 분기 1일 00:00, 로컬) + 월 키 3개.
/// 달력 분기(1~3월 = Q1)다 — 회계연도는 회사마다 달라 다루지 않는다.
pub fn quarter_range(period_key: &str, tz: &str) -> anyhow::Result<(i64, i64, Vec<String>)> {
    let (year, quarter) = parse_quarter_key(period_key)?;
    let first_month = (quarter - 1) * MONTHS_PER_QUARTER + 1;
    let month_keys: Vec<String> =
        (0..MONTHS_PER_QUARTER).map(|i| format!("{year}-{:02}", first_month + i)).collect();
    let (start_ms, _, _) = period::month_range(&month_keys[0], tz)?;
    let (_, end_ms, _) = period::month_range(&month_keys[2], tz)?;
    Ok((start_ms, end_ms, month_keys))
}

/// 평가서의 하위 요약 1개(분기 → 월간, 월간 점검 → 주간).
struct LowerUnit {
    /// `period_summaries.period_type`("month" | "week").
    kind: &'static str,
    key: String,
    /// 이 하위 기간이 끝나는 시각(epoch ms, exclusive) — 캐시가 기간 종료 전에 만들어졌는지 판정한다.
    end_ms: i64,
}

/// 평가 기간 `[start_ms, end_ms)` + 하위 요약 목록. 평가 종류가 틀리면 에러.
fn review_range(period_type: &str, period_key: &str, tz: &str) -> anyhow::Result<(i64, i64, Vec<LowerUnit>)> {
    match period_type {
        REVIEW_TYPE_QUARTER => {
            let (start_ms, end_ms, month_keys) = quarter_range(period_key, tz)?;
            let units = month_keys
                .into_iter()
                .map(|key| {
                    let (_, month_end, _) = period::month_range(&key, tz)?;
                    Ok(LowerUnit { kind: "month", key, end_ms: month_end })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok((start_ms, end_ms, units))
        }
        REVIEW_TYPE_MONTH => {
            let (start_ms, end_ms, week_keys) = period::month_range(period_key, tz)?;
            let units = week_keys
                .into_iter()
                .map(|key| {
                    let (_, week_end, _) = period::week_range(&key, tz)?;
                    Ok(LowerUnit { kind: "week", key, end_ms: week_end })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok((start_ms, end_ms, units))
        }
        other => anyhow::bail!("invalid review periodType: {other}"),
    }
}

/// 캐시된 하위 요약이 "그 기간을 다 담지 못한" 상태인가. 끝난 기간은 종료 전에 만든 캐시가, 진행 중인
/// 기간은 하루 이상 지난 캐시가 해당한다(모듈 문서 "하위 요약 조달 규칙").
fn is_incomplete(created_at: i64, unit_end_ms: i64, now: i64) -> bool {
    created_at < unit_end_ms.min(now - DAY_MS)
}

// ── 프로젝트 필터 ─────────────────────────────────────────────────

/// 요약 본문에서 `## {프로젝트}` 섹션 중 `keep`에 든 것만 남긴다(그 아래 `### ` 주제·불릿·`> ` 줄 포함).
/// 첫 `## ` 이전 총평과 `## 정리`/`## Summary` 같은 비프로젝트 섹션은 버린다 — 여러 프로젝트를 한데
/// 섞어 적은 줄이라 제외한 프로젝트 내용이 새어 들어온다.
pub fn filter_project_sections(content: &str, keep: &HashSet<String>) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut keeping = false;
    for line in content.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            keeping = keep.contains(heading.trim());
        }
        if keeping {
            out.push(line);
        }
    }
    out.join("\n").trim().to_string()
}

/// `stream.project` 원문이 필터를 통과하는가(`None` = 전체 통과). 프로젝트가 없는 스트림은 필터가
/// 있을 때 제외한다 — 어느 프로젝트 것인지 모르면 사용자가 뺀 프로젝트일 수도 있다.
fn project_passes(project: Option<&str>, filter: Option<&HashSet<String>>) -> bool {
    match (filter, project) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(keep), Some(raw)) => keep.contains(excerpt::last_path_segment(raw)),
    }
}

// ── 기간 내 프로젝트 목록(체크리스트) ─────────────────────────────

/// 평가 기간에 내 요청·메시지(`prompt`/`message`)가 있었던 프로젝트 표시명과 그 수(내림차순, 동률은
/// 이름순). FE "포함할 프로젝트" 체크리스트가 쓴다. 로컬 SQL만 쓰므로 요약 활성화와 무관하게 조회된다.
pub fn list_review_projects(
    conn: &Connection,
    period_type: &str,
    period_key: &str,
    tz: &str,
) -> anyhow::Result<Vec<Value>> {
    let (start_ms, end_ms, _) = review_range(period_type, period_key, tz)?;
    Ok(project_activity(conn, start_ms, end_ms, None)?
        .into_iter()
        .map(|(name, activity)| json!({ "name": name, "activity": activity }))
        .collect())
}

/// 프로젝트 표시명별 내 요청·메시지 수(내림차순, 동률은 이름순). 체크리스트와 평가서의 "프로젝트 비중"
/// 차트가 공유한다 — 차트는 고른 프로젝트로 좁힌 값(`filter`)을 쓴다.
fn project_activity(
    conn: &Connection,
    start_ms: i64,
    end_ms: i64,
    filter: Option<&HashSet<String>>,
) -> anyhow::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT s.project, COUNT(*) FROM events e JOIN streams s ON s.id = e.stream_id
         WHERE e.ts >= ?1 AND e.ts < ?2 AND e.type IN ('prompt', 'message') AND s.project IS NOT NULL
         GROUP BY s.project",
    )?;
    let rows = stmt
        .query_map(params![start_ms, end_ms], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut by_name: HashMap<String, i64> = HashMap::new();
    for (project, count) in rows {
        if !project_passes(Some(&project), filter) {
            continue;
        }
        *by_name.entry(excerpt::last_path_segment(&project).to_string()).or_insert(0) += count;
    }
    let mut names: Vec<(String, i64)> = by_name.into_iter().collect();
    names.sort_by(|(a_name, a_count), (b_name, b_count)| b_count.cmp(a_count).then_with(|| a_name.cmp(b_name)));
    Ok(names)
}

// ── 사실 신호(DB 집계) ────────────────────────────────────────────

#[derive(Debug, Default, PartialEq)]
struct ActivityStats {
    active_days: i64,
    sessions: i64,
    prompts: i64,
    github_events: i64,
    linear_issues: i64,
    slack_messages: i64,
}

/// 프로젝트 필터를 적용한 기간 통계. `period::get_period_stats`와 같은 정의(세션 = claude_code/
/// kiro_cli의 agent 아닌 스트림, Linear = 이슈 스트림 수)지만 프로젝트로 좁힐 수 있어야 해서 따로 센다.
fn activity_stats(
    conn: &Connection,
    start_ms: i64,
    end_ms: i64,
    zone: &Tz,
    filter: Option<&HashSet<String>>,
) -> anyhow::Result<ActivityStats> {
    let mut stats = ActivityStats::default();

    let mut stmt = conn.prepare(
        "SELECT s.project, e.source, e.type, COUNT(*) FROM events e JOIN streams s ON s.id = e.stream_id
         WHERE e.ts >= ?1 AND e.ts < ?2 GROUP BY s.project, e.source, e.type",
    )?;
    let rows = stmt.query_map(params![start_ms, end_ms], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (project, source, event_type, count) = row?;
        if !project_passes(project.as_deref(), filter) {
            continue;
        }
        if event_type == "prompt" {
            stats.prompts += count;
        }
        match source.as_str() {
            "github" => stats.github_events += count,
            "slack" => stats.slack_messages += count,
            _ => {}
        }
    }

    let mut stmt = conn.prepare(
        "SELECT s.project, s.source, COUNT(DISTINCT s.id) FROM events e JOIN streams s ON s.id = e.stream_id
         WHERE e.ts >= ?1 AND e.ts < ?2
           AND ((s.source IN ('claude_code', 'kiro_cli') AND s.kind != 'agent') OR s.source = 'linear')
         GROUP BY s.project, s.source",
    )?;
    let rows = stmt.query_map(params![start_ms, end_ms], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
    })?;
    for row in rows {
        let (project, source, count) = row?;
        if !project_passes(project.as_deref(), filter) {
            continue;
        }
        if source == "linear" {
            stats.linear_issues += count;
        } else {
            stats.sessions += count;
        }
    }

    let mut stmt = conn.prepare(
        "SELECT s.project, e.ts / ?3 FROM events e JOIN streams s ON s.id = e.stream_id
         WHERE e.ts >= ?1 AND e.ts < ?2 GROUP BY s.project, e.ts / ?3",
    )?;
    let rows = stmt.query_map(params![start_ms, end_ms, ACTIVITY_BUCKET_MS], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut days = HashSet::new();
    for row in rows {
        let (project, bucket) = row?;
        if !project_passes(project.as_deref(), filter) {
            continue;
        }
        if let Some(dt) = zone.timestamp_millis_opt(bucket * ACTIVITY_BUCKET_MS).single() {
            days.insert(dt.date_naive());
        }
    }
    stats.active_days = days.len() as i64;
    Ok(stats)
}

/// Linear 이슈 1개의 전이 기록(`capture/linear.rs`가 `externalId` 접미사 `#started`/`#completed`/
/// `#canceled`로 남긴 이벤트).
#[derive(Debug, Default)]
struct IssueTrack {
    key: String,
    title: String,
    started: Option<i64>,
    completed: Option<i64>,
    canceled: Option<i64>,
    last_ts: i64,
}

#[derive(Debug, Default, PartialEq)]
struct CarryOver {
    key: String,
    title: String,
    started: i64,
    last_ts: i64,
    stalled: bool,
}

#[derive(Debug, Default, PartialEq)]
struct LinearSignals {
    touched: usize,
    completed: usize,
    canceled: usize,
    /// 기간 중 완료한 이슈의 시작→완료 중앙값(0.1일 단위 — 실측 중앙값이 하루 미만이라 정수 일로는
    /// "0일"이 된다). 시작 기록이 있는 이슈만.
    median_cycle_tenths: Option<i64>,
    carry_over: Vec<CarryOver>,
}

/// 기간 중 손댄(이벤트가 한 건이라도 있는) Linear 이슈의 완료·취소·미완료·정체를 센다. `as_of`는
/// "기간 말" 기준 시각 — 진행 중인 기간이면 지금이다. 이슈가 기간 전에 시작했어도 기간 중 손댔다면
/// 포함한다(분기 경계를 넘긴 일을 놓치지 않기 위함).
fn linear_signals(
    conn: &Connection,
    start_ms: i64,
    end_ms: i64,
    as_of: i64,
    filter: Option<&HashSet<String>>,
) -> anyhow::Result<LinearSignals> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.title, s.project, e.external_id, e.ts FROM events e JOIN streams s ON s.id = e.stream_id
         WHERE s.source = 'linear' AND e.ts < ?2
           AND s.id IN (SELECT DISTINCT stream_id FROM events WHERE source = 'linear' AND ts >= ?1 AND ts < ?2)
         ORDER BY s.id, e.ts",
    )?;
    let rows = stmt.query_map(params![start_ms, end_ms], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;

    let mut tracks: Vec<IssueTrack> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let (stream_id, title, project, external_id, ts) = row?;
        if !project_passes(project.as_deref(), filter) {
            continue;
        }
        let slot = *index.entry(stream_id.clone()).or_insert_with(|| {
            let key = stream_id.strip_prefix("linear:").unwrap_or(&stream_id).to_string();
            tracks.push(IssueTrack {
                title: title.clone().unwrap_or_else(|| key.clone()),
                key,
                ..IssueTrack::default()
            });
            tracks.len() - 1
        });
        let track = &mut tracks[slot];
        track.last_ts = track.last_ts.max(ts);
        match external_id.as_deref().and_then(|id| id.rsplit_once('#')).map(|(_, suffix)| suffix) {
            Some("started") => track.started = Some(ts),
            Some("completed") => track.completed = Some(ts),
            Some("canceled") => track.canceled = Some(ts),
            _ => {}
        }
    }

    let in_period = |ts: Option<i64>| ts.is_some_and(|t| t >= start_ms && t < end_ms);
    let stall_before = as_of - STALL_DAYS * DAY_MS;
    let mut signals = LinearSignals { touched: tracks.len(), ..LinearSignals::default() };
    let mut cycle_tenths: Vec<i64> = Vec::new();
    for track in &tracks {
        if in_period(track.completed) {
            signals.completed += 1;
            if let (Some(started), Some(completed)) = (track.started, track.completed) {
                if completed >= started {
                    cycle_tenths.push(((completed - started) * 10 + DAY_MS / 2) / DAY_MS);
                }
            }
        }
        if in_period(track.canceled) {
            signals.canceled += 1;
        }
        let finished = track.completed.is_some() || track.canceled.is_some();
        if let (Some(started), false) = (track.started, finished) {
            signals.carry_over.push(CarryOver {
                key: track.key.clone(),
                title: truncate_with_ellipsis(&track.title, ISSUE_TITLE_MAX_CHARS),
                started,
                last_ts: track.last_ts,
                stalled: track.last_ts < stall_before,
            });
        }
    }
    cycle_tenths.sort_unstable();
    signals.median_cycle_tenths = median(&cycle_tenths);
    // 정체가 먼저, 그다음 오래 멈춘 순 — 목록이 잘려도 가장 오래 멈춘 일이 남는다.
    signals
        .carry_over
        .sort_by(|a, b| b.stalled.cmp(&a.stalled).then_with(|| a.last_ts.cmp(&b.last_ts)));
    Ok(signals)
}

/// 정렬된 값의 중앙값(짝수 개면 가운데 두 값의 평균, 반올림). 프롬프트가 이 수치를 그대로 인용하므로
/// 표본이 적을 때도 정확해야 한다.
fn median(sorted: &[i64]) -> Option<i64> {
    let n = sorted.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(sorted[n / 2]),
        _ => Some((sorted[n / 2 - 1] + sorted[n / 2] + 1) / 2),
    }
}

#[derive(Debug, Default, PartialEq)]
struct GithubSignals {
    prs_opened: usize,
    prs_merged: usize,
    issues_opened: usize,
    issues_closed: usize,
    comments_reviews: usize,
}

/// `PR #12 opened: ...`/`Issue #3 closed: ...` 제목에서 (종류, 번호, 동작)을 뽑는다. `capture/github.rs`가
/// 남기는 형식이다 — Events API는 `opened/merged/closed/reopened`, Search 백필은 `생성`.
fn parse_github_title(title: &str) -> Option<(&str, u64, &str)> {
    let (kind, rest) = if let Some(rest) = title.strip_prefix("PR #") {
        ("pr", rest)
    } else if let Some(rest) = title.strip_prefix("Issue #") {
        ("issue", rest)
    } else {
        return None;
    };
    let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let number: u64 = rest[..digits_end].parse().ok()?;
    let action = rest[digits_end..].trim_start().split([':', ' ']).next().unwrap_or("");
    Some((kind, number, action))
}

fn github_signals(
    conn: &Connection,
    start_ms: i64,
    end_ms: i64,
    filter: Option<&HashSet<String>>,
) -> anyhow::Result<GithubSignals> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.project, e.title FROM events e JOIN streams s ON s.id = e.stream_id
         WHERE e.source = 'github' AND e.ts >= ?1 AND e.ts < ?2",
    )?;
    let rows = stmt.query_map(params![start_ms, end_ms], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?))
    })?;

    // 같은 PR이 Events(`opened`)와 Search 백필(`생성`) 양쪽에서 들어올 수 있어 (저장소, 번호)로 중복을 뺀다.
    let mut prs_opened = HashSet::new();
    let mut prs_merged = HashSet::new();
    let mut issues_opened = HashSet::new();
    let mut issues_closed = HashSet::new();
    let mut comments_reviews = 0;
    for row in rows {
        let (stream_id, project, title) = row?;
        if !project_passes(project.as_deref(), filter) {
            continue;
        }
        let Some(title) = title else { continue };
        if title.starts_with("코멘트/리뷰") {
            comments_reviews += 1;
            continue;
        }
        let Some((kind, number, action)) = parse_github_title(&title) else { continue };
        let key = (stream_id, number);
        match (kind, action) {
            ("pr", "opened" | "생성") => {
                prs_opened.insert(key);
            }
            ("pr", "merged") => {
                prs_merged.insert(key);
            }
            ("issue", "opened" | "생성") => {
                issues_opened.insert(key);
            }
            ("issue", "closed") => {
                issues_closed.insert(key);
            }
            _ => {}
        }
    }
    Ok(GithubSignals {
        prs_opened: prs_opened.len(),
        prs_merged: prs_merged.len(),
        issues_opened: issues_opened.len(),
        issues_closed: issues_closed.len(),
        comments_reviews,
    })
}

/// 활동 추이 한 칸(분기 = 한 주, 월간 점검 = 하루).
#[derive(Debug, Default, PartialEq)]
struct SeriesPoint {
    /// 분기면 그 주 월요일, 월간 점검이면 그 날(로컬 "YYYY-MM-DD").
    key: String,
    /// 내 요청(AI 세션 `prompt`).
    requests: i64,
    /// Slack·GitHub·Linear 활동(메시지·PR·이슈·코멘트·전이).
    tool_activity: i64,
    /// Linear 이슈 완료.
    completed: i64,
}

/// 기간을 분기면 주 단위, 월간 점검이면 일 단위로 나눠 활동을 센다. 활동이 없는 칸도 0으로 채워
/// 공백이 보이게 하고, 오늘 이후 칸은 만들지 않는다(진행 중인 기간에 빈 미래가 "활동 없음"으로 읽히지
/// 않게). 평가서 화면의 "활동 추이" 차트 전용 — LLM 발췌에는 넣지 않는다(활동량은 판단 근거가 아니다).
#[allow(clippy::too_many_arguments)]
fn activity_series(
    conn: &Connection,
    period_type: &str,
    start_ms: i64,
    end_ms: i64,
    now: i64,
    zone: &Tz,
    filter: Option<&HashSet<String>>,
) -> anyhow::Result<Vec<SeriesPoint>> {
    let weekly = period_type == REVIEW_TYPE_QUARTER;
    let bucket_key = |date: NaiveDate| -> NaiveDate {
        if weekly {
            date - chrono::Duration::days(i64::from(date.weekday().num_days_from_monday()))
        } else {
            date
        }
    };
    let local_date = |ms: i64| zone.timestamp_millis_opt(ms).single().map(|dt| dt.date_naive());
    let (Some(first_date), Some(last_date)) = (local_date(start_ms), local_date(end_ms.min(now + 1) - 1)) else {
        return Ok(Vec::new());
    };

    let mut points: Vec<SeriesPoint> = Vec::new();
    let mut index: HashMap<NaiveDate, usize> = HashMap::new();
    let step = if weekly { 7 } else { 1 };
    let mut cursor = bucket_key(first_date);
    while cursor <= last_date {
        index.insert(cursor, points.len());
        points.push(SeriesPoint { key: cursor.format("%Y-%m-%d").to_string(), ..SeriesPoint::default() });
        cursor += chrono::Duration::days(step);
    }

    let mut stmt = conn.prepare(
        "SELECT s.project, e.type = 'prompt',
                e.source = 'linear' AND e.external_id LIKE '%#completed',
                e.ts / ?3, COUNT(*)
         FROM events e JOIN streams s ON s.id = e.stream_id
         WHERE e.ts >= ?1 AND e.ts < ?2 AND (e.type = 'prompt' OR e.source IN ('slack', 'github', 'linear'))
         GROUP BY s.project, 2, 3, 4",
    )?;
    let rows = stmt.query_map(params![start_ms, end_ms, ACTIVITY_BUCKET_MS], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, bool>(1)?,
            row.get::<_, bool>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (project, is_prompt, is_completed, bucket, count) = row?;
        if !project_passes(project.as_deref(), filter) {
            continue;
        }
        let Some(date) = local_date(bucket * ACTIVITY_BUCKET_MS) else { continue };
        let Some(&slot) = index.get(&bucket_key(date)) else { continue };
        let point = &mut points[slot];
        if is_prompt {
            point.requests += count;
        } else {
            point.tool_activity += count;
            if is_completed {
                point.completed += count;
            }
        }
    }
    Ok(points)
}

/// 요일(월=0)×시(0~23)별 내 요청·메시지 수(로컬 시각). 평가서 화면의 "시간·요일 분포" — 사용자가
/// 켤 때만 보이는 옵션이다(기본 꺼짐: 감시 도구처럼 읽힐 수 있어서). LLM 발췌에는 넣지 않는다.
fn activity_rhythm(
    conn: &Connection,
    start_ms: i64,
    end_ms: i64,
    zone: &Tz,
    filter: Option<&HashSet<String>>,
) -> anyhow::Result<[[i64; 24]; 7]> {
    let mut counts = [[0_i64; 24]; 7];
    let mut stmt = conn.prepare(
        "SELECT s.project, e.ts / ?3, COUNT(*) FROM events e JOIN streams s ON s.id = e.stream_id
         WHERE e.ts >= ?1 AND e.ts < ?2 AND e.type IN ('prompt', 'message')
         GROUP BY s.project, e.ts / ?3",
    )?;
    let rows = stmt.query_map(params![start_ms, end_ms, ACTIVITY_BUCKET_MS], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
    })?;
    for row in rows {
        let (project, bucket, count) = row?;
        if !project_passes(project.as_deref(), filter) {
            continue;
        }
        // 15분 버킷은 실존하는 모든 UTC 오프셋(15분 배수)에서 로컬 한 시간 안에 들어간다.
        if let Some(dt) = zone.timestamp_millis_opt(bucket * ACTIVITY_BUCKET_MS).single() {
            let weekday = dt.weekday().num_days_from_monday() as usize;
            counts[weekday][dt.hour() as usize] += count;
        }
    }
    Ok(counts)
}

/// 평가서를 만든 시점의 DB 집계 — 발췌의 "근거 범위·사실 신호"와 화면의 숫자 타일·차트가 같은 값을
/// 보도록 한 번만 센다.
struct ReviewSnapshot {
    stats: ActivityStats,
    linear: LinearSignals,
    github: GithubSignals,
    series: Vec<SeriesPoint>,
    projects: Vec<(String, i64)>,
    rhythm: [[i64; 24]; 7],
}

fn compute_snapshot(
    conn: &Connection,
    period_type: &str,
    period_key: &str,
    tz: &str,
    now: i64,
    filter: Option<&HashSet<String>>,
) -> anyhow::Result<ReviewSnapshot> {
    let zone: Tz = tz.parse().map_err(|_| anyhow::anyhow!("invalid timezone: {tz}"))?;
    let (start_ms, end_ms, _) = review_range(period_type, period_key, tz)?;
    Ok(ReviewSnapshot {
        stats: activity_stats(conn, start_ms, end_ms, &zone, filter)?,
        linear: linear_signals(conn, start_ms, end_ms, now.min(end_ms), filter)?,
        github: github_signals(conn, start_ms, end_ms, filter)?,
        series: activity_series(conn, period_type, start_ms, end_ms, now, &zone, filter)?,
        projects: project_activity(conn, start_ms, end_ms, filter)?,
        rhythm: activity_rhythm(conn, start_ms, end_ms, &zone, filter)?,
    })
}

/// 스냅샷의 FE 계약(`ReviewStats`, camelCase).
fn snapshot_json(snapshot: &ReviewSnapshot, period_type: &str) -> Value {
    let linear = &snapshot.linear;
    let github = &snapshot.github;
    json!({
        "activeDays": snapshot.stats.active_days,
        "sessions": snapshot.stats.sessions,
        "prompts": snapshot.stats.prompts,
        "githubEvents": snapshot.stats.github_events,
        "linearIssues": snapshot.stats.linear_issues,
        "slackMessages": snapshot.stats.slack_messages,
        "linear": {
            "touched": linear.touched,
            "completed": linear.completed,
            "canceled": linear.canceled,
            "carryOver": linear.carry_over.len(),
            "stalled": linear.carry_over.iter().filter(|c| c.stalled).count(),
            "medianCycleTenths": linear.median_cycle_tenths,
        },
        "github": {
            "prsOpened": github.prs_opened,
            "prsMerged": github.prs_merged,
            "issuesOpened": github.issues_opened,
            "issuesClosed": github.issues_closed,
            "commentsReviews": github.comments_reviews,
        },
        "series": {
            "unit": if period_type == REVIEW_TYPE_QUARTER { "week" } else { "day" },
            "points": snapshot.series.iter().map(|p| json!({
                "key": p.key,
                "requests": p.requests,
                "toolActivity": p.tool_activity,
                "completed": p.completed,
            })).collect::<Vec<_>>(),
        },
        "projects": snapshot.projects.iter().map(|(name, activity)| json!({ "name": name, "activity": activity })).collect::<Vec<_>>(),
        // 요일(월=0)×시(0~23) 행렬.
        "rhythm": snapshot.rhythm,
    })
}

// ── 발췌 조립 ─────────────────────────────────────────────────────

/// 평가서를 만드는 입력 — 캐시 행 `inputs` 컬럼에 그대로 남는다.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReviewInputs {
    /// 포함 프로젝트 표시명. `None` = 전체.
    pub projects: Option<Vec<String>>,
    pub goals: Option<String>,
    pub profile: ReviewProfile,
}

impl ReviewInputs {
    /// FE에서 받은 값을 정규화한다 — 프로젝트는 trim·중복 제거(빈 목록이면 no_data: 고른 게 없으면
    /// 평가할 것도 없다), 목표는 trim 후 비면 `None`·[`MAX_GOALS_CHARS`]자로 자름, 프로필은 정규화.
    pub fn normalized(projects: Option<Vec<String>>, goals: Option<String>, profile: ReviewProfile) -> anyhow::Result<Self> {
        let projects = match projects {
            None => None,
            Some(list) => {
                let mut seen = HashSet::new();
                let cleaned: Vec<String> = list
                    .into_iter()
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty() && seen.insert(p.clone()))
                    .collect();
                if cleaned.is_empty() {
                    anyhow::bail!("{NO_DATA_ERROR_CODE}");
                }
                Some(cleaned)
            }
        };
        let goals = goals
            .map(|g| g.trim().chars().take(MAX_GOALS_CHARS).collect::<String>())
            .filter(|g| !g.trim().is_empty());
        Ok(Self { projects, goals, profile: profile.normalized() })
    }

    fn to_json(&self) -> Value {
        json!({
            "projects": self.projects,
            "goals": self.goals,
            "profile": self.profile,
        })
    }
}

fn level_label(level: &str, ko: bool) -> Option<&'static str> {
    Some(match (level, ko) {
        ("junior", true) => "주니어 (~3년)",
        ("mid", true) => "미들 (4~7년)",
        ("senior", true) => "시니어 (8년~)",
        ("lead", true) => "리드·매니저",
        ("junior", false) => "Junior (~3 yrs)",
        ("mid", false) => "Mid-level (4-7 yrs)",
        ("senior", false) => "Senior (8+ yrs)",
        ("lead", false) => "Lead / Manager",
        _ => return None,
    })
}

fn format_local_date(ms: i64, zone: &Tz) -> String {
    zone.timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn format_month_day(ms: i64, zone: &Tz) -> String {
    zone.timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.format("%m-%d").to_string())
        .unwrap_or_default()
}

/// 평가 대상자 블록.
fn render_person(profile: &ReviewProfile, ko: bool) -> String {
    let mut out = String::from(if ko { "## 평가 대상자\n" } else { "## Person\n" });
    if profile.is_empty() {
        out.push_str(if ko {
            "- 입력 없음 — 공통 기준으로 평가\n"
        } else {
            "- Not provided — use common criteria\n"
        });
        return out;
    }
    let not_set = if ko { "미지정" } else { "not set" };
    let role = if profile.role.is_empty() { not_set } else { profile.role.as_str() };
    let level = level_label(&profile.level, ko).unwrap_or(not_set);
    let manages = match (profile.manages_people, ko) {
        (true, true) => "함",
        (false, true) => "안 함 또는 미지정",
        (true, false) => "yes",
        (false, false) => "no or not set",
    };
    if ko {
        out.push_str(&format!("- 직무: {role}\n- 연차 구간: {level}\n- 팀원 관리: {manages}\n"));
    } else {
        out.push_str(&format!("- Role: {role}\n- Level: {level}\n- Manages people: {manages}\n"));
    }
    out
}

fn render_goals(goals: Option<&str>, ko: bool) -> String {
    let heading = if ko { "## 기간 목표" } else { "## Goals for the period" };
    let body = goals.unwrap_or(if ko {
        "입력 없음 — 기록만으로 평가"
    } else {
        "Not provided — review from records only"
    });
    format!("{heading}\n{body}\n")
}

fn render_stats_line(stats: &ActivityStats, ko: bool) -> String {
    let mut parts = vec![if ko {
        format!("활동일 {}일", stats.active_days)
    } else {
        format!("{} active days", stats.active_days)
    }];
    let items: [(i64, &str, &str); 5] = [
        (stats.sessions, "세션 {}개", "{} sessions"),
        (stats.prompts, "요청 {}건", "{} requests"),
        (stats.github_events, "GitHub 활동 {}건", "{} GitHub events"),
        (stats.linear_issues, "Linear 이슈 {}개", "{} Linear issues"),
        (stats.slack_messages, "Slack 메시지 {}건", "{} Slack messages"),
    ];
    for (value, ko_tpl, en_tpl) in items {
        if value > 0 {
            parts.push((if ko { ko_tpl } else { en_tpl }).replace("{}", &value.to_string()));
        }
    }
    parts.join(" · ")
}

#[allow(clippy::too_many_arguments)]
fn render_scope(
    ko: bool,
    start_ms: i64,
    end_ms: i64,
    now: i64,
    zone: &Tz,
    projects: Option<&[String]>,
    stats: &ActivityStats,
    used: &[String],
    missing: &[String],
) -> String {
    let start = format_local_date(start_ms, zone);
    let last_day = format_local_date(end_ms - 1, zone);
    let period = if now < end_ms {
        let today = format_local_date(now, zone);
        if ko {
            format!("{start} ~ {today} (진행 중, 기간 끝 {last_day})")
        } else {
            format!("{start} – {today} (in progress, ends {last_day})")
        }
    } else if ko {
        format!("{start} ~ {last_day}")
    } else {
        format!("{start} – {last_day}")
    };
    let project_line = match projects {
        None => (if ko { "전체" } else { "All" }).to_string(),
        Some(list) => list.join(", "),
    };
    let mut out = String::new();
    if ko {
        out.push_str("## 근거 범위\n");
        out.push_str(&format!("- 기간: {period}\n"));
        out.push_str(&format!("- 포함 프로젝트: {project_line}\n"));
        out.push_str(&format!("- 활동 요약: {}\n", render_stats_line(stats, true)));
        out.push_str(&format!("- 사용한 요약: {}", used.join(" · ")));
        if !missing.is_empty() {
            out.push_str(&format!(" (요약 없음: {})", missing.join(" · ")));
        }
        out.push_str("\n- 기록되지 않는 일: 회의·대면 대화·메일·문서 작성처럼 이 앱이 캡처하지 않는 활동은 들어 있지 않다.\n");
    } else {
        out.push_str("## Evidence scope\n");
        out.push_str(&format!("- Period: {period}\n"));
        out.push_str(&format!("- Projects: {project_line}\n"));
        out.push_str(&format!("- Activity: {}\n", render_stats_line(stats, false)));
        out.push_str(&format!("- Summaries used: {}", used.join(" · ")));
        if !missing.is_empty() {
            out.push_str(&format!(" (no summary: {})", missing.join(" · ")));
        }
        out.push_str("\n- Not recorded: meetings, in-person conversations, email, and document writing that this app does not capture.\n");
    }
    out
}

fn render_signals(linear: &LinearSignals, github: &GithubSignals, zone: &Tz, ko: bool) -> String {
    let mut out = String::from(if ko {
        "## 사실 신호 (DB 집계 — 다시 세지 말고 그대로 인용)\n"
    } else {
        "## Fact signals (counted from the database — quote as-is, do not recount)\n"
    });
    let mut any = false;
    if linear.touched > 0 {
        any = true;
        let cycle = linear.median_cycle_tenths.map(|t| {
            let days = format!("{}.{}", t / 10, t % 10);
            if ko {
                format!("(시작→완료 중앙값 {days}일)")
            } else {
                format!("(median start→done {days} days)")
            }
        });
        let stalled = linear.carry_over.iter().filter(|c| c.stalled).count();
        if ko {
            out.push_str(&format!(
                "- Linear: 기간 중 다룬 이슈 {}개 · 완료 {}개{} · 취소 {}개\n",
                linear.touched,
                linear.completed,
                cycle.map(|c| format!(" {c}")).unwrap_or_default(),
                linear.canceled
            ));
            out.push_str(&format!(
                "- 기간 말 미완료 {}개 (그중 {STALL_DAYS}일 넘게 움직임 없음 {stalled}개)\n",
                linear.carry_over.len()
            ));
        } else {
            out.push_str(&format!(
                "- Linear: {} issues touched · {} completed{} · {} canceled\n",
                linear.touched,
                linear.completed,
                cycle.map(|c| format!(" {c}")).unwrap_or_default(),
                linear.canceled
            ));
            out.push_str(&format!(
                "- Unfinished at period end: {} (of which {stalled} with no activity for {STALL_DAYS}+ days)\n",
                linear.carry_over.len()
            ));
        }
        for item in linear.carry_over.iter().take(MAX_CARRY_OVER_LINES) {
            let started = format_month_day(item.started, zone);
            let last = format_month_day(item.last_ts, zone);
            let stalled_mark = match (item.stalled, ko) {
                (true, true) => format!(" · {STALL_DAYS}일+ 정체"),
                (true, false) => format!(" · stalled {STALL_DAYS}+ days"),
                _ => String::new(),
            };
            if ko {
                out.push_str(&format!("  - {} — 시작 {started} · 마지막 활동 {last}{stalled_mark}\n", item.title));
            } else {
                out.push_str(&format!("  - {} — started {started} · last activity {last}{stalled_mark}\n", item.title));
            }
        }
    }
    // 0인 항목은 뺀다 — 특히 PR 머지는 GitHub Events API가 본인 머지를 늘 내려주지 않아 0으로 찍히는
    // 경우가 많은데(실측 3분기 0건), "머지 0"을 보면 LLM이 "PR을 하나도 머지하지 못했다"로 읽는다.
    let github_items: [(usize, &str, &str); 5] = [
        (github.prs_opened, "PR 생성 {}", "{} PRs opened"),
        (github.prs_merged, "PR 머지 {}", "{} PRs merged"),
        (github.issues_opened, "이슈 생성 {}", "{} issues opened"),
        (github.issues_closed, "이슈 종료 {}", "{} issues closed"),
        (github.comments_reviews, "코멘트·리뷰 {}", "{} comments/reviews"),
    ];
    let github_parts: Vec<String> = github_items
        .iter()
        .filter(|(value, _, _)| *value > 0)
        .map(|(value, ko_tpl, en_tpl)| (if ko { ko_tpl } else { en_tpl }).replace("{}", &value.to_string()))
        .collect();
    if !github_parts.is_empty() {
        any = true;
        out.push_str(&format!("- GitHub: {}\n", github_parts.join(" · ")));
    }
    if !any {
        out.push_str(if ko {
            "- Linear·GitHub 기록 없음\n"
        } else {
            "- No Linear or GitHub records\n"
        });
    }
    out
}

fn lower_heading(unit: &LowerUnit, ko: bool) -> anyhow::Result<String> {
    Ok(match (unit.kind, ko) {
        ("month", true) => format!("# {} 월간 요약", unit.key),
        ("month", false) => format!("# {} monthly summary", unit.key),
        (_, _) => {
            let monday = NaiveDate::parse_from_str(&unit.key, "%Y-%m-%d")?;
            let sunday = monday + chrono::Duration::days(6);
            if ko {
                format!("# {monday} ~ {sunday} 주간 요약")
            } else {
                format!("# Week of {monday} – {sunday}")
            }
        }
    })
}

/// 하위 요약 1개를 가져온다 — 캐시가 온전하면 그대로, 없거나 기간을 다 담지 못했으면 한 단계만 다시
/// 만든다(그 아래는 `CachedOnly`). 데이터가 없는 기간(no_data)은 `None`. 다시 만들다 no_data가 나면
/// 기존 캐시를 그대로 쓴다(덜 온전해도 없는 것보다 낫다). 그 밖의 실패(엔진 미설정 등)는 전파한다.
async fn lower_summary(
    db: &crate::Db,
    unit: &LowerUnit,
    tz: &str,
    locale: &str,
    cfg: &SummaryConfig,
    now: i64,
) -> anyhow::Result<Option<String>> {
    let cached = {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
        period::get_cached(&conn, unit.kind, &unit.key, locale)?
    };
    let cached_content = cached.as_ref().and_then(|c| c["content"].as_str().map(str::to_string));
    if let Some(cached) = &cached {
        let created_at = cached["createdAt"].as_i64().unwrap_or(0);
        if !is_incomplete(created_at, unit.end_ms, now) {
            return Ok(cached_content);
        }
    }
    match Box::pin(period::generate_and_cache(
        db,
        unit.kind,
        &unit.key,
        tz,
        locale,
        None,
        cfg,
        period::Cascade::CachedOnly,
    ))
    .await
    {
        Ok(generated) => Ok(generated["content"].as_str().map(str::to_string)),
        Err(e) if e.to_string() == NO_DATA_ERROR_CODE => Ok(cached_content),
        Err(e) => Err(e),
    }
}

/// 평가서 발췌를 만든다. `preview_performance_review_input` 커맨드가 그대로 노출하고("미리보기 = 실제
/// 전송본"), [`generate_and_cache`]도 override가 없을 때 이 함수로 발췌를 만든다. 하위 요약이 하나도
/// 없으면 no_data.
pub async fn build_excerpt(
    db: &crate::Db,
    period_type: &str,
    period_key: &str,
    tz: &str,
    locale: &str,
    inputs: &ReviewInputs,
    cfg: &SummaryConfig,
) -> anyhow::Result<String> {
    build_excerpt_at(db, period_type, period_key, tz, locale, inputs, cfg, now_ms()).await
}

#[allow(clippy::too_many_arguments)]
async fn build_excerpt_at(
    db: &crate::Db,
    period_type: &str,
    period_key: &str,
    tz: &str,
    locale: &str,
    inputs: &ReviewInputs,
    cfg: &SummaryConfig,
    now: i64,
) -> anyhow::Result<String> {
    let ko = locale == "ko";
    let zone: Tz = tz.parse().map_err(|_| anyhow::anyhow!("invalid timezone: {tz}"))?;
    let (start_ms, end_ms, units) = review_range(period_type, period_key, tz)?;
    let filter: Option<HashSet<String>> = inputs.projects.as_ref().map(|list| list.iter().cloned().collect());

    let mut blocks: Vec<String> = Vec::new();
    let mut used = Vec::new();
    let mut missing = Vec::new();
    for unit in &units {
        // 미래의 하위 기간(진행 중인 분기의 아직 안 온 달)은 조회조차 하지 않는다 — 생성 시도가 곧
        // 불필요한 엔진 호출이다.
        let (unit_start, _) = match unit.kind {
            "month" => {
                let (s, e, _) = period::month_range(&unit.key, tz)?;
                (s, e)
            }
            _ => day_range_ms(&unit.key, tz)?,
        };
        if unit_start > now {
            continue;
        }
        let label = unit.key.clone();
        let content = lower_summary(db, unit, tz, locale, cfg, now).await?;
        let content = match (content, &filter) {
            (Some(text), Some(keep)) => Some(filter_project_sections(&text, keep)).filter(|t| !t.is_empty()),
            (content, _) => content,
        };
        match content {
            Some(text) => {
                used.push(label);
                blocks.push(format!("{}\n\n{text}", lower_heading(unit, ko)?));
            }
            None => missing.push(label),
        }
    }
    if blocks.is_empty() {
        anyhow::bail!("{NO_DATA_ERROR_CODE}");
    }

    let snapshot = {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
        compute_snapshot(&conn, period_type, period_key, tz, now, filter.as_ref())?
    };
    let (stats, linear, github) = (&snapshot.stats, &snapshot.linear, &snapshot.github);

    let title = match (period_type, ko) {
        (REVIEW_TYPE_QUARTER, true) => format!("# 대상 분기: {period_key}\n평가 종류: 분기 업무평가"),
        (REVIEW_TYPE_QUARTER, false) => format!("# Target quarter: {period_key}\nType: quarterly review"),
        (_, true) => format!("# 대상 월: {period_key}\n평가 종류: 월간 점검"),
        (_, false) => format!("# Target month: {period_key}\nType: monthly check-in"),
    };
    let mut sections = vec![
        title,
        render_person(&inputs.profile, ko),
        render_goals(inputs.goals.as_deref(), ko),
        render_scope(ko, start_ms, end_ms, now, &zone, inputs.projects.as_deref(), stats, &used, &missing),
        render_signals(linear, github, &zone, ko),
    ];
    sections.extend(blocks);

    // 블록 사이는 빈 줄 하나로 통일한다(각 렌더 함수의 끝 줄바꿈 유무와 무관하게).
    let joined = sections.iter().map(|s| s.trim_end()).collect::<Vec<_>>().join("\n\n");
    let scrubbed = scrub::scrub_text(joined.trim_end());
    Ok(truncate_with_ellipsis(&scrubbed, MAX_EXCERPT_CHARS))
}

// ── performance_reviews 캐시(V12) ─────────────────────────────────

/// `(period_type, period_key, locale)` 평가서 1건. FE 계약(`PerformanceReview`, camelCase)과 1:1.
pub fn get_cached(conn: &Connection, period_type: &str, period_key: &str, locale: &str) -> anyhow::Result<Option<Value>> {
    conn.query_row(
        "SELECT tz, engine, model, content, inputs, stats, prompt_version, created_at FROM performance_reviews
         WHERE period_type = ?1 AND period_key = ?2 AND locale = ?3",
        params![period_type, period_key, locale],
        |row| {
            let inputs_raw: String = row.get("inputs")?;
            let stats_raw: String = row.get("stats")?;
            Ok(json!({
                "periodType": period_type,
                "periodKey": period_key,
                "locale": locale,
                "tz": row.get::<_, String>("tz")?,
                "engine": row.get::<_, String>("engine")?,
                "model": row.get::<_, String>("model")?,
                "content": row.get::<_, String>("content")?,
                "inputs": serde_json::from_str::<Value>(&inputs_raw).unwrap_or_else(|_| json!({})),
                // 빈 객체(`{}`)는 스냅샷 없음 — FE는 null로 받아 타일·차트를 숨긴다.
                "stats": serde_json::from_str::<Value>(&stats_raw)
                    .ok()
                    .filter(|v| v.as_object().is_some_and(|o| !o.is_empty())),
                "promptVersion": row.get::<_, String>("prompt_version")?,
                "createdAt": row.get::<_, i64>("created_at")?,
            }))
        },
    )
    .optional()
    .map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn upsert(
    conn: &Connection,
    period_type: &str,
    period_key: &str,
    locale: &str,
    tz: &str,
    engine: &str,
    model: &str,
    content: &str,
    inputs: &ReviewInputs,
    stats: &Value,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO performance_reviews
           (period_type, period_key, locale, tz, engine, model, content, inputs, stats, prompt_version, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(period_type, period_key, locale) DO UPDATE SET
           tz = excluded.tz,
           engine = excluded.engine,
           model = excluded.model,
           content = excluded.content,
           inputs = excluded.inputs,
           stats = excluded.stats,
           prompt_version = excluded.prompt_version,
           created_at = excluded.created_at",
        params![
            period_type,
            period_key,
            locale,
            tz,
            engine,
            model,
            content,
            inputs.to_json().to_string(),
            stats.to_string(),
            prompts::REVIEW_PROMPT_VERSION,
            now_ms()
        ],
    )?;
    Ok(())
}

/// 발췌(override 우선) → 평가 프롬프트 → 엔진 호출 → `performance_reviews` upsert. 호출할 때마다 새로
/// 만든다(재생성 = 같은 호출). enabled/캡처 일시정지 확인은 호출부(커맨드) 책임.
#[allow(clippy::too_many_arguments)]
pub async fn generate_and_cache(
    db: &crate::Db,
    period_type: &str,
    period_key: &str,
    tz: &str,
    locale: &str,
    inputs: &ReviewInputs,
    excerpt_override: Option<String>,
    cfg: &SummaryConfig,
) -> anyhow::Result<Value> {
    // 종류 검증을 발췌보다 먼저 한다 — override 경로는 발췌 생성을 건너뛰므로 여기서 거르지 않으면
    // 잘못된 종류로 엔진까지 간다.
    let prompt = prompts::review_prompt(period_type, locale)?;
    let excerpt_text = match excerpt_override {
        Some(text) => excerpt::sanitize_excerpt_override(&text)?,
        None => build_excerpt(db, period_type, period_key, tz, locale, inputs, cfg).await?,
    };
    let combined = format!("{prompt}\n\n{excerpt_text}");
    let generated = engine::generate(&combined, cfg).await?;

    let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
    // 스냅샷은 발췌와 같은 입력(프로젝트 필터)으로 다시 센다 — 순수 SQL이라 싸고, 미리보기 수정본으로
    // 만든 경우(override)에도 화면의 숫자가 비지 않는다.
    let filter: Option<HashSet<String>> = inputs.projects.as_ref().map(|list| list.iter().cloned().collect());
    let snapshot = compute_snapshot(&conn, period_type, period_key, tz, now_ms(), filter.as_ref())?;
    upsert(
        &conn,
        period_type,
        period_key,
        locale,
        tz,
        &generated.engine,
        &generated.model,
        &generated.content,
        inputs,
        &snapshot_json(&snapshot, period_type),
    )?;
    get_cached(&conn, period_type, period_key, locale)?
        .ok_or_else(|| anyhow::anyhow!("평가서 저장 후 조회에 실패했습니다"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    const TZ: &str = "Asia/Seoul";

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("logroom-review-test-{}-{n}-{nanos}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn seeded_conn() -> (TempDir, Connection) {
        let tmp = TempDir::new();
        let conn = db::open_and_migrate_at(&tmp.path.join("logroom.db")).expect("마이그레이션 성공");
        (tmp, conn)
    }

    fn insert_stream(conn: &Connection, id: &str, source: &str, kind: &str, project: Option<&str>, title: &str) {
        conn.execute(
            "INSERT INTO streams (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, 0, 0, 'active', '{}', 0)",
            params![id, source, kind, title, project],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_event(conn: &Connection, id: &str, stream_id: &str, ts: i64, source: &str, event_type: &str, title: &str, external_id: &str) {
        conn.execute(
            "INSERT INTO events (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, NULL, NULL, NULL, NULL, ?7, '{}', ?3)",
            params![id, stream_id, ts, source, event_type, title, external_id],
        )
        .unwrap();
    }

    /// 로컬(Asia/Seoul) 날짜의 정오 epoch ms.
    fn noon(local_date: &str) -> i64 {
        day_range_ms(local_date, TZ).unwrap().0 + 12 * 3_600_000
    }

    fn keep(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // ── 기간 계산 ─────────────────────────────────────────────────

    #[test]
    fn quarter_range_covers_three_calendar_months() {
        let (start_ms, end_ms, months) = quarter_range("2026-Q3", TZ).unwrap();
        assert_eq!(months, vec!["2026-07", "2026-08", "2026-09"]);
        assert_eq!(start_ms, day_range_ms("2026-07-01", TZ).unwrap().0);
        assert_eq!(end_ms, day_range_ms("2026-09-30", TZ).unwrap().1);

        let (_, _, q4) = quarter_range("2026-Q4", TZ).unwrap();
        assert_eq!(q4, vec!["2026-10", "2026-11", "2026-12"]);
    }

    #[test]
    fn quarter_range_rejects_malformed_keys() {
        for key in ["2026-Q0", "2026-Q5", "2026-07", "Q3-2026", "2026-Qx"] {
            assert!(quarter_range(key, TZ).is_err(), "{key} 는 거부돼야 함");
        }
    }

    #[test]
    fn review_range_uses_months_for_quarter_and_weeks_for_month() {
        let (_, _, units) = review_range("quarter", "2026-Q3", TZ).unwrap();
        assert_eq!(units.iter().map(|u| u.kind).collect::<Vec<_>>(), vec!["month"; 3]);

        let (_, _, units) = review_range("month", "2026-07", TZ).unwrap();
        assert!(units.iter().all(|u| u.kind == "week"));
        assert_eq!(units.first().unwrap().key, "2026-06-29", "1일이 속한 주(월요일은 전달)부터");

        assert!(review_range("week", "2026-07-13", TZ).is_err());
    }

    #[test]
    fn is_incomplete_flags_cache_made_before_period_end() {
        let end = noon("2026-09-30");
        let after_end = end + 5 * DAY_MS;
        assert!(is_incomplete(end - 20 * DAY_MS, end, after_end), "기간 끝나기 전 캐시는 다시 만든다");
        assert!(!is_incomplete(end + DAY_MS, end, after_end), "기간 끝난 뒤 만든 캐시는 그대로 쓴다");

        // 진행 중인 기간: 하루 안에 만든 캐시는 그대로, 하루 넘은 캐시는 다시.
        let now = end - 10 * DAY_MS;
        assert!(!is_incomplete(now - 3_600_000, end, now));
        assert!(is_incomplete(now - 2 * DAY_MS, end, now));
    }

    // ── 프로젝트 필터 ─────────────────────────────────────────────

    #[test]
    fn filter_project_sections_keeps_only_selected_projects() {
        let content = "> 활동일 20일 · 전체 총평\n\n## logroom\n### 요약\n> 완료\n- 07-01 요약 개편\n\n## acme-admin\n- 07-02 결제 화면\n\n## 정리\n- logroom — 완료\n- acme-admin — 진행 중";
        let filtered = filter_project_sections(content, &keep(&["acme-admin"]));
        assert_eq!(filtered, "## acme-admin\n- 07-02 결제 화면");
        assert!(!filtered.contains("logroom"), "뺀 프로젝트가 정리·총평으로 새면 안 됨");
        assert_eq!(filter_project_sections(content, &keep(&["없는-프로젝트"])), "");
    }

    #[test]
    fn project_passes_matches_display_name_and_excludes_unknown_when_filtered() {
        let filter = keep(&["logroom"]);
        assert!(project_passes(Some("/Users/x/src/logroom"), Some(&filter)));
        assert!(!project_passes(Some("/Users/x/src/acme-admin"), Some(&filter)));
        assert!(!project_passes(None, Some(&filter)));
        assert!(project_passes(None, None));
    }

    #[test]
    fn review_inputs_normalize_projects_and_goals() {
        let inputs = ReviewInputs::normalized(
            Some(vec![" a ".into(), "a".into(), "".into(), "b".into()]),
            Some("   ".into()),
            ReviewProfile::default(),
        )
        .unwrap();
        assert_eq!(inputs.projects, Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(inputs.goals, None, "공백뿐인 목표는 입력 없음");

        let long = "가".repeat(MAX_GOALS_CHARS + 5);
        let inputs = ReviewInputs::normalized(None, Some(long), ReviewProfile::default()).unwrap();
        assert_eq!(inputs.goals.unwrap().chars().count(), MAX_GOALS_CHARS);

        let err = ReviewInputs::normalized(Some(vec![" ".into()]), None, ReviewProfile::default()).unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE, "고른 프로젝트가 없으면 평가할 것도 없다");
    }

    // ── 기간 내 프로젝트 목록 ─────────────────────────────────────

    #[test]
    fn list_review_projects_counts_my_requests_and_messages_by_display_name() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(&conn, "cc:1", "claude_code", "session", Some("/a/logroom"), "s1");
        insert_stream(&conn, "cc:2", "claude_code", "session", Some("/b/logroom"), "s2");
        insert_stream(&conn, "gh:1", "github", "thread", Some("owner/acme-admin"), "repo");
        insert_event(&conn, "e1", "cc:1", noon("2026-07-10"), "claude_code", "prompt", "p", "x1");
        insert_event(&conn, "e2", "cc:2", noon("2026-08-10"), "claude_code", "prompt", "p", "x2");
        insert_event(&conn, "e3", "cc:2", noon("2026-08-10"), "claude_code", "tool_use", "t", "x3");
        insert_event(&conn, "e4", "gh:1", noon("2026-09-01"), "github", "message", "PR #1 opened: a", "x4");
        insert_event(&conn, "e5", "gh:1", noon("2026-10-01"), "github", "message", "PR #2 opened: b", "x5");

        let projects = list_review_projects(&conn, "quarter", "2026-Q3", TZ).unwrap();
        assert_eq!(
            projects,
            vec![json!({"name": "logroom", "activity": 2}), json!({"name": "acme-admin", "activity": 1})],
            "경로가 달라도 표시명이 같으면 합치고, tool_use·기간 밖은 세지 않는다"
        );
    }

    // ── 사실 신호 ─────────────────────────────────────────────────

    fn seed_linear(conn: &Connection) {
        insert_stream(conn, "linear:FEAT-1", "linear", "task", Some("신규기능"), "FEAT-1 완료된 이슈");
        insert_event(conn, "l1", "linear:FEAT-1", noon("2026-07-01"), "linear", "message", "FEAT-1 시작", "ln:1#started");
        insert_event(conn, "l2", "linear:FEAT-1", noon("2026-07-05"), "linear", "message", "FEAT-1 완료", "ln:1#completed");

        // 분기 전에 시작해 분기 중 손댔지만 끝나지 않고 오래 멈춘 이슈.
        insert_stream(conn, "linear:FEAT-2", "linear", "task", Some("신규기능"), "FEAT-2 멈춘 이슈");
        insert_event(conn, "l3", "linear:FEAT-2", noon("2026-06-20"), "linear", "message", "FEAT-2 시작", "ln:2#started");
        insert_event(conn, "l4", "linear:FEAT-2", noon("2026-07-10"), "linear", "message", "FEAT-2 코멘트", "ln:2#comment:9");

        // 최근까지 움직인 미완료 이슈(정체 아님).
        insert_stream(conn, "linear:FEAT-3", "linear", "task", Some("신규기능"), "FEAT-3 진행 중");
        insert_event(conn, "l5", "linear:FEAT-3", noon("2026-09-20"), "linear", "message", "FEAT-3 시작", "ln:3#started");

        insert_stream(conn, "linear:TICKET-4", "linear", "task", Some("Maintenance"), "TICKET-4 취소");
        insert_event(conn, "l6", "linear:TICKET-4", noon("2026-08-01"), "linear", "message", "TICKET-4 시작", "ln:4#started");
        insert_event(conn, "l7", "linear:TICKET-4", noon("2026-08-03"), "linear", "message", "TICKET-4 취소", "ln:4#canceled");
    }

    #[test]
    fn linear_signals_count_completion_cancel_carry_over_and_stall() {
        let (_tmp, conn) = seeded_conn();
        seed_linear(&conn);
        let (start, end, _) = quarter_range("2026-Q3", TZ).unwrap();

        let signals = linear_signals(&conn, start, end, end, None).unwrap();
        assert_eq!(signals.touched, 4);
        assert_eq!(signals.completed, 1);
        assert_eq!(signals.canceled, 1);
        assert_eq!(signals.median_cycle_tenths, Some(40));
        assert_eq!(signals.carry_over.len(), 2, "FEAT-2(분기 전 시작)·FEAT-3 이 미완료");
        assert_eq!(signals.carry_over[0].key, "FEAT-2", "정체가 먼저 온다");
        assert!(signals.carry_over[0].stalled);
        assert!(!signals.carry_over[1].stalled, "9/20 시작은 30일 안이라 정체 아님");
    }

    #[test]
    fn linear_signals_respect_project_filter() {
        let (_tmp, conn) = seeded_conn();
        seed_linear(&conn);
        let (start, end, _) = quarter_range("2026-Q3", TZ).unwrap();

        let signals = linear_signals(&conn, start, end, end, Some(&keep(&["Maintenance"]))).unwrap();
        assert_eq!(signals.touched, 1);
        assert_eq!(signals.canceled, 1);
        assert!(signals.carry_over.is_empty());
    }

    #[test]
    fn median_averages_two_middles_for_even_samples() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[10, 20, 30]), Some(20));
        assert_eq!(median(&[10, 20, 30, 40]), Some(25), "2.0일·3.0일의 평균 2.5일");
    }

    #[test]
    fn parse_github_title_reads_kind_number_and_action() {
        assert_eq!(parse_github_title("PR #42 opened: Add feature"), Some(("pr", 42, "opened")));
        assert_eq!(parse_github_title("PR #42 merged: Add feature"), Some(("pr", 42, "merged")));
        assert_eq!(parse_github_title("PR #7 생성: 제목"), Some(("pr", 7, "생성")));
        assert_eq!(parse_github_title("Issue #3 closed:"), Some(("issue", 3, "closed")));
        assert_eq!(parse_github_title("Issue #3 closed"), Some(("issue", 3, "closed")));
        assert_eq!(parse_github_title("코멘트/리뷰: x"), None);
        assert_eq!(parse_github_title("branch 생성: main"), None);
    }

    #[test]
    fn github_signals_dedupe_backfill_and_events_per_repo() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(&conn, "github:o/a", "github", "thread", Some("o/a"), "o/a");
        insert_stream(&conn, "github:o/b", "github", "thread", Some("o/b"), "o/b");
        let day = noon("2026-08-01");
        insert_event(&conn, "g1", "github:o/a", day, "github", "message", "PR #1 opened: x", "g1");
        insert_event(&conn, "g2", "github:o/a", day, "github", "message", "PR #1 생성: x", "g2");
        insert_event(&conn, "g3", "github:o/b", day, "github", "message", "PR #1 opened: y", "g3");
        insert_event(&conn, "g4", "github:o/a", day, "github", "message", "Issue #9 closed: z", "g4");
        insert_event(&conn, "g5", "github:o/a", day, "github", "message", "코멘트/리뷰: x", "g5");
        let (start, end, _) = quarter_range("2026-Q3", TZ).unwrap();

        let signals = github_signals(&conn, start, end, None).unwrap();
        assert_eq!(signals.prs_opened, 2, "같은 저장소의 같은 PR은 한 번, 다른 저장소의 #1은 따로");
        assert_eq!(signals.issues_closed, 1);
        assert_eq!(signals.comments_reviews, 1);

        let only_b = github_signals(&conn, start, end, Some(&keep(&["b"]))).unwrap();
        assert_eq!(only_b.prs_opened, 1);
        assert_eq!(only_b.issues_closed, 0);
    }

    #[test]
    fn render_signals_omits_zero_github_items() {
        let zone: Tz = TZ.parse().unwrap();
        let github = GithubSignals { prs_opened: 3, comments_reviews: 2, ..GithubSignals::default() };
        let text = render_signals(&LinearSignals::default(), &github, &zone, true);
        assert!(text.contains("- GitHub: PR 생성 3 · 코멘트·리뷰 2"));
        assert!(!text.contains("머지"), "0건인 PR 머지를 적으면 LLM이 '머지 못 함'으로 읽는다");

        let empty = render_signals(&LinearSignals::default(), &GithubSignals::default(), &zone, true);
        assert!(empty.contains("Linear·GitHub 기록 없음"));
    }

    #[test]
    fn activity_stats_filters_by_project() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(&conn, "cc:1", "claude_code", "session", Some("/x/logroom"), "s1");
        insert_stream(&conn, "cc:2", "claude_code", "session", Some("/x/acme-admin"), "s2");
        insert_stream(&conn, "cc:3", "claude_code", "agent", Some("/x/logroom"), "agent");
        insert_event(&conn, "e1", "cc:1", noon("2026-07-01"), "claude_code", "prompt", "p", "1");
        insert_event(&conn, "e2", "cc:1", noon("2026-07-02"), "claude_code", "prompt", "p", "2");
        insert_event(&conn, "e3", "cc:2", noon("2026-07-03"), "claude_code", "prompt", "p", "3");
        insert_event(&conn, "e4", "cc:3", noon("2026-07-04"), "claude_code", "prompt", "p", "4");
        let (start, end, _) = quarter_range("2026-Q3", TZ).unwrap();
        let zone: Tz = TZ.parse().unwrap();

        let all = activity_stats(&conn, start, end, &zone, None).unwrap();
        assert_eq!(all, ActivityStats { active_days: 4, sessions: 2, prompts: 4, ..ActivityStats::default() });

        let only = activity_stats(&conn, start, end, &zone, Some(&keep(&["logroom"]))).unwrap();
        assert_eq!(only, ActivityStats { active_days: 3, sessions: 1, prompts: 3, ..ActivityStats::default() });
    }

    #[test]
    fn activity_series_buckets_by_week_for_quarter_and_stops_at_today() {
        let (_tmp, conn) = seeded_conn();
        seed_linear(&conn);
        insert_stream(&conn, "cc:1", "claude_code", "session", Some("/x/logroom"), "s1");
        insert_event(&conn, "p1", "cc:1", noon("2026-07-07"), "claude_code", "prompt", "p", "p1");
        insert_event(&conn, "p2", "cc:1", noon("2026-07-08"), "claude_code", "prompt", "p", "p2");
        insert_event(&conn, "t1", "cc:1", noon("2026-07-08"), "claude_code", "tool_use", "t", "t1");
        let (start, end, _) = quarter_range("2026-Q3", TZ).unwrap();
        let zone: Tz = TZ.parse().unwrap();

        let now = noon("2026-07-20");
        let points = activity_series(&conn, "quarter", start, end, now, &zone, None).unwrap();
        let keys: Vec<&str> = points.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, vec!["2026-06-29", "2026-07-06", "2026-07-13", "2026-07-20"], "오늘(7/20)이 속한 주까지만");
        assert_eq!(points[0].tool_activity, 2, "7/1 FEAT-1 시작 + 7/5 완료");
        assert_eq!(points[0].completed, 1);
        assert_eq!(points[1].requests, 2, "tool_use 는 요청이 아니다");
        assert_eq!(points[1].tool_activity, 1, "7/10 FEAT-2 코멘트");

        let only = activity_series(&conn, "quarter", start, end, now, &zone, Some(&keep(&["logroom"]))).unwrap();
        assert_eq!(only.iter().map(|p| p.tool_activity).sum::<i64>(), 0, "뺀 프로젝트의 Linear 활동은 세지 않는다");
    }

    #[test]
    fn activity_series_uses_days_for_month_check_in() {
        let (_tmp, conn) = seeded_conn();
        let (start, end, _) = period::month_range("2026-07", TZ).unwrap();
        let zone: Tz = TZ.parse().unwrap();
        let points = activity_series(&conn, "month", start, end, noon("2026-08-15"), &zone, None).unwrap();
        assert_eq!(points.len(), 31);
        assert_eq!(points.first().unwrap().key, "2026-07-01");
        assert_eq!(points.last().unwrap().key, "2026-07-31");
    }

    #[test]
    fn activity_rhythm_counts_by_local_weekday_and_hour() {
        let (_tmp, conn) = seeded_conn();
        insert_stream(&conn, "cc:1", "claude_code", "session", Some("/x/logroom"), "s1");
        insert_stream(&conn, "cc:2", "claude_code", "session", Some("/x/acme-admin"), "s2");
        // 2026-07-06 월요일 KST 23:30, 2026-07-11 토요일 KST 10:05
        let monday_2330 = day_range_ms("2026-07-06", TZ).unwrap().0 + (23 * 60 + 30) * 60_000;
        let saturday_1005 = day_range_ms("2026-07-11", TZ).unwrap().0 + (10 * 60 + 5) * 60_000;
        insert_event(&conn, "e1", "cc:1", monday_2330, "claude_code", "prompt", "p", "1");
        insert_event(&conn, "e2", "cc:1", saturday_1005, "claude_code", "prompt", "p", "2");
        insert_event(&conn, "e3", "cc:2", saturday_1005, "claude_code", "prompt", "p", "3");
        insert_event(&conn, "e4", "cc:1", saturday_1005, "claude_code", "tool_use", "t", "4");
        let (start, end, _) = quarter_range("2026-Q3", TZ).unwrap();
        let zone: Tz = TZ.parse().unwrap();

        let all = activity_rhythm(&conn, start, end, &zone, None).unwrap();
        assert_eq!(all[0][23], 1, "월요일 23시");
        assert_eq!(all[5][10], 2, "토요일 10시 — tool_use 는 세지 않는다");

        let only = activity_rhythm(&conn, start, end, &zone, Some(&keep(&["logroom"]))).unwrap();
        assert_eq!(only[5][10], 1);
    }

    #[test]
    fn snapshot_json_exposes_counts_series_and_project_shares() {
        let (_tmp, conn) = seeded_conn();
        seed_linear(&conn);
        let snapshot = compute_snapshot(&conn, "quarter", "2026-Q3", TZ, noon("2026-10-05"), None).unwrap();
        let value = snapshot_json(&snapshot, "quarter");
        assert_eq!(value["linear"]["completed"], json!(1));
        assert_eq!(value["linear"]["stalled"], json!(1));
        assert_eq!(value["linear"]["medianCycleTenths"], json!(40));
        assert_eq!(value["series"]["unit"], json!("week"));
        assert_eq!(value["projects"][0]["name"], json!("신규기능"));
    }

    // ── 발췌 조립(엔진 호출 없는 경로만) ──────────────────────────

    fn put_summary(conn: &Connection, kind: &str, key: &str, locale: &str, content: &str, created_at: i64) {
        period::upsert(conn, kind, key, locale, TZ, "cli", "sonnet", content).unwrap();
        conn.execute(
            "UPDATE period_summaries SET created_at = ?1 WHERE period_type = ?2 AND period_key = ?3 AND locale = ?4",
            params![created_at, kind, key, locale],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn build_excerpt_assembles_person_goals_scope_signals_and_filtered_months() {
        let (_tmp, conn) = seeded_conn();
        seed_linear(&conn);
        let after_quarter = noon("2026-10-05");
        put_summary(&conn, "month", "2026-07", "ko", "## 신규기능\n- 07-05 FEAT-1 완료\n\n## logroom\n- 07-03 개인 작업", after_quarter);
        put_summary(&conn, "month", "2026-08", "ko", "## 신규기능\n- 08-10 기능 B", after_quarter);
        put_summary(&conn, "month", "2026-09", "ko", "## 신규기능\n- 09-20 FEAT-3 시작", after_quarter);
        let db: crate::Db = Arc::new(Mutex::new(conn));

        let inputs = ReviewInputs::normalized(
            Some(vec!["신규기능".into()]),
            Some("결제 화면 출시".into()),
            ReviewProfile { role: "프론트엔드 개발자".into(), level: "senior".into(), manages_people: false },
        )
        .unwrap();
        let text = build_excerpt_at(&db, "quarter", "2026-Q3", TZ, "ko", &inputs, &SummaryConfig::default(), after_quarter)
            .await
            .unwrap();

        assert!(text.starts_with("# 대상 분기: 2026-Q3"));
        assert!(text.contains("- 직무: 프론트엔드 개발자\n- 연차 구간: 시니어 (8년~)"));
        assert!(text.contains("## 기간 목표\n결제 화면 출시"));
        assert!(text.contains("- 기간: 2026-07-01 ~ 2026-09-30"));
        assert!(text.contains("- 포함 프로젝트: 신규기능"));
        assert!(text.contains("- 사용한 요약: 2026-07 · 2026-08 · 2026-09"));
        assert!(text.contains("- Linear: 기간 중 다룬 이슈 3개 · 완료 1개 (시작→완료 중앙값 4.0일) · 취소 0개"));
        assert!(text.contains("FEAT-2 멈춘 이슈 — 시작 06-20 · 마지막 활동 07-10 · 30일+ 정체"));
        assert!(text.contains("# 2026-07 월간 요약\n\n## 신규기능"));
        assert!(!text.contains("개인 작업"), "빼기로 한 프로젝트 섹션이 발췌에 남으면 안 됨");
        assert!(!text.contains("TICKET-4"), "빼기로 한 프로젝트의 Linear 신호도 빠져야 함");
    }

    #[tokio::test]
    async fn build_excerpt_skips_future_months_and_marks_missing_ones() {
        let (_tmp, conn) = seeded_conn();
        let now = noon("2026-08-15");
        put_summary(&conn, "month", "2026-07", "ko", "## logroom\n- 07-03 작업", now);
        // 8월은 진행 중이고 하위(주간) 캐시가 없어 CachedOnly 재생성이 no_data → 요약 없음으로 표시.
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let inputs = ReviewInputs::default();

        let text = build_excerpt_at(&db, "quarter", "2026-Q3", TZ, "ko", &inputs, &SummaryConfig::default(), now)
            .await
            .unwrap();
        assert!(text.contains("(진행 중, 기간 끝 2026-09-30)"));
        assert!(text.contains("- 사용한 요약: 2026-07 (요약 없음: 2026-08)"), "9월은 아직 오지 않아 목록에도 없다: {text}");
        assert!(text.contains("- 입력 없음 — 공통 기준으로 평가"));
        assert!(text.contains("입력 없음 — 기록만으로 평가"));
    }

    #[tokio::test]
    async fn build_excerpt_returns_no_data_without_any_lower_summary() {
        let (_tmp, conn) = seeded_conn();
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let err = build_excerpt_at(
            &db,
            "quarter",
            "2026-Q3",
            TZ,
            "ko",
            &ReviewInputs::default(),
            &SummaryConfig::default(),
            noon("2026-10-05"),
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[tokio::test]
    async fn incomplete_cached_month_falls_back_when_regeneration_has_no_data() {
        // 기간 종료 전에 만든 7월 요약 — 다시 만들려 해도 하위(주간) 캐시가 없어 no_data. 이때 기존
        // 캐시를 버리지 않고 그대로 써야 한다(엔진 호출 없이 끝나는 경로).
        let (_tmp, conn) = seeded_conn();
        put_summary(&conn, "month", "2026-07", "ko", "## logroom\n- 07-03 작업", noon("2026-07-10"));
        let db: crate::Db = Arc::new(Mutex::new(conn));

        let text = build_excerpt_at(
            &db,
            "quarter",
            "2026-Q3",
            TZ,
            "ko",
            &ReviewInputs::default(),
            &SummaryConfig::default(),
            noon("2026-10-05"),
        )
        .await
        .unwrap();
        assert!(text.contains("07-03 작업"));
    }

    #[tokio::test]
    async fn month_check_in_uses_weekly_summaries() {
        let (_tmp, conn) = seeded_conn();
        let after = noon("2026-08-10");
        put_summary(&conn, "week", "2026-07-06", "en", "## logroom\n- 07-07 작업", after);
        let db: crate::Db = Arc::new(Mutex::new(conn));

        let text = build_excerpt_at(&db, "month", "2026-07", TZ, "en", &ReviewInputs::default(), &SummaryConfig::default(), after)
            .await
            .unwrap();
        assert!(text.starts_with("# Target month: 2026-07\nType: monthly check-in"));
        assert!(text.contains("# Week of 2026-07-06 – 2026-07-12\n\n## logroom"));
    }

    // ── 캐시 ──────────────────────────────────────────────────────

    #[test]
    fn upsert_then_get_cached_round_trips_inputs_and_overwrites() {
        let (_tmp, conn) = seeded_conn();
        assert_eq!(get_cached(&conn, "quarter", "2026-Q3", "ko").unwrap(), None);

        let inputs = ReviewInputs::normalized(Some(vec!["a".into()]), Some("목표".into()), ReviewProfile::default()).unwrap();
        let stats = json!({ "activeDays": 3, "series": { "unit": "week", "points": [] } });
        upsert(&conn, "quarter", "2026-Q3", "ko", TZ, "cli", "sonnet", "## 잘한 것\n- x", &inputs, &stats).unwrap();
        let cached = get_cached(&conn, "quarter", "2026-Q3", "ko").unwrap().unwrap();
        assert_eq!(cached["inputs"]["projects"], json!(["a"]));
        assert_eq!(cached["inputs"]["goals"], json!("목표"));
        assert_eq!(cached["inputs"]["profile"]["managesPeople"], json!(false));
        assert_eq!(cached["promptVersion"], json!(prompts::REVIEW_PROMPT_VERSION));
        assert_eq!(cached["stats"]["activeDays"], json!(3));

        upsert(&conn, "quarter", "2026-Q3", "ko", TZ, "api", "haiku", "## 잘한 것\n- y", &ReviewInputs::default(), &json!({})).unwrap();
        let recached = get_cached(&conn, "quarter", "2026-Q3", "ko").unwrap().unwrap();
        assert_eq!(recached["content"], json!("## 잘한 것\n- y"));
        assert_eq!(recached["inputs"]["projects"], Value::Null);
        assert_eq!(recached["stats"], Value::Null, "빈 스냅샷은 null — FE가 타일·차트를 숨긴다");
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM performance_reviews", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn generate_rejects_invalid_type_before_touching_engine() {
        let (_tmp, conn) = seeded_conn();
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let err = generate_and_cache(
            &db,
            "week",
            "2026-07-13",
            TZ,
            "ko",
            &ReviewInputs::default(),
            Some("임의 발췌".into()),
            &SummaryConfig::default(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("invalid review periodType"));
    }
}
