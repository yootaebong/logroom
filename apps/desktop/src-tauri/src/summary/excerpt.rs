//! 일일 AI 요약 발췌 생성(M7-①, ADR-0016) — 순수 SQL + 포맷, 실측 검증된 규칙.
//! `engine::generate`에 넘길 "발췌 텍스트"를 하루치 캡처 데이터(세션형 소스 + 커넥터 3종)에서
//! 뽑아낸다. 시간창은 `query::get_digest`와 동일하게 chrono-tz 로컬 자정 기준
//! `[00:00, 24:00)`(query::day_range_ms 재사용).

use crate::capture::policy::{
    truncate_with_ellipsis, MEANINGFUL_PROMPT_MIN_CHARS, STREAM_TITLE_MAX_CHARS,
};
use crate::capture::scrub;
use crate::query::day_range_ms;
use chrono::TimeZone;
use chrono_tz::Tz;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// 전체 발췌 안전컷(코드포인트 수). LLM 입력 폭주를 막기 위한 최종 방어선.
/// `pub(crate)`: `summary::period`(M7-③)의 주간/월간 합성 발췌도 동일한 안전컷을 재사용한다.
///
/// 16,000이던 시절 실사용 데이터 손실 버그가 발견됐다: 실측 하루 최대 prompt 461개(2026-07-01) ×
/// prompt 라인 최대 90자 + 시각 접두(`HH:MM `) ≈ 45,000자로, 16,000이면 그날 활동의 상당 부분이
/// 중간에 통째로 잘려나간다. 64,000으로 올려 하루 활동 전량을 담을 여유를 뒀었다.
///
/// 96,000(이번 개정): "요청 → 결과" 문맥 보강으로 각 prompt 라인 아래에 응답 요지(최대
/// [`REPLY_MAX_CHARS`]=100자, `  └ ` 하위 줄)가 따라붙어 라인당 분량이 약 2배가 됐다. 실측 하루
/// 최대 prompt 461개 기준 약 87,000자까지 커질 수 있어 64,000으로는 다시 잘려나간다 — 96,000으로
/// 올려 그 여유를 확보하되, 이 값은 여전히 LLM 입력 폭주(비정상적으로 큰 하루 데이터)를 막는 최종
/// 방어선으로 남긴다.
pub(crate) const MAX_EXCERPT_CHARS: usize = 96_000;

// 스트림 제목 절단 길이(60)·prompt 라인 채택 최소 길이(15)는 capture/policy.rs의 pub const를
// 직접 재사용한다 — 로컬 중복 선언 시 policy 쪽 값 변경에 조용히 어긋나는 드리프트 위험(리뷰 지적).
/// prompt 라인 최대 길이(코드포인트 수).
const PROMPT_LINE_MAX_CHARS: usize = 90;

/// 응답 요지(reply) 최대 길이(코드포인트 수) — prompt 직후 응답 페어링과 prompt 0건 세션의 응답
/// 전용 폴백 라인이 [`refine_reply_text`]를 통해 공유한다.
const REPLY_MAX_CHARS: usize = 100;

/// prompt를 하나도 채택하지 못한 세션에서 response만으로 채울 최대 라인 수(폴백).
const NO_PROMPT_FALLBACK_MAX_LINES: usize = 5;

/// GitHub repo당 최대 채택 이벤트 title 수.
const MAX_GITHUB_ITEMS_PER_REPO: usize = 8;
/// GitHub 이벤트 title 최대 길이.
const GITHUB_TITLE_MAX_CHARS: usize = 80;

/// Linear 이슈 스트림 최대 채택 수(활동 수 내림차순).
const MAX_LINEAR_ITEMS: usize = 15;
/// Linear 이슈 title 최대 길이.
const LINEAR_TITLE_MAX_CHARS: usize = 80;

/// 발췌 레벨에서 걸러낼 노이즈 prompt 프리픽스 — capture/policy.rs::META_PROMPT_PREFIXES(캡처
/// 시점 필터)와는 별개의 집합이다. 아래 패턴들은 캡처 시점에는 걸러지지 않고 그대로 저장되지만
/// (멀티 에이전트 완료 알림·컨텍스트 압축 요약 등), 하루 요약용 발췌에는 노이즈이므로 여기서만
/// 제외한다.
const EXCERPT_NOISE_PREFIXES: &[&str] = &[
    "Another Claude session sent",
    "This session is being continued",
    "<teammate-message",
    "[Request interrupted",
];

fn is_excerpt_noise(text: &str) -> bool {
    EXCERPT_NOISE_PREFIXES.iter().any(|p| text.starts_with(p))
}

/// project 문자열에서 마지막 `/` 세그먼트(표시 이름) — FE `lib/utils.ts::lastPathSegment`와 동일
/// 로직의 Rust 이식(발췌 헤더 표기용). `pub`: `summary::resume`(M7-②)가 프로젝트 표시명 매칭에
/// 그대로 재사용한다(중복 구현 시 드리프트 위험).
pub fn last_path_segment(path: &str) -> &str {
    path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path)
}

fn format_local_time(ms: i64, zone: &Tz) -> String {
    zone.timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.format("%H:%M").to_string())
        .unwrap_or_else(|| "--:--".to_string())
}

/// "알려진 프로젝트" 줄을 모을 기간(일) — 요약 대상 날짜의 끝 시각 기준으로 거슬러 센다.
const KNOWN_PROJECTS_WINDOW_DAYS: i64 = 30;
/// "알려진 프로젝트" 줄에 넣을 최대 프로젝트 수(최근 활동 순).
const MAX_KNOWN_PROJECTS: usize = 30;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// 코딩 에이전트 세션 스트림 1건(집계 로우).
struct SessionStreamRow {
    stream_id: String,
    title: Option<String>,
    project: Option<String>,
    prompts: i64,
    first_ts: i64,
    last_ts: i64,
    /// hub base 스트림(`capture/hub.rs` — 레포 밖 폴더에서 시작한 세션 중 레포 신호가 없어 자식
    /// 스트림 `<base>@<repo>` 로 옮겨지지 않고 남은 턴들). 발췌에서 프로젝트명 대신 "레포 미지정"
    /// 섹션으로 모은다.
    hub_base: bool,
    /// hub 자식 스트림(`metadata.hubStream` 있음) — project 가 이미 레포로 정해져 있다.
    hub_child: bool,
}

/// project 경로별 "레포 밖 폴더인가" 판정 캐시(발췌 1회 실행 동안만 유지 — FS 조회 반복 방지).
type OutsideRepoCache = HashMap<String, bool>;

/// `project` 가 레포 밖 폴더인지 — 존재하는 절대경로 디렉토리인데 상위로 `.git` 을 못 찾는 경우
/// (`capture::normalize::find_repo_root` 가 `None`). hub 로 표시되지 않은 세션도 레포 밖에서 시작했으면
/// (실측: Kiro CLI 세션이 project=`~/.agent-hub`) 시작 폴더 이름으로 섹션을 만들지 않게 한다. 지금은 없는
/// 경로(지운 레포·테스트 가짜 경로)는 판정할 근거가 없으니 레포 밖으로 보지 않는다.
fn is_outside_repo(project: &str, cache: &mut OutsideRepoCache) -> bool {
    if let Some(&cached) = cache.get(project) {
        return cached;
    }
    let path = Path::new(project);
    let outside =
        path.is_absolute() && path.is_dir() && crate::capture::normalize::find_repo_root(project).is_none();
    cache.insert(project.to_string(), outside);
    outside
}

fn fetch_session_streams(conn: &Connection, start_ms: i64, end_ms: i64) -> anyhow::Result<Vec<SessionStreamRow>> {
    // hub base = metadata.hub=true 이고 metadata.hubStream 이 없는 스트림(자식 스트림은 hubStream 을
    // 가진다). 서브에이전트(kind='agent') 스트림은 원래부터 발췌 대상이 아니다.
    let mut stmt = conn.prepare(
        "SELECT s.id AS stream_id, s.title AS title, s.project AS project,
                SUM(CASE WHEN e.type = 'prompt' THEN 1 ELSE 0 END) AS prompts,
                MIN(e.ts) AS first_ts, MAX(e.ts) AS last_ts,
                CASE WHEN json_valid(s.metadata) AND json_extract(s.metadata, '$.hub') = 1
                       AND json_extract(s.metadata, '$.hubStream') IS NULL
                     THEN 1 ELSE 0 END AS hub_base,
                CASE WHEN json_valid(s.metadata) AND json_extract(s.metadata, '$.hubStream') IS NOT NULL
                     THEN 1 ELSE 0 END AS hub_child
         FROM events e
         JOIN streams s ON s.id = e.stream_id
         WHERE e.ts >= ?1 AND e.ts < ?2
           AND s.source IN ('claude_code', 'kiro_cli')
           AND s.kind != 'agent'
         GROUP BY s.id
         ORDER BY prompts DESC, first_ts ASC",
    )?;
    let rows = stmt
        .query_map(params![start_ms, end_ms], |row| {
            Ok(SessionStreamRow {
                stream_id: row.get("stream_id")?,
                title: row.get("title")?,
                project: row.get("project")?,
                prompts: row.get("prompts")?,
                first_ts: row.get("first_ts")?,
                last_ts: row.get("last_ts")?,
                hub_base: row.get::<_, i64>("hub_base")? == 1,
                hub_child: row.get::<_, i64>("hub_child")? == 1,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// "알려진 프로젝트" 표시명 목록 — `end_ms` 기준 최근 [`KNOWN_PROJECTS_WINDOW_DAYS`]일 안에 활동이
/// 있는 스트림의 프로젝트를 표시명(`last_path_segment`)으로 묶어 최근 활동 순으로 최대
/// [`MAX_KNOWN_PROJECTS`]개. AI 가 "레포 미지정" 기록을 그날 섹션이 없던 프로젝트에도 넣을 수 있게
/// 후보를 알려 주는 용도다.
///
/// `query::list_projects` 는 기간 제한이 없고 hub 시작 폴더(`~/.agent-hub` 등)도 섞여 그대로 못 쓴다 —
/// 표시명 묶기 규칙(`last_path_segment`)만 같게 따른다. hub base 세션의 project 경로는 통째로 뺀다
/// (같은 폴더에 남은 서브에이전트 스트림도 함께 빠진다. 레포로 재귀속된 서브에이전트는 project 가
/// 레포라 남는다). 요약 대상 날짜 기준으로 세므로 지난 날짜를 다시 요약해도 결과가 같다.
/// 소스는 세션 발췌([`fetch_session_streams`])와 같은 범위(`claude_code`·`kiro_cli`)로 한정하고,
/// 레포 밖 폴더([`is_outside_repo`])와 캡처 제외 프로젝트(`exclude_projects`)도 뺀다.
fn fetch_known_projects(
    conn: &Connection,
    end_ms: i64,
    exclude_projects: &[String],
    outside_cache: &mut OutsideRepoCache,
) -> anyhow::Result<Vec<String>> {
    let since_ms = end_ms - KNOWN_PROJECTS_WINDOW_DAYS * DAY_MS;
    let mut stmt = conn.prepare(
        "SELECT project, MAX(COALESCE(ended_at, started_at)) AS last_ts
         FROM streams
         WHERE project IS NOT NULL
           AND source IN ('claude_code', 'kiro_cli')
           AND COALESCE(ended_at, started_at) >= ?1
           AND started_at < ?2
           AND project NOT IN (
             SELECT project FROM streams
             WHERE project IS NOT NULL AND kind != 'agent'
               AND json_valid(metadata) AND json_extract(metadata, '$.hub') = 1
               AND json_extract(metadata, '$.hubStream') IS NULL
           )
         GROUP BY project",
    )?;
    let rows = stmt
        .query_map(params![since_ms, end_ms], |row| {
            Ok((row.get::<_, String>("project")?, row.get::<_, i64>("last_ts")?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut latest_by_name: BTreeMap<String, i64> = BTreeMap::new();
    for (project, last_ts) in rows {
        if crate::capture::policy::is_summarizer_project(&project)
            || crate::capture::config::project_matches_exclude(&project, exclude_projects)
            || is_outside_repo(&project, outside_cache)
        {
            continue;
        }
        let name = last_path_segment(&project).to_string();
        let entry = latest_by_name.entry(name).or_insert(last_ts);
        *entry = (*entry).max(last_ts);
    }
    let mut names: Vec<(String, i64)> = latest_by_name.into_iter().collect();
    // 최근 활동 내림차순, 동률이면 이름 오름차순(BTreeMap 순서 + 안정 정렬).
    names.sort_by_key(|(_, ts)| std::cmp::Reverse(*ts));
    Ok(names.into_iter().take(MAX_KNOWN_PROJECTS).map(|(name, _)| name).collect())
}

fn known_projects_line(locale: &str, names: &[String]) -> String {
    let joined = names.join(", ");
    if locale == "ko" {
        format!("알려진 프로젝트: {joined}")
    } else {
        format!("Known projects: {joined}")
    }
}

/// "레포 미지정" 섹션 제목 — 프롬프트(`prompts.rs`)가 이 문자열을 그대로 가리키므로 바꾸면 함께 고친다.
fn unassigned_heading(locale: &str) -> &'static str {
    if locale == "ko" {
        "레포 미지정"
    } else {
        "Unassigned"
    }
}

/// 발췌용 prompt 라인 1건 — 정제된 요청 텍스트 + (있으면) 직후 응답 요지.
///
/// 배경: 발췌가 사용자 prompt만 담다 보니 "c로 진행", "1번만"처럼 지시대명사만 남는 요청은 나중에
/// 다시 봐도 무슨 작업인지 알 수 없다는 실사용 피드백을 받았다. `reply`는 그 요청 직후
/// `type='response'` 이벤트의 요지를 페어링해 문맥을 복원한다([`fetch_prompt_lines`] 참고).
///
/// `pub(crate)`: `summary::resume`(M7-②)도 `fetch_prompt_lines`를 통해 이 구조체를 그대로
/// 소비한다(resume은 `reply`를 쓰지 않고 `text`만 사용 — 사용자 결정: resume은 이번 개정 대상 제외).
pub(crate) struct PromptLine {
    pub ts: i64,
    pub text: String,
    /// 이 요청 직후 assistant 응답의 요지(없으면 None).
    pub reply: Option<String>,
}

/// 응답(`type='response'`) 이벤트에서 "응답 요지" 텍스트를 뽑는다 — title 우선(없으면 body) →
/// trim → 비어있거나 [`is_excerpt_noise`]면 `None` → 개행을 공백으로 치환 → 마크다운 볼드 표식
/// (`**`) 제거 → [`REPLY_MAX_CHARS`]로 절단(`truncate_with_ellipsis` 재사용).
///
/// prompt 직후 reply 페어링과 prompt 0건 세션의 응답 전용 폴백 라인(둘 다 [`fetch_prompt_lines`]
/// 내부)이 이 로직을 공유한다 — 둘 다 "response 이벤트 → 사람이 읽을 요지 문자열" 변환이라는
/// 점에서 동일하다.
fn refine_reply_text(title: Option<String>, body: Option<String>) -> Option<String> {
    let raw = title.filter(|s| !s.trim().is_empty()).or(body).unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() || is_excerpt_noise(trimmed) {
        return None;
    }
    let flattened = trimmed.replace('\n', " ").replace("**", "");
    Some(truncate_with_ellipsis(&flattened, REPLY_MAX_CHARS))
}

/// 스트림 하나의 prompt 이벤트에서 채택할 발췌 라인들([`PromptLine`] — ts/정제된 요청 텍스트/직후
/// 응답 요지). 각 prompt 이벤트의 title(없으면 body)을 trim → [`MEANINGFUL_PROMPT_MIN_CHARS`]
/// 이상만 → [`is_excerpt_noise`] 프리픽스 제외 → 개행을 공백으로 치환 → [`PROMPT_LINE_MAX_CHARS`]로
/// 절단(`truncate_with_ellipsis` 재사용 — 초과 시 말미에 `…`).
///
/// **요청 → 결과 페어링**: SQL이 `prompt`뿐 아니라 `response` 이벤트도 함께 ts 오름차순으로
/// 가져온다. 순회하면서 각 prompt 라인의 "매칭 구간"(그 prompt 직후 ~ 다음 prompt 이벤트 전)에서
/// **첫 번째** response만 그 라인의 `reply`로 채택한다(구간에 response가 없으면 `None`, 구간 안에
/// response가 여럿이면 두 번째부터는 버림). 다음 prompt 이벤트가 나타나면(그 자체가 채택되지
/// 못하고 필터링되는 경우 포함) 매칭 구간이 즉시 끝난다 — 그 이후의 response는 이전 prompt의
/// 결과가 아니라 그다음 요청의 결과일 가능성이 높기 때문이다.
///
/// **prompt 0건 폴백**: 위 로직으로 채택된 라인이 하나도 없으면(그 기간에 prompt 이벤트 자체가
/// 없거나, 전부 필터링됐거나) 응답만으로 라인을 채운다 — 노이즈가 아닌 response를 앞에서부터
/// 최대 [`NO_PROMPT_FALLBACK_MAX_LINES`]개까지 `PromptLine { reply: None, .. }`으로 반환한다(요약에
/// "기록된 요청 없음"으로만 나오던 실사용 케이스 방지).
///
/// `limit`이 `Some(n)`이면 **채택한 prompt 라인**이 n개가 되는 즉시 중단하고(response는 개수에
/// 포함하지 않는다), `None`이면 기간 내 전량을 반환한다. daily excerpt 경로
/// (`session_stream_to_project_line`)는 `None`으로 호출해 전량을 받고, resume 브리핑
/// (`summary::resume::render_recent_work_section`)만 "최근 작업" 요지가 목적이라
/// `Some(RECENT_WORK_PROMPT_LINES)`로 앞 5개만 채택한다.
///
/// `pub(crate)`: `summary::resume`(M7-②)도 동일한 라인 채택 규칙을 그대로 재사용한다(중복 구현 시
/// 필터 조건이 조용히 드리프트할 위험 — 리뷰 지적 방지).
pub(crate) fn fetch_prompt_lines(
    conn: &Connection,
    stream_id: &str,
    start_ms: i64,
    end_ms: i64,
    limit: Option<usize>,
) -> anyhow::Result<Vec<PromptLine>> {
    let mut stmt = conn.prepare(
        "SELECT ts, type, title, body FROM events
         WHERE stream_id = ?1 AND type IN ('prompt', 'response') AND ts >= ?2 AND ts < ?3
         ORDER BY ts ASC",
    )?;
    let rows: Vec<(i64, String, Option<String>, Option<String>)> = stmt
        .query_map(params![stream_id, start_ms, end_ms], |row| {
            let ts: i64 = row.get("ts")?;
            let event_type: String = row.get("type")?;
            let title: Option<String> = row.get("title")?;
            let body: Option<String> = row.get("body")?;
            Ok((ts, event_type, title, body))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut lines: Vec<PromptLine> = Vec::new();
    // 채택된 마지막 prompt 라인(`lines`의 인덱스) 중 아직 reply를 못 채운 라인 — 다음 response가
    // 오면 여기에 채우고 소비한다(`take()`로 한 번만). 새 prompt 이벤트를 만나면 즉시 리셋해
    // 매칭 구간을 그 prompt로 넘긴다.
    let mut pending_reply_idx: Option<usize> = None;

    for (ts, event_type, title, body) in rows.iter().cloned() {
        if event_type == "prompt" {
            if let Some(n) = limit {
                if lines.len() >= n {
                    break;
                }
            }
            pending_reply_idx = None;
            let raw = title.filter(|s| !s.trim().is_empty()).or(body).unwrap_or_default();
            let trimmed = raw.trim();
            if trimmed.chars().count() < MEANINGFUL_PROMPT_MIN_CHARS {
                continue;
            }
            if is_excerpt_noise(trimmed) {
                continue;
            }
            let flattened = trimmed.replace('\n', " ");
            lines.push(PromptLine {
                ts,
                text: truncate_with_ellipsis(&flattened, PROMPT_LINE_MAX_CHARS),
                reply: None,
            });
            pending_reply_idx = Some(lines.len() - 1);
        } else if let Some(idx) = pending_reply_idx.take() {
            if let Some(reply) = refine_reply_text(title, body) {
                lines[idx].reply = Some(reply);
            }
        }
    }

    if lines.is_empty() {
        for (ts, event_type, title, body) in rows {
            if event_type != "response" {
                continue;
            }
            if let Some(text) = refine_reply_text(title, body) {
                lines.push(PromptLine { ts, text, reply: None });
                if lines.len() >= NO_PROMPT_FALLBACK_MAX_LINES {
                    break;
                }
            }
        }
    }

    Ok(lines)
}

/// GitHub 이벤트 title 1건(그룹핑 전 원자료) — repo(project 원문, 프로젝트 재그룹핑 전 키) +
/// 정렬용 ts + title. `repo`가 `None`이면(project 미상) 프로젝트별 재구성 단계에서 제외된다.
struct GithubTitleRow {
    repo: Option<String>,
    ts: i64,
    title: String,
}

fn fetch_github_titles(conn: &Connection, start_ms: i64, end_ms: i64) -> anyhow::Result<Vec<GithubTitleRow>> {
    let mut stmt = conn.prepare(
        "SELECT s.project AS repo, MIN(e.ts) AS ts, e.title AS title
         FROM events e
         JOIN streams s ON s.id = e.stream_id
         WHERE e.source = 'github' AND e.ts >= ?1 AND e.ts < ?2 AND e.title IS NOT NULL
         GROUP BY s.project, e.title
         ORDER BY s.project ASC, ts ASC",
    )?;
    let rows = stmt
        .query_map(params![start_ms, end_ms], |row| {
            Ok(GithubTitleRow {
                repo: row.get("repo")?,
                ts: row.get("ts")?,
                title: row.get("title")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `"PR #<N> ..."` title에서 PR 번호(문자열)를 뽑는다. PR 형식이 아니면 `None`.
/// 접미사 없이 숫자로 끝나는 title(`"PR #42"`)도 파싱한다(방어적 — 리뷰 지적).
fn pr_number(title: &str) -> Option<&str> {
    let rest = title.strip_prefix("PR #")?;
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 {
        None
    } else {
        Some(&rest[..end])
    }
}

/// 같은 PR #N에 대해 "PR #N …: 제목"(콜론 있는 제목형) 항목이 있으면, 무제목 "PR #N opened/merged/
/// closed"(콜론 없는 항목)는 제외한다(중복 노이즈 제거, github.rs `labeled_title` 형식 참고).
fn dedupe_pr_titles(items: Vec<(i64, String)>) -> Vec<(i64, String)> {
    let titled_numbers: std::collections::HashSet<String> = items
        .iter()
        .filter_map(|(_, title)| {
            let num = pr_number(title)?;
            if title.contains(": ") {
                Some(num.to_string())
            } else {
                None
            }
        })
        .collect();

    items
        .into_iter()
        .filter(|(_, title)| {
            !matches!(pr_number(title), Some(num) if !title.contains(": ") && titled_numbers.contains(num))
        })
        .collect()
}

/// repo별 `### {repo}` 헤더 + 최대 8개 title 불릿을 렌더링한다.
/// GitHub title 원자료를 repo(project)별로 그룹핑해 PR dedupe + 건수 상한을 적용한 뒤
/// `(project, (ts, title))` 통합 라인 목록으로 편다(프로젝트별 재구성 단계 입력).
/// project가 없는(`None`) 행은 여기서 제외한다(명세: "프로젝트 없는 스트림 제외").
fn github_titles_by_project(rows: Vec<GithubTitleRow>) -> Vec<(String, i64, String)> {
    let mut by_repo: BTreeMap<String, Vec<(i64, String)>> = BTreeMap::new();
    for row in rows {
        let Some(repo) = row.repo else { continue };
        by_repo.entry(repo).or_default().push((row.ts, row.title));
    }

    let mut out = Vec::new();
    for (repo, items) in by_repo {
        let deduped = dedupe_pr_titles(items);
        for (ts, title) in deduped.into_iter().take(MAX_GITHUB_ITEMS_PER_REPO) {
            out.push((repo.clone(), ts, truncate_with_ellipsis(&title, GITHUB_TITLE_MAX_CHARS)));
        }
    }
    out
}

/// Linear 이슈 스트림 1건 — `project`는 팀명(Linear는 팀명이 project 키, repo/세션 표시명과 자동
/// 통합되지 않고 그대로 분리된 프로젝트 그룹이 된다 — LLM이 흐름에서 티켓 키로 연결하도록 의도한
/// 설계, 명세 참고). `project`가 `None`이면 재구성 단계에서 제외. `metadata`는 스트림의 원본 JSON
/// 문자열(`capture/linear.rs::issue_stream_metadata`가 생성 — 에픽·상태를 실어 나른다,
/// [`parse_linear_metadata`] 참고).
struct LinearIssueRow {
    title: Option<String>,
    stream_id: String,
    project: Option<String>,
    metadata: Option<String>,
    /// 정렬/라인 표시용 최초 활동 ts(그룹 내 다른 소스 라인과의 시간순 통합 기준).
    ts: i64,
    activity: i64,
}

fn fetch_linear_issues(conn: &Connection, start_ms: i64, end_ms: i64) -> anyhow::Result<Vec<LinearIssueRow>> {
    let mut stmt = conn.prepare(
        "SELECT s.id AS stream_id, s.title AS title, s.project AS project, s.metadata AS metadata,
                MIN(e.ts) AS ts, COUNT(*) AS activity
         FROM events e
         JOIN streams s ON s.id = e.stream_id
         WHERE e.source = 'linear' AND e.ts >= ?1 AND e.ts < ?2
         GROUP BY s.id
         ORDER BY activity DESC
         LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![start_ms, end_ms, MAX_LINEAR_ITEMS as i64], |row| {
            Ok(LinearIssueRow {
                stream_id: row.get("stream_id")?,
                title: row.get("title")?,
                project: row.get("project")?,
                metadata: row.get("metadata")?,
                ts: row.get("ts")?,
                activity: row.get("activity")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 스트림 `metadata`(JSON 문자열, `capture/linear.rs::issue_stream_metadata` 생성)에서 에픽
/// (`linearProject`)과 상태(`state`)를 읽어온다 — `(에픽, 상태)`. JSON 파싱 실패·키 없음·값 없음은
/// 조용히 무시하고 해당 자리를 `None`으로 둔다(이 파일 전반의 "Option으로 방어적 skip" 관례와 동일).
fn parse_linear_metadata(metadata: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(raw) = metadata else { return (None, None) };
    let Ok(value) = serde_json::from_str::<Value>(raw) else { return (None, None) };
    let linear_project = value.get("linearProject").and_then(Value::as_str).map(str::to_string);
    let state = value.get("state").and_then(Value::as_str).map(str::to_string);
    (linear_project, state)
}

/// Linear 이슈 1건을 "{에픽 › }{title}{ [상태]} (HH:MM, 활동 N)" 라인 하나로 렌더링(프로젝트별
/// 통합 라인용). 세션 블록 헤더와 동일하게 **시각을 맨 앞 괄호 항목**으로 넣는다 — 라인들이 ts
/// 오름차순으로 섞여 나가도 LLM이 시간순을 눈으로 확인할 근거가 없으면 출력 순서를 임의로
/// 재배열하기 때문(v5 프롬프트가 "발췌 순서=시간순 유지"를 지시하므로 그 근거를 발췌 본문에
/// 노출한다).
///
/// **에픽을 맨 앞에 두는 이유**: 발췌의 `## {프로젝트}` 축은 Linear 팀명이고, 에픽(Linear
/// `project`)은 그 아래 층위의 업무 단위(예: "결제 화면")다. 에픽을 라인 맨 앞에 노출해 둬야
/// `prompts.rs`가 지시하는 "관련 작업을 `### {주제}`로 묶어라" 규칙이 이 라인을 근거로 같은 에픽의
/// 이슈들을 하나의 `### 주제`로 묶을 수 있다. 상태는 대괄호로 붙여 `> ` 정리 줄(완료/진행 중 판정)의
/// 근거가 되게 한다.
fn render_linear_line(row: &LinearIssueRow, zone: &Tz, locale: &str) -> String {
    let title = row
        .title
        .as_deref()
        .map(|t| truncate_with_ellipsis(t, LINEAR_TITLE_MAX_CHARS))
        .unwrap_or_else(|| row.stream_id.clone());
    let (linear_project, state) = parse_linear_metadata(row.metadata.as_deref());
    let title = match state {
        Some(state) => format!("{title} [{state}]"),
        None => title,
    };
    let title = match linear_project {
        Some(epic) => format!("{epic} › {title}"),
        None => title,
    };
    format!("{title} ({}, {})", format_local_time(row.ts, zone), activity_label(locale, row.activity))
}

/// 본문 라벨(프롬프트/활동 카운트)도 locale 분기한다 — 헤딩만 번역하고 라벨이 한국어로 남으면
/// en 로케일에서 혼합 언어 입력이 되어 "Write in English" 프롬프트 규칙과 충돌한다(리뷰 지적).
fn prompts_label(locale: &str, n: i64) -> String {
    if locale == "ko" {
        format!("프롬프트 {n}")
    } else {
        format!("{n} prompts")
    }
}

fn activity_label(locale: &str, n: i64) -> String {
    if locale == "ko" {
        format!("활동 {n}")
    } else {
        format!("{n} events")
    }
}

/// 프로젝트별 재구성 전 소스 라인 1건 — 세션 블록(헤더+prompt 라인들을 통째로 묶은 텍스트)/GitHub
/// title/Linear title을 이 공통 표현으로 변환한 뒤 프로젝트 키로 그룹핑하고 그룹 내 `ts` 오름차순
/// 으로 통합 정렬한다(명세: "ts 시간순으로 통합").
struct ProjectLine {
    /// `last_path_segment(project)` — 표시 이름이자 그룹핑 키.
    project: String,
    ts: i64,
    /// 정렬된 프로젝트 내 활동량(라인 수) 계산에 쓰이는 "라인 1개" 단위 — 세션은 블록 전체가 1건.
    text: String,
}

/// 세션 스트림 1건을 소스 라벨이 붙은 텍스트 블록으로 렌더링한다 — 헤더(`[세션] {title}
/// (HH:MM~HH:MM, N prompts)`) + 그 아래 prompt 불릿들. 블록 전체를 하나의 [`ProjectLine`]으로
/// 취급해(정렬 키는 스트림 시작 시각) 다른 소스 라인들과 시간순으로 섞인다.
fn session_stream_to_project_line(
    conn: &Connection,
    row: &SessionStreamRow,
    zone: &Tz,
    start_ms: i64,
    end_ms: i64,
    locale: &str,
) -> anyhow::Result<Option<ProjectLine>> {
    let Some(project) = row.project.as_deref() else { return Ok(None) };
    let project = last_path_segment(project).to_string();

    let title = row
        .title
        .as_deref()
        .map(|t| truncate_with_ellipsis(t, STREAM_TITLE_MAX_CHARS))
        .unwrap_or_else(|| row.stream_id.clone());
    let time_range =
        format!("{}~{}", format_local_time(row.first_ts, zone), format_local_time(row.last_ts, zone));
    let session_label = if locale == "ko" { "[세션]" } else { "[Session]" };

    let mut text = format!("{session_label} {title} ({time_range}, {})\n", prompts_label(locale, row.prompts));
    for line in fetch_prompt_lines(conn, &row.stream_id, start_ms, end_ms, None)? {
        text.push_str("- ");
        text.push_str(&format_local_time(line.ts, zone));
        text.push(' ');
        text.push_str(&line.text);
        text.push('\n');
        // 이 요청을 처리한 응답 요지(있으면)를 들여쓴 하위 줄로 붙인다 — "c로 진행"류 지시대명사만
        // 남는 요청도 나중에 └ 줄을 보면 무엇을 결정·처리했는지 알 수 있게 한다(v5 "요청 → 결과"
        // 문맥 보강, prompts.rs 규칙 7번이 이 줄을 근거로 쓰도록 지시한다).
        if let Some(reply) = &line.reply {
            text.push_str("  └ ");
            text.push_str(reply);
            text.push('\n');
        }
    }

    Ok(Some(ProjectLine { project, ts: row.first_ts, text: text.trim_end().to_string() }))
}

/// 프로젝트 그룹 1개(표시명 + 시간순 정렬된 라인들) — 활동량(라인 수) 내림차순 출력 순서 결정에
/// 라인 개수를 그대로 쓴다.
struct ProjectGroup {
    project: String,
    lines: Vec<ProjectLine>,
}

/// 세션/GitHub/Linear 3개 소스에서 뽑은 [`ProjectLine`]들을 프로젝트 표시명 기준으로 묶고, 그룹
/// 내부는 `ts` 오름차순으로 정렬한다. summarizer 자기 캡처 프로젝트는 이미 호출부에서 필터링됐다고
/// 가정하지 않고 여기서도 방어적으로 한 번 더 제외한다(정책 함수는 project 원문 기준이라 표시명
/// 변환 전에 걸러야 하므로 호출부에서 처리 — 이 함수는 이미 표시명으로 변환된 라인만 받는다).
fn group_lines_by_project(lines: Vec<ProjectLine>) -> Vec<ProjectGroup> {
    let mut by_project: BTreeMap<String, Vec<ProjectLine>> = BTreeMap::new();
    for line in lines {
        by_project.entry(line.project.clone()).or_default().push(line);
    }

    let mut groups: Vec<ProjectGroup> = by_project
        .into_iter()
        .map(|(project, mut lines)| {
            lines.sort_by_key(|l| l.ts);
            ProjectGroup { project, lines }
        })
        .collect();

    // 활동량(라인 수) 내림차순 — 동률이면 프로젝트 표시명 오름차순(BTreeMap 유래 순서, 안정 정렬).
    groups.sort_by_key(|g| std::cmp::Reverse(g.lines.len()));
    groups
}

fn render_project_group(group: &ProjectGroup) -> String {
    let mut out = format!("## {}\n\n", group.project);
    for line in &group.lines {
        out.push_str(&line.text);
        out.push_str("\n\n");
    }
    out
}

/// 그날 데이터가 0건일 때의 **안정 에러 코드** — 정상적인 빈 상태를 진짜 에러(네트워크/타임아웃)와
/// FE가 구분할 수 있게 한다(리뷰 지적). DailySummaryCard가 i18n 키로 매핑해 언어별로 표시.
/// `pub(crate)`: `summary::period`(M7-③)의 주간/월간 캐스케이드도 "그 날/그 주 스킵" 판정에
/// 동일한 코드 문자열을 재사용한다(하드코딩 중복 시 드리프트 위험).
pub(crate) const NO_DATA_ERROR_CODE: &str = "summary_no_data";

/// 발췌 상단 "활동 요약" 통계 줄을 조립한다(v7, "평면 나열이라 스캔이 안 된다"는 실사용 피드백
/// 대응) — LLM이 프로젝트·세션·요청·GitHub·Linear 활동 수를 직접 세면 실측상 오차가 나므로, 발췌
/// 생성 시점에 이미 갖고 있는 원자료(daily는 session_rows/github_rows/linear_rows/최종 groups,
/// 주간·월간은 `period::get_period_stats`가 쓰는 동일한 집계 함수들)에서 계산해 텍스트로 미리
/// 박아 넣고, 프롬프트는 이 줄을 그대로 옮겨 쓰도록 지시한다(`prompts.rs` 참고). `lead`는 맨 앞
/// 항목(일간은 "프로젝트 N개"/"N projects", 주간·월간은 "활동일 N/7"/"활동일 N일" 등)을 호출부가
/// 이미 locale에 맞게 포맷해 넘긴다 — 항목마다 조사·단위가 달라 공통 포맷터로 뽑아내기보다
/// 호출부에서 만드는 편이 더 명확하다. 값이 0인 항목(lead 제외)은 생략한다(Linear 미연결 시
/// "Linear 활동 0건"이 뜨는 지저분함 방지).
/// `pub(crate)`: `summary::period`(M7-③)의 주간/월간 발췌 합성도 동일한 형식을 재사용한다.
pub(crate) fn format_stats_line(
    locale: &str,
    lead: String,
    sessions: i64,
    requests: i64,
    github_events: i64,
    linear_events: i64,
) -> String {
    let mut parts: Vec<String> = vec![lead];
    if sessions > 0 {
        parts.push(if locale == "ko" {
            format!("세션 {sessions}개")
        } else {
            format!("{sessions} sessions")
        });
    }
    if requests > 0 {
        parts.push(if locale == "ko" {
            format!("요청 {requests}건")
        } else {
            format!("{requests} requests")
        });
    }
    if github_events > 0 {
        parts.push(if locale == "ko" {
            format!("GitHub 활동 {github_events}건")
        } else {
            format!("{github_events} GitHub events")
        });
    }
    if linear_events > 0 {
        parts.push(if locale == "ko" {
            format!("Linear 활동 {linear_events}건")
        } else {
            format!("{linear_events} Linear events")
        });
    }
    let joined = parts.join(" · ");
    if locale == "ko" {
        format!("활동 요약: {joined}")
    } else {
        format!("Activity: {joined}")
    }
}

fn daily_title_heading(locale: &str, local_date: &str) -> String {
    if locale == "ko" {
        format!("# 작업 기록 발췌 — {local_date}")
    } else {
        format!("# Work log excerpt — {local_date}")
    }
}

/// 하루치 발췌 텍스트를 만든다(`generate_daily_summary`/`preview_summary_input` 커맨드가 사용).
/// 시간창은 `query::get_digest`와 동일(`day_range_ms`, chrono-tz 로컬 자정 기준
/// `[00:00, 24:00)`). 코딩 에이전트 세션(그날 활동한 세션형 스트림 전량, prompt 라인도 스트림당
/// 상한 없이 전량) + GitHub(repo별 title, PR dedupe) + Linear(활동 수 상위 15) 3개 소스를
/// **프로젝트별로 재구성**한다 — 프로젝트 키는
/// `last_path_segment(project)`(세션 로컬 경로·GitHub owner/repo는 표시명으로 자동 통합, Linear는
/// 팀명이 그대로 키). 자기 캡처 프로젝트(`is_summarizer_project`)는 제외한다. 각 프로젝트 그룹
/// 안에서는 세션 블록·GitHub title·Linear title을 ts 오름차순으로 통합하고, 프로젝트 그룹 자체는
/// 활동량(라인 수) 내림차순으로 배치한다. 전체 프로젝트가 0개면 `Err`. Slack은 프로젝트 매핑이
/// 어려워 이번 범위에서 제외한다(후속 과제). 제목 바로 다음 줄에는 [`format_stats_line`]이 만든
/// "활동 요약" 통계 줄이 들어간다(v7 — LLM이 직접 세지 않도록 발췌가 미리 집계해 준다).
///
/// v10: hub 세션(`capture/hub.rs`)에서 레포 신호가 없어 base 스트림에 남은 턴은 시작 폴더(`~/.agent-hub`
/// 등) 이름 대신 `## 레포 미지정`(en `## Unassigned`) 섹션으로 모으고, 통계 줄 다음에
/// "알려진 프로젝트" 한 줄([`fetch_known_projects`])을 넣는다. hub 표시가 없어도 레포 밖 폴더에서 시작한
/// 세션([`is_outside_repo`])도 같은 섹션으로 보낸다. 레포 미지정 섹션은 알려진 프로젝트 줄 바로 다음
/// (프로젝트 섹션들 앞)에 둔다 — 바쁜 날 [`MAX_EXCERPT_CHARS`] 절단에 가장 먼저 잘리지 않게 — AI 가 그 기록을 내용으로 판단해 맞는
/// 프로젝트 섹션에 배치하게 하기 위함(`prompts.rs` v10). 통계의 프로젝트 수에서 레포 미지정은 뺀다. 마지막에
/// 전체 길이를 [`MAX_EXCERPT_CHARS`]로 안전컷한다(`truncate_with_ellipsis` 재사용 — 다른 절단부와
/// 동일한 코드포인트 경계 안전 로직을 중복 구현하지 않기 위함).
pub fn build_daily_excerpt(conn: &Connection, local_date: &str, tz: &str, locale: &str) -> anyhow::Result<String> {
    let exclude_projects = crate::capture::config::load_exclude_projects();
    build_daily_excerpt_with_excludes(conn, local_date, tz, locale, &exclude_projects)
}

/// [`build_daily_excerpt`] 본체 — 캡처 제외 프로젝트 목록을 인자로 받는다(테스트가 설정 파일 없이 주입).
fn build_daily_excerpt_with_excludes(
    conn: &Connection,
    local_date: &str,
    tz: &str,
    locale: &str,
    exclude_projects: &[String],
) -> anyhow::Result<String> {
    let (start_ms, end_ms) = day_range_ms(local_date, tz)?;
    let zone: Tz = tz.parse().map_err(|_| anyhow::anyhow!("invalid timezone: {tz}"))?;

    let session_rows = fetch_session_streams(conn, start_ms, end_ms)?;
    let github_rows = fetch_github_titles(conn, start_ms, end_ms)?;
    let linear_rows = fetch_linear_issues(conn, start_ms, end_ms)?;

    // 통계 줄용 원자료 개수 — 아래에서 github_rows/linear_rows가 소비(move)·순회되기 전에 먼저
    // 뽑아 둔다(추가 SQL 없이 이미 가져온 로우를 재사용, 명세).
    let session_count = session_rows.len();
    let request_count: i64 = session_rows.iter().map(|row| row.prompts).sum();
    let github_count = github_rows.len();
    let linear_count = linear_rows.len();

    let mut lines: Vec<ProjectLine> = Vec::new();
    // hub base·레포 밖 세션 블록 — 프로젝트 그룹과 섞지 않고 "레포 미지정" 섹션으로 따로 낸다.
    let mut unassigned: Vec<ProjectLine> = Vec::new();
    let mut outside_cache = OutsideRepoCache::new();

    for row in &session_rows {
        if row.project.as_deref().is_some_and(crate::capture::policy::is_summarizer_project) {
            continue;
        }
        if let Some(line) = session_stream_to_project_line(conn, row, &zone, start_ms, end_ms, locale)? {
            let outside_repo = !row.hub_child
                && row.project.as_deref().is_some_and(|p| is_outside_repo(p, &mut outside_cache));
            if row.hub_base || outside_repo {
                unassigned.push(line);
            } else {
                lines.push(line);
            }
        }
    }

    // GitHub/Linear 소스 라벨은 en/ko 표기가 동일하다(고유명사) — locale 분기 불필요.
    for (repo, ts, title) in github_titles_by_project(github_rows) {
        if crate::capture::policy::is_summarizer_project(&repo) {
            continue;
        }
        let project = last_path_segment(&repo).to_string();
        // 세션/Linear 라인과 동일하게 시각을 노출한다(v5: 출력이 시간순을 지키게 하는 근거).
        lines.push(ProjectLine {
            project,
            ts,
            text: format!("[GitHub] {title} ({})", format_local_time(ts, &zone)),
        });
    }

    for row in &linear_rows {
        let Some(project_raw) = row.project.as_deref() else { continue };
        if crate::capture::policy::is_summarizer_project(project_raw) {
            continue;
        }
        let project = last_path_segment(project_raw).to_string();
        let text = format!("[Linear] {}", render_linear_line(row, &zone, locale));
        lines.push(ProjectLine { project, ts: row.ts, text });
    }

    if lines.is_empty() && unassigned.is_empty() {
        anyhow::bail!("{NO_DATA_ERROR_CODE}");
    }

    // "프로젝트 N개"는 실제 프로젝트 그룹만 센다(레포 미지정 섹션 제외).
    let groups = group_lines_by_project(lines);
    let known_projects = fetch_known_projects(conn, end_ms, exclude_projects, &mut outside_cache)?;

    let lead = if locale == "ko" {
        format!("프로젝트 {}개", groups.len())
    } else {
        format!("{} projects", groups.len())
    };
    let stats_line =
        format_stats_line(locale, lead, session_count as i64, request_count, github_count as i64, linear_count as i64);

    let mut out = daily_title_heading(locale, local_date);
    out.push('\n');
    out.push_str(&stats_line);
    out.push('\n');
    if !known_projects.is_empty() {
        out.push_str(&known_projects_line(locale, &known_projects));
        out.push('\n');
    }
    out.push('\n');
    // 레포 미지정은 프로젝트 섹션들 앞에 둔다 — 맨 끝에 두면 바쁜 날 MAX_EXCERPT_CHARS 절단에 가장 먼저 잘린다.
    if !unassigned.is_empty() {
        unassigned.sort_by_key(|l| l.ts);
        out.push_str(&render_project_group(&ProjectGroup {
            project: unassigned_heading(locale).to_string(),
            lines: unassigned,
        }));
    }
    for group in &groups {
        out.push_str(&render_project_group(group));
    }

    // 발송 직전 2차 시크릿 스크럽(보안 리뷰 High): scrub_secrets를 껐던 시기에 캡처된 원문이
    // DB에 남아 있을 수 있고, 이 발췌는 로컬 경계를 넘는 첫 경로(외부 API/CLI로 전송)이므로
    // 저장 시점 스크럽과 별개로 여기서 한 번 더 방어한다.
    let scrubbed = scrub::scrub_text(out.trim_end());
    Ok(truncate_with_ellipsis(&scrubbed, MAX_EXCERPT_CHARS))
}

/// 사용자가 미리보기 에디터에서 직접 수정한 발췌(override)를 발송 가능하게 정제한다 —
/// 자동 발췌와 동일하게 발송 직전 스크럽 + 길이 안전컷을 적용한다(수정 중 실수로 붙여넣은 시크릿
/// 방어). 공백뿐이면 [`NO_DATA_ERROR_CODE`] — 빈 내용으로 엔진을 호출하지 않는다.
pub fn sanitize_excerpt_override(text: &str) -> anyhow::Result<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{NO_DATA_ERROR_CODE}");
    }
    let scrubbed = scrub::scrub_text(trimmed);
    Ok(truncate_with_ellipsis(&scrubbed, MAX_EXCERPT_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 순수 헬퍼 ─────────────────────────────────────────────────

    #[test]
    fn is_excerpt_noise_detects_each_pattern() {
        assert!(is_excerpt_noise("Another Claude session sent a message"));
        assert!(is_excerpt_noise("This session is being continued from a previous one"));
        assert!(is_excerpt_noise("<teammate-message from=\"agent-b\">안녕</teammate-message>"));
        assert!(is_excerpt_noise("[Request interrupted by user]"));
    }

    #[test]
    fn is_excerpt_noise_false_for_normal_prompt() {
        assert!(!is_excerpt_noise("이 프로젝트 구조를 좀 설명해 주실 수 있을까요"));
    }

    #[test]
    fn last_path_segment_extracts_final_segment() {
        assert_eq!(last_path_segment("/Users/x/git/logroom"), "logroom");
        assert_eq!(last_path_segment("owner/repo"), "repo");
        assert_eq!(last_path_segment("no-slash"), "no-slash");
    }

    #[test]
    fn pr_number_extracts_digits_after_prefix() {
        assert_eq!(pr_number("PR #42 opened: Add feature"), Some("42"));
        assert_eq!(pr_number("PR #42 opened"), Some("42"));
        assert_eq!(pr_number("Issue #7 opened: Bug"), None);
        assert_eq!(pr_number("PR #abc opened"), None);
    }

    #[test]
    fn dedupe_pr_titles_removes_untitled_when_titled_variant_exists() {
        let items = vec![
            (1_000, "PR #42 opened".to_string()),
            (2_000, "PR #42 merged: Add feature".to_string()),
            (3_000, "PR #7 opened".to_string()),
        ];
        let result = dedupe_pr_titles(items);
        let titles: Vec<&str> = result.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(titles, vec!["PR #42 merged: Add feature", "PR #7 opened"]);
    }

    #[test]
    fn dedupe_pr_titles_keeps_untitled_when_no_titled_variant() {
        let items = vec![(1_000, "PR #9 opened".to_string())];
        let result = dedupe_pr_titles(items.clone());
        assert_eq!(result, items);
    }

    #[test]
    fn dedupe_pr_titles_keeps_non_pr_titles_untouched() {
        let items = vec![
            (1_000, "Issue #1 opened: Bug".to_string()),
            (2_000, "코멘트/리뷰: Add feature".to_string()),
        ];
        let result = dedupe_pr_titles(items.clone());
        assert_eq!(result, items);
    }

    // ── build_daily_excerpt(통합, seeded_conn) ────────────────────

    use crate::db;
    use rusqlite::params as rparams;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!(
                "logroom-excerpt-test-{}-{n}-{nanos}",
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

    fn seeded_conn() -> (TempDir, Connection) {
        let tmp = TempDir::new();
        let conn = db::open_and_migrate_at(&tmp.db_path()).expect("마이그레이션 성공");
        (tmp, conn)
    }

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
            rparams![id, source, kind, title, project, started_at, ended_at, started_at],
        )
        .unwrap();
    }

    /// `insert_stream`과 동일하지만 `metadata`를 직접 지정한다(Linear 에픽/상태 렌더링 테스트 전용
    /// — `capture/linear.rs::issue_stream_metadata`가 만드는 JSON 모양을 그대로 흉내낸다).
    fn insert_linear_stream(conn: &Connection, id: &str, project: &str, title: &str, metadata: &str, ts: i64) {
        conn.execute(
            "INSERT INTO streams
               (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES (?1, 'linear', 'session', ?2, ?3, NULL, ?4, ?4, 'active', ?5, ?4)",
            rparams![id, title, project, ts, metadata],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_event(
        conn: &Connection,
        id: &str,
        stream_id: &str,
        ts: i64,
        source: &str,
        event_type: &str,
        title: Option<&str>,
        body: Option<&str>,
        external_id: &str,
    ) {
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, NULL, NULL, NULL, ?8, '{}', ?3)",
            rparams![id, stream_id, ts, source, event_type, title, body, external_id],
        )
        .unwrap();
    }

    #[test]
    fn build_daily_excerpt_returns_err_when_no_data() {
        let (_tmp, conn) = seeded_conn();
        let err = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[test]
    fn sanitize_excerpt_override_scrubs_and_rejects_blank() {
        // 사용자 수정 발췌도 자동 발췌와 동일한 발송 직전 방어(스크럽·빈 값 거부)를 받는다.
        let sanitized =
            sanitize_excerpt_override("배포 키 sk-ant-abcdefghijklmnopqrstuvwxyz1234 포함 메모").unwrap();
        assert!(!sanitized.contains("sk-ant-abcdefghijklmnopqrstuvwxyz1234"));
        assert!(sanitized.contains("[REDACTED:anthropic]"));
        assert_eq!(
            sanitize_excerpt_override("   \n  ").unwrap_err().to_string(),
            NO_DATA_ERROR_CODE
        );
    }

    #[test]
    fn build_daily_excerpt_scrubs_secrets_before_send() {
        // 보안 리뷰 High: scrub_secrets를 껐던 시기에 저장된 원문 시크릿이 발췌(외부 전송 경로)로
        // 새지 않도록, 발송 직전 2차 스크럽이 동작해야 한다.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-secret",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("세션 제목"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-secret",
            "claude_code:sess-secret",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("이 키로 배포해줘 sk-ant-abcdefghijklmnopqrstuvwxyz1234"),
            None,
            "ext-secret",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(!excerpt.contains("sk-ant-abcdefghijklmnopqrstuvwxyz1234"), "원문 시크릿이 남으면 안 됨");
        assert!(excerpt.contains("[REDACTED:anthropic]"));
    }

    #[test]
    fn build_daily_excerpt_no_data_code_is_locale_independent() {
        // 에러 코드는 locale과 무관하게 안정적이어야 FE 매핑이 깨지지 않는다(리뷰 반영).
        let (_tmp, conn) = seeded_conn();
        let err = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "en").unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[test]
    fn build_daily_excerpt_includes_session_stream_with_meaningful_prompts_only() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-1",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("세션 제목"),
            start_ms + 1_000,
            start_ms + 5_000,
        );
        // 짧은(15자 미만) 프롬프트는 제외돼야 함.
        insert_event(
            &conn,
            "ev-short",
            "claude_code:sess-1",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("ㄱㄱ"),
            None,
            "ext-short",
        );
        insert_event(
            &conn,
            "ev-meaningful",
            "claude_code:sess-1",
            start_ms + 2_000,
            "claude_code",
            "prompt",
            Some("이 프로젝트 구조를 좀 설명해 주실 수 있을까요"),
            None,
            "ext-meaningful",
        );
        // 노이즈 프리픽스는 제외돼야 함.
        insert_event(
            &conn,
            "ev-noise",
            "claude_code:sess-1",
            start_ms + 3_000,
            "claude_code",
            "prompt",
            Some("Another Claude session sent a long enough notification text here"),
            None,
            "ext-noise",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("## logroom"), "프로젝트별 섹션 헤더가 있어야 함");
        assert!(excerpt.contains("[세션] 세션 제목"));
        assert!(excerpt.contains("프롬프트 3"), "prompt 이벤트 수는 필터 전 전체(3건) 기준");
        assert!(excerpt.contains("이 프로젝트 구조를 좀 설명해 주실 수 있을까요"));
        assert!(!excerpt.contains("ㄱㄱ"));
        assert!(!excerpt.contains("Another Claude session sent"));
    }

    #[test]
    fn build_daily_excerpt_includes_stats_line_with_all_categories() {
        // v7: 발췌 제목 바로 다음 줄에 "활동 요약" 통계가 미리 집계돼 있어야 한다(LLM이 직접
        // 세지 않도록) — 세션·GitHub·Linear가 모두 있으면 전 항목이 나와야 한다.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-stats",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("세션 제목"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-stats",
            "claude_code:sess-stats",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("통계 줄 검증용 프롬프트 내용입니다"),
            None,
            "ext-stats",
        );
        insert_stream(
            &conn,
            "github:owner/logroom",
            "github",
            "session",
            Some("owner/logroom"),
            Some("owner/logroom"),
            start_ms + 3_000,
            start_ms + 3_000,
        );
        insert_event(
            &conn,
            "ev-gh-stats",
            "github:owner/logroom",
            start_ms + 3_000,
            "github",
            "message",
            Some("PR #1 opened: 새 기능"),
            None,
            "ext-gh-stats",
        );
        insert_stream(
            &conn,
            "linear:TICKET-9",
            "linear",
            "session",
            Some("Maintenance"),
            Some("TICKET-9 버그 수정"),
            start_ms + 500,
            start_ms + 500,
        );
        insert_event(
            &conn,
            "ev-l-stats",
            "linear:TICKET-9",
            start_ms + 500,
            "linear",
            "message",
            None,
            None,
            "ext-l-stats",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        // github(owner/logroom)과 session(/Users/x/git/logroom)은 표시명이 같아 "logroom" 프로젝트
        // 하나로 통합되고, linear(Maintenance)가 별도 프로젝트라 총 2개.
        assert!(
            excerpt.contains(
                "활동 요약: 프로젝트 2개 · 세션 1개 · 요청 1건 · GitHub 활동 1건 · Linear 활동 1건"
            ),
            "통계 줄이 형식대로 들어가야 함: {excerpt}"
        );
    }

    #[test]
    fn build_daily_excerpt_stats_line_omits_zero_categories() {
        // 세션만 있고 GitHub/Linear가 없으면 해당 항목은 생략돼야 한다(0건 표기 지저분함 방지).
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-only",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("세션 제목"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-only",
            "claude_code:sess-only",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("세션만 있는 날의 프롬프트 내용입니다"),
            None,
            "ext-only",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("활동 요약: 프로젝트 1개 · 세션 1개 · 요청 1건"));
        assert!(!excerpt.contains("GitHub 활동"), "GitHub 이벤트가 없으면 생략돼야 함");
        assert!(!excerpt.contains("Linear 활동"), "Linear 이벤트가 없으면 생략돼야 함");
    }

    #[test]
    fn build_daily_excerpt_session_prompt_lines_include_all_beyond_old_cap() {
        // 예전에는 세션당 앞에서 5개(`ORDER BY ts ASC` + 하드코딩 상한)만 채택하고 나머지를
        // 버렸다(실사용 데이터 손실 회귀 방지) — 8개를 넣어도 전량이 발췌에 남아야 한다.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-many",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("긴 세션"),
            start_ms + 1_000,
            start_ms + 1_000 + 8 * 60_000,
        );
        for i in 0i64..8 {
            insert_event(
                &conn,
                &format!("ev-many-{i}"),
                "claude_code:sess-many",
                start_ms + 1_000 + i * 60_000,
                "claude_code",
                "prompt",
                Some(&format!("여덟 개 중 {i}번째로 채택되어야 하는 프롬프트 내용")),
                None,
                &format!("ext-many-{i}"),
            );
        }

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        for i in 0i64..8 {
            assert!(
                excerpt.contains(&format!("여덟 개 중 {i}번째로 채택되어야 하는 프롬프트 내용")),
                "{i}번째 프롬프트가 발췌에서 누락됨(옛 5개 상한 회귀)"
            );
        }
    }

    #[test]
    fn build_daily_excerpt_session_prompt_lines_have_time_prefix() {
        // v5 보강: prompt 라인 맨 앞에 시각(HH:MM)이 붙어야 LLM이 순서를 재배열하지 않을 근거가
        // 생긴다(예전엔 fetch_prompt_lines가 ts를 조회조차 하지 않아 시각 표기가 불가능했다).
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-time",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("시각 확인 세션"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        let ts = start_ms + (10 * 3600 + 30 * 60) * 1_000; // 10:30 KST
        insert_event(
            &conn,
            "ev-time",
            "claude_code:sess-time",
            ts,
            "claude_code",
            "prompt",
            Some("시각 접두가 반드시 붙어야 하는 프롬프트 내용"),
            None,
            "ext-time",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(
            excerpt.contains("- 10:30 시각 접두가 반드시 붙어야 하는 프롬프트 내용"),
            "prompt 라인 맨 앞에 HH:MM 시각이 붙어야 함"
        );
    }

    #[test]
    fn build_daily_excerpt_appends_reply_as_result_subline() {
        // "요청 → 결과" 문맥 보강 핵심 케이스: prompt 직후 response가 있으면 그 요지가 "  └ "
        // 하위 줄로 요청 라인 바로 아래 붙어야 한다.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-reply",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("세션 제목"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-prompt",
            "claude_code:sess-reply",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("이 프로젝트 구조를 좀 설명해 주실 수 있을까요"),
            None,
            "ext-prompt",
        );
        insert_event(
            &conn,
            "ev-response",
            "claude_code:sess-reply",
            start_ms + 2_000,
            "claude_code",
            "response",
            Some("구조 설명 완료, README에 정리해 두었습니다"),
            None,
            "ext-response",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(
            excerpt.contains(
                "이 프로젝트 구조를 좀 설명해 주실 수 있을까요\n  └ 구조 설명 완료, README에 정리해 두었습니다"
            ),
            "prompt 라인 바로 아래에 응답 요지가 \"  └ \" 하위 줄로 붙어야 함"
        );
    }

    #[test]
    fn build_daily_excerpt_response_matches_only_first_and_resets_on_next_prompt() {
        // 다른 prompt가 끼어들면 매칭 구간이 그 prompt로 넘어간다: prompt1 뒤에 response 없이
        // prompt2가 먼저 오면 prompt1의 reply는 None으로 확정되고, prompt2 뒤에 response가 둘 있으면
        // 첫 번째만 매칭되고 두 번째는 버려져야 한다.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-first-match",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("세션 제목"),
            start_ms + 1_000,
            start_ms + 4_000,
        );
        insert_event(
            &conn,
            "ev-prompt1",
            "claude_code:sess-first-match",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("긴 프롬프트 내용 첫 번째 매칭 대상입니다"),
            None,
            "ext-prompt1",
        );
        insert_event(
            &conn,
            "ev-prompt2",
            "claude_code:sess-first-match",
            start_ms + 2_000,
            "claude_code",
            "prompt",
            Some("두 번째 프롬프트 내용도 충분히 깁니다"),
            None,
            "ext-prompt2",
        );
        insert_event(
            &conn,
            "ev-response1",
            "claude_code:sess-first-match",
            start_ms + 3_000,
            "claude_code",
            "response",
            Some("두 번째 요청 처리 결과 요약입니다"),
            None,
            "ext-response1",
        );
        insert_event(
            &conn,
            "ev-response2",
            "claude_code:sess-first-match",
            start_ms + 4_000,
            "claude_code",
            "response",
            Some("두 번째 다음 응답이라 매칭되면 안 됩니다"),
            None,
            "ext-response2",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(
            !excerpt.contains("긴 프롬프트 내용 첫 번째 매칭 대상입니다\n  └"),
            "prompt1은 response 전에 prompt2가 끼었으므로 reply가 없어야 함"
        );
        assert!(
            excerpt.contains("두 번째 프롬프트 내용도 충분히 깁니다\n  └ 두 번째 요청 처리 결과 요약입니다"),
            "prompt2는 직후 첫 번째 response로 매칭돼야 함"
        );
        assert!(
            !excerpt.contains("두 번째 다음 응답이라 매칭되면 안 됩니다"),
            "구간 내 두 번째 response는 버려져야 함(첫 번째만 매칭)"
        );
    }

    #[test]
    fn build_daily_excerpt_fills_lines_from_responses_when_no_prompt_adopted() {
        // prompt 0건 + response만 있는 세션도(예전엔 "기록된 요청 없음"으로만 나오던 케이스)
        // response 요지로 발췌 라인을 채워야 한다.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-no-prompt",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("응답만 있는 세션"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-resp1",
            "claude_code:sess-no-prompt",
            start_ms + 1_000,
            "claude_code",
            "response",
            Some("응답 전용 세션의 첫 번째 처리 결과입니다"),
            None,
            "ext-resp1",
        );
        insert_event(
            &conn,
            "ev-resp2",
            "claude_code:sess-no-prompt",
            start_ms + 2_000,
            "claude_code",
            "response",
            Some("응답 전용 세션의 두 번째 처리 결과입니다"),
            None,
            "ext-resp2",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("## logroom"), "prompt가 없어도 프로젝트 섹션은 있어야 함");
        assert!(excerpt.contains("응답 전용 세션의 첫 번째 처리 결과입니다"));
        assert!(excerpt.contains("응답 전용 세션의 두 번째 처리 결과입니다"));
    }

    #[test]
    fn build_daily_excerpt_github_dedupes_untitled_pr_and_caps_per_repo() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "github:octocat/Hello-World",
            "github",
            "session",
            Some("octocat/Hello-World"),
            Some("octocat/Hello-World"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-pr-untitled",
            "github:octocat/Hello-World",
            start_ms + 1_000,
            "github",
            "message",
            Some("PR #42 opened"),
            None,
            "ext-pr-untitled",
        );
        insert_event(
            &conn,
            "ev-pr-titled",
            "github:octocat/Hello-World",
            start_ms + 2_000,
            "github",
            "message",
            Some("PR #42 merged: Add feature"),
            None,
            "ext-pr-titled",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("## Hello-World"), "GitHub owner/repo도 마지막 세그먼트가 프로젝트명");
        // v5: GitHub 라인 끝에 시각(HH:MM)이 붙는다 — 세션 블록 헤더·Linear 라인과 동일하게
        // 시간순 근거를 발췌 본문에 노출해야 프롬프트의 "시간순 유지" 지시가 성립한다.
        assert!(excerpt.contains("[GitHub] PR #42 merged: Add feature (00:00)"));
        assert!(!excerpt.contains("PR #42 opened"), "무제목 PR 중복은 제외돼야 함");
    }

    #[test]
    fn build_daily_excerpt_linear_orders_by_activity_desc() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "linear:TICKET-1",
            "linear",
            "session",
            Some("Maintenance"),
            Some("TICKET-1 버그 수정"),
            start_ms + 500,
            start_ms + 500,
        );
        insert_stream(
            &conn,
            "linear:TICKET-2",
            "linear",
            "session",
            Some("Maintenance"),
            Some("TICKET-2 기능 추가"),
            start_ms + 1_000,
            start_ms + 3_000,
        );
        insert_event(&conn, "ev-l1", "linear:TICKET-1", start_ms + 500, "linear", "message", None, None, "ext-l1");
        insert_event(&conn, "ev-l2a", "linear:TICKET-2", start_ms + 1_000, "linear", "message", None, None, "ext-l2a");
        insert_event(&conn, "ev-l2b", "linear:TICKET-2", start_ms + 2_000, "linear", "message", None, None, "ext-l2b");
        insert_event(&conn, "ev-l2c", "linear:TICKET-2", start_ms + 3_000, "linear", "message", None, None, "ext-l2c");

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("## Maintenance"), "Linear는 팀명이 그대로 프로젝트 키");
        // 같은 프로젝트(Maintenance) 그룹 안에서는 ts 오름차순 통합이라 먼저 활동한 TICKET-1이 앞에
        // 온다 — "활동량 내림차순"은 프로젝트 그룹 자체의 배치 순서에만 적용되고, 그룹 내부는
        // 시간순이라는 점을 함께 검증한다.
        let maint1_pos = excerpt.find("TICKET-1 버그 수정").unwrap();
        let maint2_pos = excerpt.find("TICKET-2 기능 추가").unwrap();
        assert!(maint1_pos < maint2_pos, "그룹 내부는 ts 오름차순이어야 함");
        // v5: Linear 라인도 세션 헤더와 같이 시각(HH:MM)을 먼저 노출한다 — 프롬프트가 "발췌
        // 순서=시간순 유지"를 지시하므로 그 근거가 본문에 있어야 한다.
        assert!(excerpt.contains(", 활동 3)"));
        assert!(excerpt.contains(", 활동 1)"));
    }

    #[test]
    fn parse_linear_metadata_reads_epic_and_state() {
        assert_eq!(
            parse_linear_metadata(Some(r#"{"linearProject":"결제 화면","state":"QA","stateType":"started"}"#)),
            (Some("결제 화면".to_string()), Some("QA".to_string()))
        );
    }

    #[test]
    fn parse_linear_metadata_silently_ignores_missing_or_invalid() {
        assert_eq!(parse_linear_metadata(None), (None, None));
        assert_eq!(parse_linear_metadata(Some("{}")), (None, None));
        assert_eq!(parse_linear_metadata(Some("not-json{")), (None, None));
    }

    #[test]
    fn build_daily_excerpt_linear_line_shows_epic_and_state() {
        // 명세: "{에픽} › {이슈} [{상태}]" — 에픽이 맨 앞, 상태는 대괄호.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_linear_stream(
            &conn,
            "linear:TICKET-2336",
            "신규기능",
            "TICKET-2336 결제 내역 목록 컬럼 추가",
            r#"{"linearProject":"결제 화면","state":"QA","stateType":"started"}"#,
            start_ms + 500,
        );
        insert_event(&conn, "ev-l1", "linear:TICKET-2336", start_ms + 500, "linear", "message", None, None, "ext-l1");

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("[Linear] 결제 화면 › TICKET-2336 결제 내역 목록 컬럼 추가 [QA] ("));
    }

    #[test]
    fn build_daily_excerpt_linear_line_omits_epic_and_state_when_absent() {
        // metadata가 '{}'(에픽·상태 없음)면 기존과 동일하게 title만 노출돼야 한다(하위 호환).
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_linear_stream(
            &conn,
            "linear:TICKET-9",
            "Maintenance",
            "TICKET-9 버그 수정",
            "{}",
            start_ms + 500,
        );
        insert_event(&conn, "ev-l1", "linear:TICKET-9", start_ms + 500, "linear", "message", None, None, "ext-l1");

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("[Linear] TICKET-9 버그 수정 ("));
        assert!(!excerpt.contains("›"), "에픽이 없으면 \"›\" 구분자가 없어야 함");
        assert!(!excerpt.contains("수정 ["), "상태가 없으면 title 뒤에 상태 대괄호가 붙지 않아야 함");
    }

    #[test]
    fn build_daily_excerpt_groups_sessions_and_github_under_same_display_name() {
        // 세션(로컬 절대경로)과 GitHub(owner/repo)가 마지막 세그먼트("logroom")로 같으면 같은
        // 프로젝트 그룹으로 통합돼야 한다(명세 핵심 요구).
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-1",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("세션 작업"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-1",
            "claude_code:sess-1",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("의미 있는 프롬프트 내용입니다 충분히"),
            None,
            "ext-1",
        );
        insert_stream(
            &conn,
            "github:owner/logroom",
            "github",
            "session",
            Some("owner/logroom"),
            Some("owner/logroom"),
            start_ms + 3_000,
            start_ms + 3_000,
        );
        insert_event(
            &conn,
            "ev-gh",
            "github:owner/logroom",
            start_ms + 3_000,
            "github",
            "message",
            Some("PR #1 opened: 새 기능"),
            None,
            "ext-gh",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        let section_count = excerpt.matches("## logroom").count();
        assert_eq!(section_count, 1, "같은 표시명은 프로젝트 섹션 하나로 통합돼야 함");
        let section_pos = excerpt.find("## logroom").unwrap();
        let section = &excerpt[section_pos..];
        let session_pos = section.find("[세션]").unwrap();
        let github_pos = section.find("[GitHub]").unwrap();
        assert!(session_pos < github_pos, "그룹 내부는 ts 오름차순(세션이 먼저)이어야 함");
    }

    #[test]
    fn build_daily_excerpt_orders_projects_by_activity_desc() {
        // 프로젝트 그룹 자체의 배치 순서는 활동량(라인 수) 내림차순.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:small",
            "claude_code",
            "session",
            Some("/x/small-project"),
            Some("작은 프로젝트 세션"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-small",
            "claude_code:small",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("작은 프로젝트에서 한 번 작업했습니다"),
            None,
            "ext-small",
        );
        insert_stream(
            &conn,
            "claude_code:big",
            "claude_code",
            "session",
            Some("/x/big-project"),
            Some("큰 프로젝트 세션"),
            start_ms + 3_000,
            start_ms + 9_000,
        );
        for (i, ts_offset) in [3_000, 4_000, 5_000].into_iter().enumerate() {
            insert_event(
                &conn,
                &format!("ev-big-{i}"),
                "claude_code:big",
                start_ms + ts_offset,
                "claude_code",
                "prompt",
                Some(&format!("큰 프로젝트에서 여러 번 작업한 프롬프트 {i}")),
                None,
                &format!("ext-big-{i}"),
            );
        }

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        let big_pos = excerpt.find("## big-project").unwrap();
        let small_pos = excerpt.find("## small-project").unwrap();
        assert!(big_pos < small_pos, "라인 수가 더 많은 프로젝트가 먼저 나와야 함");
    }

    #[test]
    fn build_daily_excerpt_excludes_summarizer_project() {
        // 자기 캡처 루프 방지(ADR-0016, capture/policy.rs::is_summarizer_project) — 다른 유효
        // 프로젝트가 있으면 summarizer 프로젝트만 제외되고 나머지는 정상 포함돼야 한다.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:summarizer-sess",
            "claude_code",
            "session",
            Some("/Users/x/.logroom/summarizer"),
            Some("요약 CLI 호출 세션"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-summarizer",
            "claude_code:summarizer-sess",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("이 내용은 자기 캡처라 제외되어야 합니다"),
            None,
            "ext-summarizer",
        );
        insert_stream(
            &conn,
            "claude_code:normal-sess",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("정상 세션"),
            start_ms + 3_000,
            start_ms + 4_000,
        );
        insert_event(
            &conn,
            "ev-normal",
            "claude_code:normal-sess",
            start_ms + 3_000,
            "claude_code",
            "prompt",
            Some("이 내용은 정상 프로젝트라 포함되어야 합니다"),
            None,
            "ext-normal",
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(!excerpt.contains("이 내용은 자기 캡처라 제외되어야 합니다"));
        assert!(excerpt.contains("이 내용은 정상 프로젝트라 포함되어야 합니다"));
    }

    #[test]
    fn build_daily_excerpt_excludes_streams_without_project() {
        // 명세: "프로젝트 없는(None) 스트림 제외" — project가 없으면 데이터가 있어도 no_data.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:no-project",
            "claude_code",
            "session",
            None,
            Some("제목"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-1",
            "claude_code:no-project",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("프로젝트가 없어서 제외되어야 하는 프롬프트입니다"),
            None,
            "ext-1",
        );

        let err = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[test]
    fn build_daily_excerpt_scopes_to_day_range_only() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-out",
            "claude_code",
            "session",
            Some("/Users/x/logroom"),
            Some("전날 세션"),
            start_ms - 10_000,
            start_ms - 5_000,
        );
        insert_event(
            &conn,
            "ev-out",
            "claude_code:sess-out",
            start_ms - 5_000,
            "claude_code",
            "prompt",
            Some("이 이벤트는 하루 범위 밖이라 제외돼야 합니다"),
            None,
            "ext-out",
        );

        let err = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
        assert!(end_ms > start_ms);
    }

    // ── hub 세션(레포 미지정) · 알려진 프로젝트(v10) ─────────────────

    /// `insert_stream` 과 같되 metadata JSON 을 직접 지정한다(hub base·자식 스트림 흉내).
    #[allow(clippy::too_many_arguments)]
    fn insert_stream_with_meta(
        conn: &Connection,
        id: &str,
        kind: &str,
        project: &str,
        title: &str,
        started_at: i64,
        ended_at: i64,
        metadata: &str,
    ) {
        conn.execute(
            "INSERT INTO streams
               (id, source, kind, title, project, git_branch, started_at, ended_at, status, metadata, created_at)
             VALUES (?1, 'claude_code', ?2, ?3, ?4, NULL, ?5, ?6, 'active', ?7, ?5)",
            rparams![id, kind, title, project, started_at, ended_at, metadata],
        )
        .unwrap();
    }

    fn insert_prompt(conn: &Connection, id: &str, stream_id: &str, ts: i64, text: &str) {
        insert_event(conn, id, stream_id, ts, "claude_code", "prompt", Some(text), None, &format!("ext-{id}"));
    }

    /// hub base(`~/.agent-hub`) + 자식 스트림(`@logroom`) + 일반 레포 세션(other-repo) 하루치.
    fn seed_hub_day(conn: &Connection, start_ms: i64) {
        insert_stream_with_meta(
            conn,
            "claude_code:hub",
            "session",
            "/Users/x/.agent-hub",
            "허브 세션",
            start_ms + 1_000,
            start_ms + 9_000,
            r#"{"hub":true,"entrypoint":"sdk-cli"}"#,
        );
        insert_prompt(conn, "hub-1", "claude_code:hub", start_ms + 1_000, "운영 DB 에서 오늘 가입자 수 조회해줘");
        insert_stream_with_meta(
            conn,
            "claude_code:hub@/Users/x/git/logroom",
            "session",
            "/Users/x/git/logroom",
            "허브 세션",
            start_ms + 2_000,
            start_ms + 3_000,
            r#"{"hub":true,"hubStream":"claude_code:hub"}"#,
        );
        insert_prompt(
            conn,
            "child-1",
            "claude_code:hub@/Users/x/git/logroom",
            start_ms + 2_000,
            "logroom 요약 발췌 테스트 고쳐줘 부탁해요",
        );
        insert_stream_with_meta(
            conn,
            "claude_code:repo",
            "session",
            "/Users/x/git/other-repo",
            "다른 레포 세션",
            start_ms + 4_000,
            start_ms + 5_000,
            "{}",
        );
        insert_prompt(conn, "repo-1", "claude_code:repo", start_ms + 4_000, "다른 레포의 빌드 스크립트 정리해줘요");
    }

    #[test]
    fn hub_base_lines_go_to_unassigned_section_before_projects() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        seed_hub_day(&conn, start_ms);

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(!excerpt.contains("## .agent-hub"), "hub 시작 폴더 이름이 섹션이 되면 안 됨: {excerpt}");

        let known_at = excerpt.find("알려진 프로젝트: ").expect("알려진 프로젝트 줄");
        let unassigned_at = excerpt.find("## 레포 미지정").expect("레포 미지정 섹션");
        let logroom_at = excerpt.find("## logroom").expect("자식 스트림은 레포 섹션으로");
        let other_at = excerpt.find("## other-repo").expect("일반 레포 섹션");
        // 바쁜 날 절단(MAX_EXCERPT_CHARS)에 먼저 잘리지 않게 알려진 프로젝트 줄 바로 다음, 프로젝트 섹션들 앞.
        assert!(
            known_at < unassigned_at && unassigned_at < logroom_at && unassigned_at < other_at,
            "레포 미지정은 프로젝트 섹션들 앞: {excerpt}"
        );
        let between = &excerpt[known_at..unassigned_at];
        assert_eq!(between.matches('\n').count(), 2, "알려진 프로젝트 줄 + 빈 줄 바로 다음: {excerpt}");

        let first_project_at = logroom_at.min(other_at);
        let unassigned_body = &excerpt[unassigned_at..first_project_at];
        assert!(unassigned_body.contains("운영 DB 에서 오늘 가입자 수 조회해줘"));
        assert!(!unassigned_body.contains("logroom 요약 발췌 테스트"), "자식 스트림 라인은 레포 섹션에만");
        assert!(excerpt[logroom_at..].contains("logroom 요약 발췌 테스트 고쳐줘"));
    }

    /// `source` 를 지정해 스트림을 넣는다(kiro_cli·github 등).
    fn insert_stream_src(conn: &Connection, id: &str, source: &str, project: &str, title: &str, ts: i64) {
        insert_stream(conn, id, source, "session", Some(project), Some(title), ts, ts + 1_000);
    }

    #[test]
    fn kiro_session_outside_repo_goes_to_unassigned() {
        // 실측: Kiro CLI 세션이 hub 표시 없이 project=~/.agent-hub(레포 밖)로 남아 `## .agent-hub` 칸이 생겼다.
        let (tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        let outside = tmp.path.join("outside-folder");
        let repo = tmp.path.join("real-repo");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();
        let outside = outside.to_string_lossy().to_string();
        let repo = repo.to_string_lossy().to_string();
        assert!(crate::capture::normalize::find_repo_root(&outside).is_none(), "전제: 임시 폴더는 레포 밖");

        insert_stream_src(&conn, "kiro_cli:s1", "kiro_cli", &outside, "kiro 세션", start_ms + 1_000);
        insert_event(
            &conn, "k1", "kiro_cli:s1", start_ms + 1_000, "kiro_cli", "prompt",
            Some("스테이징 로그에서 오류 건수 확인해줘"), None, "ext-k1",
        );
        insert_stream_src(&conn, "claude_code:r1", "claude_code", &repo, "레포 세션", start_ms + 2_000);
        insert_prompt(&conn, "r1", "claude_code:r1", start_ms + 2_000, "레포 안에서 테스트 고쳐줘 부탁해요");

        let excerpt = build_daily_excerpt_with_excludes(&conn, "2026-07-16", "Asia/Seoul", "ko", &[]).unwrap();
        assert!(!excerpt.contains("## outside-folder"), "레포 밖 폴더 이름이 섹션이 되면 안 됨: {excerpt}");
        let unassigned_at = excerpt.find("## 레포 미지정").expect("레포 미지정 섹션");
        let repo_at = excerpt.find("## real-repo").expect("레포 안 세션은 그대로");
        assert!(excerpt[unassigned_at..repo_at].contains("스테이징 로그에서 오류 건수 확인해줘"), "{excerpt}");
        assert!(excerpt.contains("알려진 프로젝트: real-repo\n"), "레포 밖 폴더는 알려진 프로젝트에서도 빠짐: {excerpt}");
    }

    #[test]
    fn known_projects_skip_excluded_and_non_session_sources() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        seed_hub_day(&conn, start_ms);
        insert_stream_src(&conn, "claude_code:secret", "claude_code", "/Users/x/git/secret-repo", "비밀", start_ms - DAY_MS);
        insert_stream_src(&conn, "github:gh", "github", "owner/gh-only-repo", "PR", start_ms - DAY_MS);

        let excludes = vec!["/Users/x/git/secret-repo".to_string()];
        let excerpt =
            build_daily_excerpt_with_excludes(&conn, "2026-07-16", "Asia/Seoul", "ko", &excludes).unwrap();
        let known = excerpt.lines().find(|l| l.starts_with("알려진 프로젝트: ")).expect("알려진 프로젝트 줄");
        assert_eq!(known, "알려진 프로젝트: other-repo, logroom", "제외 프로젝트·세션 외 소스는 빠짐");
    }

    #[test]
    fn hub_unassigned_is_excluded_from_project_count_and_known_projects() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        seed_hub_day(&conn, start_ms);
        // 20일 전 활동한 레포(그날 섹션은 없지만 알려진 프로젝트에는 들어가야 함)와 40일 전 레포(창 밖).
        insert_stream(
            &conn,
            "claude_code:recent",
            "claude_code",
            "session",
            Some("/Users/x/git/recent-repo"),
            Some("최근"),
            start_ms - 20 * DAY_MS,
            start_ms - 20 * DAY_MS + 1_000,
        );
        insert_stream(
            &conn,
            "claude_code:old",
            "claude_code",
            "session",
            Some("/Users/x/git/old-repo"),
            Some("오래됨"),
            start_ms - 40 * DAY_MS,
            start_ms - 40 * DAY_MS + 1_000,
        );
        // 레포 신호가 없어 hub 폴더에 남은 서브에이전트 스트림 — 알려진 프로젝트에 나오면 안 됨.
        insert_stream_with_meta(
            &conn,
            "claude_code:hub:agent:a1",
            "agent",
            "/Users/x/.agent-hub",
            "서브에이전트",
            start_ms + 1_500,
            start_ms + 1_800,
            r#"{"hub":true}"#,
        );

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "ko").unwrap();
        assert!(
            excerpt.contains("활동 요약: 프로젝트 2개 · 세션 3개 · 요청 3건"),
            "레포 미지정은 프로젝트 수에서 빠짐(세션·요청 수엔 포함): {excerpt}"
        );
        let known = excerpt
            .lines()
            .find(|l| l.starts_with("알려진 프로젝트: "))
            .expect("통계 줄 다음에 알려진 프로젝트 줄");
        assert_eq!(known, "알려진 프로젝트: other-repo, logroom, recent-repo");
        let stats_idx = excerpt.lines().position(|l| l.starts_with("활동 요약: ")).unwrap();
        assert_eq!(excerpt.lines().nth(stats_idx + 1), Some(known));
    }

    #[test]
    fn hub_only_day_still_builds_excerpt_in_english() {
        // 레포 신호 없는 hub 기록만 있는 날도 발췌는 만들어진다(no_data 아님) — en 로케일 제목.
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _end_ms) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream_with_meta(
            &conn,
            "claude_code:hub",
            "session",
            "/Users/x/.agent-hub",
            "hub",
            start_ms + 1_000,
            start_ms + 2_000,
            r#"{"hub":true}"#,
        );
        insert_prompt(&conn, "hub-en", "claude_code:hub", start_ms + 1_000, "check the staging database row counts");

        let excerpt = build_daily_excerpt(&conn, "2026-07-16", "Asia/Seoul", "en").unwrap();
        assert!(excerpt.contains("Activity: 0 projects · 1 sessions · 1 requests"), "{excerpt}");
        assert!(excerpt.contains("## Unassigned"));
        assert!(!excerpt.contains("Known projects:"), "알려진 프로젝트가 없으면 줄 생략");
    }
}
