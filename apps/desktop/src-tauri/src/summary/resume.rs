//! 재개 브리핑(M7-②) — "어디까지 했지?"에 답하는 **프로젝트 단위** 브리핑. 일간/주간/월간
//! 요약(시간 단위·회고)과 달리 재개 지향(미완결·다음 할 일·열린 결정 강조)이며, **캐시 저장이
//! 없다** — 프로젝트 상태는 계속 바뀌므로 항상 최신 데이터로 온디맨드 생성한다.
//!
//! 발췌는 4블록으로 구성한다:
//! 0. **지금 상태**(git) — 매칭 스트림 중 source가 claude_code/kiro_cli이고 project가 절대경로인
//!    스트림 중 가장 최근 활동 스트림의 로컬 git 저장소 상태(현재 브랜치·언커밋 변경·최근 커밋).
//!    과거 이벤트 로그(아래 3블록)와 달리 "지금 사실"이라 프롬프트가 최우선 근거로 삼도록 지시한다
//!    (배경: GitHub 폴러가 5분 간격이라 연 직후 머지된 PR을 "opened"로만 남겨, 이벤트 로그만 보면
//!    "미완결"로 오판할 수 있다 — 로컬 git 현재 상태가 더 신뢰할 수 있는 근거).
//! 1. 최근 작업 — 매칭 스트림 중 세션형(claude_code/kiro_cli)의 prompt 라인(`excerpt::fetch_prompt_lines`
//!    재사용, 최신 우선 상위 8스트림).
//! 2. 최근 커밋·PR — 매칭 스트림 중 GitHub 소스의 title distinct 최신 20건.
//! 3. 마지막 세션 끝부분 — 매칭된 세션형 스트림 중 `ended_at` 최대인 스트림의 마지막 prompt 3개
//!    (중단 지점 = 재개점).
//!
//! **프로젝트 표시명 매칭**이 핵심이다: SQL만으로는 project 문자열에서 마지막 `/` 세그먼트를 뽑기
//! 어려워(소스마다 project 키 형식이 다름 — Claude 절대경로 `/…/logroom` vs GitHub `owner/logroom`),
//! 전체 스트림을 로드한 뒤 Rust에서 `excerpt::last_path_segment`로 후처리 필터링한다.

use super::excerpt::{self, MAX_EXCERPT_CHARS, NO_DATA_ERROR_CODE};
use super::{engine, prompts};
use crate::capture::config::SummaryConfig;
use crate::capture::policy::truncate_with_ellipsis;
use crate::capture::scrub;
use crate::query::day_range_ms;
use chrono::{Duration, TimeZone};
use chrono_tz::Tz;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration as StdDuration;

/// git 조회 명령 1개당 타임아웃(초) — 로컬 실행이라 짧게 잡는다(리모트 접근 없음, 순수 로컬 git).
const GIT_COMMAND_TIMEOUT_SECS: u64 = 5;

/// `git status --short` 출력에서 앞부분만 채택할 최대 줄 수(전체 diff가 아니라 변경 파일 목록
/// 요약이 목적이므로 과도하게 길면 잘라낸다).
const GIT_STATUS_MAX_LINES: usize = 10;

/// `git log --oneline` 조회 개수(최근 커밋 흐름 파악용 — 너무 많으면 노이즈).
const GIT_LOG_COMMIT_COUNT: usize = 8;

/// 발췌 조회 기간(일) — "그 프로젝트의 마지막 활동 ts 기준 역산 7일"(오늘 기준이 아님 — 방치된
/// 프로젝트도 마지막 맥락을 잡아야 재개 가능하다는 개념 설계 원칙).
const LOOKBACK_DAYS: i64 = 7;

/// 최근 작업 블록에 채택할 최대 세션형 스트림 수(prompt 이벤트 수와 무관 — 최신 활동 우선).
const MAX_RECENT_SESSION_STREAMS: usize = 8;

/// 최근 커밋·PR 블록에 채택할 최대 title distinct 건수.
const MAX_GITHUB_TITLES: usize = 20;
/// GitHub title 최대 표시 길이(excerpt.rs와 동일 값 — 로컬 중복 상수라 값 변경 시 함께 맞춰야 함).
const GITHUB_TITLE_MAX_CHARS: usize = 80;

/// 마지막 세션 끝부분에 채택할 prompt 개수(중단 지점 표시).
const LAST_SESSION_TAIL_COUNT: usize = 3;

/// "최근 작업" 블록에서 스트림 1개당 채택할 최대 prompt 라인 수 — 원래 `excerpt::fetch_prompt_lines`
/// 내부에 하드코딩돼 있던 값(`MAX_PROMPT_LINES_PER_STREAM`)을 그대로 옮겨온 것이다. daily excerpt는
/// 세션당 5개로 잘려 오후 작업이 통째로 소실되는 버그가 있어 전량 채택으로 바뀌었지만(사용자 결정:
/// resume은 이번 개정 대상 제외), 재개 브리핑은 "최근 작업"의 요지만 필요하므로 전량이 아니라 앞
/// 5개로 계속 제한한다.
const RECENT_WORK_PROMPT_LINES: usize = 5;

/// 프로젝트 표시명 매칭 대상 스트림 1건(후보 전체 로드 후 Rust에서 필터링).
struct CandidateStream {
    stream_id: String,
    source: String,
    kind: String,
    title: Option<String>,
    project: String,
    ended_at: i64,
}

/// `project IS NOT NULL`인 전체 스트림을 로드한다(표시명 매칭은 호출부가 수행 — SQL로는 마지막
/// `/` 세그먼트 추출이 어려워 Rust 후처리가 필요하다). project가 없는 스트림은 애초에 이 브리핑의
/// 매칭 대상이 될 수 없으므로 여기서 제외한다.
fn fetch_candidate_streams(conn: &Connection) -> anyhow::Result<Vec<CandidateStream>> {
    let mut stmt = conn.prepare(
        "SELECT id, source, kind, title, project, ended_at FROM streams
         WHERE project IS NOT NULL AND ended_at IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(CandidateStream {
                stream_id: row.get("id")?,
                source: row.get("source")?,
                kind: row.get("kind")?,
                title: row.get("title")?,
                project: row.get("project")?,
                ended_at: row.get("ended_at")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `project_display_name`(마지막 `/` 세그먼트)과 일치하는 스트림만 걸러낸다.
fn matches_project(stream: &CandidateStream, project_display_name: &str) -> bool {
    excerpt::last_path_segment(&stream.project) == project_display_name
}

/// 매칭 스트림 중 가장 최근 `ended_at`(그 프로젝트의 "마지막 활동 ts"). 매칭 스트림이 하나도 없으면
/// `None`([`NO_DATA_ERROR_CODE`]로 이어짐).
fn last_activity_ts(matched: &[&CandidateStream]) -> Option<i64> {
    matched.iter().map(|s| s.ended_at).max()
}

// ── "지금 상태"(git) 블록 ────────────────────────────────────────

/// "지금 상태" 블록 대상 로컬 경로 선정 — 매칭 스트림 중 source가 claude_code/kiro_cli이고
/// project가 절대경로(`/`로 시작)인 스트림들 중 **가장 최근 활동(`ended_at` 최대) 스트림**의
/// project 경로를 채택한다. 그런 스트림이 하나도 없으면(경로 없는 프로젝트 — Slack/Linear 활동만
/// 매칭된 경우) `None` — 이 블록 자체를 생략한다.
fn resolve_git_target_path<'a>(matched: &[&'a CandidateStream]) -> Option<&'a str> {
    matched
        .iter()
        .copied()
        .filter(|s| matches!(s.source.as_str(), "claude_code" | "kiro_cli") && s.project.starts_with('/'))
        .max_by_key(|s| s.ended_at)
        .map(|s| s.project.as_str())
}

/// `path`가 존재하는 디렉토리이고 그 아래 `.git`이 존재할 때만 true(보안 필수 — 검증을 통과한
/// 경로에서만 git 명령을 실행한다. 임의 경로에서의 프로세스 실행을 막기 위한 최소 방어선).
fn is_valid_git_repo(path: &str) -> bool {
    let root = Path::new(path);
    root.is_dir() && root.join(".git").exists()
}

/// 검증을 통과한 경로를 canonicalize해 심볼릭 링크 등을 해소한다(보안 리뷰 Medium — 검증 시점과
/// 실행 시점 사이의 symlink 교체 TOCTOU 여지 축소). canonicalize 후에도 `.git`이 있어야 채택한다.
/// 실패하면 `None` — git 블록 자체를 생략한다.
fn canonical_git_repo(path: &str) -> Option<std::path::PathBuf> {
    let canon = std::fs::canonicalize(path).ok()?;
    if canon.is_dir() && canon.join(".git").exists() {
        Some(canon)
    } else {
        None
    }
}

/// `git -C {path} {args}`를 읽기 전용으로 실행한다(타임아웃 [`GIT_COMMAND_TIMEOUT_SECS`]초,
/// `kill_on_drop`으로 타임아웃 시 좀비 프로세스 방지 — `engine.rs::generate_via_cli`와 동일 패턴).
/// 실패(타임아웃/실행 오류/비정상 종료)하면 조용히 `None` — 이 블록은 "있으면 도움이 되는" 부가
/// 정보라 실패해도 발췌 생성 전체를 막지 않는다.
async fn run_git_readonly(path: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    // 대상 저장소의 `.git/config`·전역 gitconfig에 심어진 fsmonitor 데몬/훅이 `git status` 등
    // 조회 시 임의 프로세스를 실행하는 벡터를 차단한다(보안 리뷰 Low — 캡처 경로가 신뢰할 수 없는
    // 제3자 클론일 수 있음). `-c`(전역 옵션)는 서브커맨드 앞에 와야 하며 config 파일보다 우선한다.
    cmd.arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-C")
        .arg(path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let child = cmd.spawn().ok()?;
    let output =
        tokio::time::timeout(StdDuration::from_secs(GIT_COMMAND_TIMEOUT_SECS), child.wait_with_output())
            .await
            .ok()?
            .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// gh 조회 타임아웃(초) — `git`(순수 로컬)과 달리 GitHub API를 호출하는 네트워크 작업이라 조금 더
/// 길게 잡는다. gh 미설치/미인증/네트워크 실패는 조용히 생략(git 블록의 "있으면 도움" 원칙).
const GH_COMMAND_TIMEOUT_SECS: u64 = 10;
/// "열린 PR" 목록에 채택할 최대 건수.
const GH_OPEN_PR_LIMIT: usize = 20;

/// `gh pr list`(현재 저장소의 열린 PR)를 조회해 `"#N title (branch)"` 목록 문자열로 만든다.
/// **열린 PR이 없으면 `Some(빈 벡터가 아니라)` "없음" 신호를 위해 `Some(vec![])`를 반환**하고,
/// gh 미설치/미인증/네트워크 실패 등 조회 자체가 불가하면 `None`(항목 생략).
///
/// 배경: GitHub 폴러가 5분 간격이라 연 직후 머지된 PR을 이벤트 로그에 "opened"로만 남겨 재개
/// 브리핑이 "미완결"로 오판했다(실사용). `gh pr list`는 **지금 실제로 열려 있는** PR만 주므로 이
/// 오판을 없앤다. gh는 대상 경로의 git remote로 저장소를 판단하므로 `current_dir`을 그 경로로 둔다.
async fn fetch_open_prs(path: &Path) -> Option<Vec<String>> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.arg("pr")
        .arg("list")
        .arg("--state")
        .arg("open")
        .arg("--limit")
        .arg(GH_OPEN_PR_LIMIT.to_string())
        .arg("--json")
        .arg("number,title,headRefName")
        .current_dir(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let child = cmd.spawn().ok()?;
    let output =
        tokio::time::timeout(StdDuration::from_secs(GH_COMMAND_TIMEOUT_SECS), child.wait_with_output())
            .await
            .ok()?
            .ok()?;
    if !output.status.success() {
        return None;
    }
    let parsed: Value = serde_json::from_slice(&output.stdout).ok()?;
    let arr = parsed.as_array()?;
    Some(
        arr.iter()
            .filter_map(|pr| {
                let number = pr.get("number")?.as_i64()?;
                let title = pr.get("title")?.as_str()?;
                let branch = pr.get("headRefName")?.as_str().unwrap_or("");
                Some(format!("#{number} {title} ({branch})"))
            })
            .collect(),
    )
}

/// "## 지금 상태"(git) 블록 렌더 — 현재 브랜치 + 언커밋 변경(`git status --short` 상위
/// [`GIT_STATUS_MAX_LINES`]줄) + 최근 커밋([`GIT_LOG_COMMIT_COUNT`]개, `git log --oneline`) +
/// **열린 PR**(`gh pr list`)을 순서대로 조회한다. 각 항목은 독립적으로 실패할 수 있으며(조용히
/// 생략), 모두 실패하면 빈 문자열을 반환해 호출부가 이 블록 자체를 건너뛰게 한다.
async fn render_git_status_section(path: &Path, locale: &str) -> String {
    let mut out = String::new();

    if let Some(branch) = run_git_readonly(path, &["branch", "--show-current"]).await {
        let label = if locale == "ko" { "현재 브랜치" } else { "Current branch" };
        out.push_str(&format!("{label}: {branch}\n"));
    }

    if let Some(status) = run_git_readonly(path, &["status", "--short"]).await {
        let lines: Vec<&str> = status.lines().take(GIT_STATUS_MAX_LINES).collect();
        if !lines.is_empty() {
            let label = if locale == "ko" { "언커밋 변경" } else { "Uncommitted changes" };
            out.push_str(&format!("{label}:\n"));
            for line in lines {
                out.push_str("- ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    if let Some(log) =
        run_git_readonly(path, &["log", "--oneline", &format!("-{GIT_LOG_COMMIT_COUNT}")]).await
    {
        let label = if locale == "ko" { "최근 커밋" } else { "Recent commits" };
        out.push_str(&format!("{label}:\n"));
        for line in log.lines() {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
    }

    // 열린 PR — gh가 없거나 실패하면 None(생략). 조회는 됐지만 열린 PR이 0건이면 "없음"을 명시해
    // LLM이 이벤트 로그의 "PR opened"를 미완결로 오판하지 않게 한다(이 신호가 핵심 효과).
    if let Some(prs) = fetch_open_prs(path).await {
        let label = if locale == "ko" { "열린 PR" } else { "Open PRs" };
        if prs.is_empty() {
            let none = if locale == "ko" { "없음 (열린 PR 없음)" } else { "none" };
            out.push_str(&format!("{label}: {none}\n"));
        } else {
            out.push_str(&format!("{label}:\n"));
            for pr in prs {
                out.push_str("- ");
                out.push_str(&pr);
                out.push('\n');
            }
        }
    }

    out
}

/// 세션형(claude_code/kiro_cli, kind != 'agent') 매칭 스트림 중 `ended_at` 내림차순 상위
/// [`MAX_RECENT_SESSION_STREAMS`]개 — "최근 작업" 블록의 채택 대상.
fn recent_session_streams<'a>(matched: &[&'a CandidateStream]) -> Vec<&'a CandidateStream> {
    let mut sessions: Vec<&CandidateStream> = matched
        .iter()
        .copied()
        .filter(|s| matches!(s.source.as_str(), "claude_code" | "kiro_cli") && s.kind != "agent")
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.ended_at));
    sessions.truncate(MAX_RECENT_SESSION_STREAMS);
    sessions
}

/// "## 최근 작업" 블록 렌더 — 스트림별 `### {title}` 헤더 + `excerpt::fetch_prompt_lines`(캡처 정책과
/// 동일한 라인 채택 규칙)로 뽑은 prompt 라인들. 시간창은 스트림 자체 시작~종료 전체(발췌 기간
/// `[start_ms, end_ms)` 내로 한정하지 않음 — 이미 `matched` 자체가 그 기간에 활동한 스트림이므로
/// 이벤트 조회는 스트림의 전체 생애로 충분하다).
fn render_recent_work_section(
    conn: &Connection,
    streams: &[&CandidateStream],
    locale: &str,
) -> anyhow::Result<String> {
    let mut out = String::new();
    for stream in streams {
        let title = stream
            .title
            .as_deref()
            .map(|t| truncate_with_ellipsis(t, crate::capture::policy::STREAM_TITLE_MAX_CHARS))
            .unwrap_or_else(|| stream.stream_id.clone());
        out.push_str(&format!("### {title}\n"));
        // 스트림 전체 생애(0..i64::MAX)에서 prompt 라인을 뽑는다 — day_range_ms처럼 하루 단위로
        // 잘린 시간창이 아니라 스트림 단위 조회이므로 넓은 범위를 그대로 연다. "최근 작업"은 요지만
        // 필요하므로 앞 RECENT_WORK_PROMPT_LINES개로 제한한다(시각 접두는 붙이지 않음 — 기존 출력
        // 유지, v5 대상 제외). `PromptLine::reply`("요청 → 결과" 페어링, daily 발췌 전용 보강)는
        // 의도적으로 쓰지 않는다 — resume은 이번 개정 대상 제외(사용자 결정), text만 출력해 기존
        // 재개 브리핑 출력을 그대로 유지한다.
        for line in
            excerpt::fetch_prompt_lines(conn, &stream.stream_id, 0, i64::MAX, Some(RECENT_WORK_PROMPT_LINES))?
        {
            out.push_str("- ");
            out.push_str(&line.text);
            out.push('\n');
        }
        let _ = locale; // 현재 이 섹션은 라벨을 쓰지 않는다(헤더가 title 그대로) — 향후 라벨 추가 대비 자리만 확보.
        out.push('\n');
    }
    Ok(out)
}

/// 매칭 스트림 중 GitHub 소스 stream_id 목록.
fn github_stream_ids(matched: &[&CandidateStream]) -> Vec<String> {
    matched
        .iter()
        .filter(|s| s.source == "github")
        .map(|s| s.stream_id.clone())
        .collect()
}

/// GitHub 스트림들의 이벤트에서 title distinct 최신 [`MAX_GITHUB_TITLES`]건을 가져온다. LLM이 여기서
/// 열린 PR·미완결을 스스로 추론하도록 원본 title 문자열을 그대로 둔다(가공·요약하지 않음).
fn fetch_github_titles(conn: &Connection, stream_ids: &[String]) -> anyhow::Result<Vec<String>> {
    if stream_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = stream_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT title, MAX(ts) AS ts FROM events
         WHERE stream_id IN ({placeholders}) AND title IS NOT NULL
         GROUP BY title
         ORDER BY ts DESC
         LIMIT {MAX_GITHUB_TITLES}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params = rusqlite::params_from_iter(stream_ids.iter());
    let rows = stmt
        .query_map(params, |row| row.get::<_, String>("title"))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn render_github_section(titles: &[String]) -> String {
    let mut out = String::new();
    for title in titles {
        out.push_str("- ");
        out.push_str(&truncate_with_ellipsis(title, GITHUB_TITLE_MAX_CHARS));
        out.push('\n');
    }
    out
}

/// 매칭된 세션형 스트림 중 `ended_at`이 가장 큰 스트림(=마지막으로 활동이 끝난 스트림) 하나.
fn last_ended_session_stream<'a>(sessions: &[&'a CandidateStream]) -> Option<&'a CandidateStream> {
    sessions.iter().copied().max_by_key(|s| s.ended_at)
}

/// 스트림 하나의 마지막 prompt N개(중단 지점 재구성용). `excerpt::fetch_prompt_lines`는 `limit`으로
/// ts 오름차순 앞부분 N개를 채택하는 함수라 "마지막" 요구와 맞지 않아, 이 함수는 전체 라인을 뽑은
/// 뒤 끝에서부터 [`LAST_SESSION_TAIL_COUNT`]개를 취한다(순서는 원래 시간순 그대로 유지).
fn last_prompt_lines(conn: &Connection, stream_id: &str, take: usize) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT title, body FROM events
         WHERE stream_id = ?1 AND type = 'prompt'
         ORDER BY ts DESC
         LIMIT ?2",
    )?;
    // MEANINGFUL_PROMPT_MIN_CHARS/노이즈 필터는 excerpt::fetch_prompt_lines와 동일 기준을 쓰고 싶지만
    // 그 함수는 "앞에서부터 N개"만 반환하므로, 여기서는 넉넉히(take의 4배) 뽑아 같은 필터를 적용한
    // 뒤 최신 take개만 남긴다 — 노이즈/짧은 프롬프트가 섞여도 유효 개수를 채울 여유를 둔다.
    let fetch_limit = (take as i64) * 4;
    let rows = stmt
        .query_map(params![stream_id, fetch_limit], |row| {
            let title: Option<String> = row.get("title")?;
            let body: Option<String> = row.get("body")?;
            Ok((title, body))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut lines = Vec::new();
    for (title, body) in rows {
        if lines.len() >= take {
            break;
        }
        let raw = title.filter(|s| !s.trim().is_empty()).or(body).unwrap_or_default();
        let trimmed = raw.trim();
        if trimmed.chars().count() < crate::capture::policy::MEANINGFUL_PROMPT_MIN_CHARS {
            continue;
        }
        let flattened = trimmed.replace('\n', " ");
        lines.push(truncate_with_ellipsis(&flattened, 90));
    }
    // DESC로 가져왔으니(최신 먼저) 원래 시간순으로 되돌린다 — "끝부분"이라는 서사가 시간 흐름대로
    // 읽혀야 LLM이 중단 지점을 올바르게 재구성한다.
    lines.reverse();
    Ok(lines)
}

/// 로케일별 섹션 헤딩(ADR-0016 관례와 동일하게 locale별 문자열).
struct ResumeHeadings {
    title: &'static str,
    current_state: &'static str,
    recent_work: &'static str,
    github: &'static str,
    last_session_tail: &'static str,
}

fn resume_headings_for_locale(locale: &str) -> ResumeHeadings {
    if locale == "ko" {
        ResumeHeadings {
            title: "# 재개 브리핑 발췌 — {project}",
            current_state: "## 지금 상태",
            recent_work: "## 최근 작업",
            github: "## 최근 커밋·PR",
            last_session_tail: "## 마지막 세션 끝부분",
        }
    } else {
        ResumeHeadings {
            title: "# Resume briefing excerpt — {project}",
            current_state: "## Current state",
            recent_work: "## Recent work",
            github: "## Recent commits/PRs",
            last_session_tail: "## Last session tail",
        }
    }
}

/// [`build_resume_excerpt_body`]의 반환값 — 발췌를 "제목 라인"과 "본문(최근 작업/커밋·PR/끝부분)"
/// 으로 나눠 담는다. git "지금 상태" 블록은 이 둘 사이에 끼워 넣어야 하므로(발췌 맨 위, 제목
/// 바로 아래) 미리 분리해둔다. `git_target_path`는 git 조회 대상 로컬 경로(검증 전, 있으면).
struct ResumeExcerptParts {
    title_line: String,
    body: String,
    git_target_path: Option<String>,
}

/// 재개 브리핑 발췌 생성(동기, SQL 조회 전용) — `project_display_name`(마지막 `/` 세그먼트 기준
/// 표시 이름)에 매칭되는 스트림들의 최근 작업/커밋·PR/마지막 세션 끝부분 3블록 + git 조회 대상
/// 경로(있으면)를 만든다.
///
/// git 조회(`tokio::process::Command`, async)는 이 함수 밖에서 수행한다 — 이 함수는 `Connection`
/// 락을 쥔 채(짧은 동기 SQL 조회) 호출되므로(`generate_resume_briefing`/`lib.rs::preview_resume_input`
/// 참고), 여기서 await하면 `MutexGuard`(비-Send)가 await 경계를 넘어 멀티스레드 런타임에서
/// 컴파일이 안 된다. 그래서 이 함수는 **동기로 남기고** git 대상 경로만 계산해 반환하며,
/// [`build_resume_excerpt`]가 락 해제 후 git 블록을 조회해 제목과 본문 사이에 끼워 넣는다.
///
/// 매칭 스트림이 하나도 없거나(그 표시명으로 활동한 적이 없음), 매칭 스트림은 있지만 마지막 활동
/// ts를 구할 수 없으면(방어적 — ended_at은 NOT NULL 조건으로 이미 걸렀으므로 실질적으로는 매칭
/// 자체가 없는 경우와 같다) [`NO_DATA_ERROR_CODE`].
fn build_resume_excerpt_body(
    conn: &Connection,
    project_display_name: &str,
    tz: &str,
    locale: &str,
) -> anyhow::Result<ResumeExcerptParts> {
    let candidates = fetch_candidate_streams(conn)?;
    let matched: Vec<&CandidateStream> =
        candidates.iter().filter(|s| matches_project(s, project_display_name)).collect();

    let Some(last_ts) = last_activity_ts(&matched) else {
        anyhow::bail!("{NO_DATA_ERROR_CODE}");
    };

    // "오늘 기준 7일"이 아니라 "그 프로젝트의 마지막 활동 ts 기준 역산 7일" — 마지막 활동이 속한
    // 로컬 날짜의 자정(24:00)을 종료 시점으로 잡고 거기서 LOOKBACK_DAYS일 전 자정을 시작 시점으로
    // 삼는다(day_range_ms 재사용 — 다이제스트/일간 요약과 동일한 "하루" 정의).
    let zone: Tz = tz.parse().map_err(|_| anyhow::anyhow!("invalid timezone: {tz}"))?;
    let last_local_date = zone
        .timestamp_millis_opt(last_ts)
        .single()
        .ok_or_else(|| anyhow::anyhow!("invalid last activity timestamp"))?
        .format("%Y-%m-%d")
        .to_string();
    let (_, end_ms) = day_range_ms(&last_local_date, tz)?;
    let start_local_date = (zone
        .timestamp_millis_opt(last_ts)
        .single()
        .expect("위에서 이미 검증")
        .naive_local()
        .date()
        - Duration::days(LOOKBACK_DAYS))
    .format("%Y-%m-%d")
    .to_string();
    let (start_ms, _) = day_range_ms(&start_local_date, tz)?;

    // 매칭 스트림을 발췌 기간(마지막 활동 기준 역산 7일) 내로 좁힌다 — 프로젝트가 오래됐으면 이
    // 필터가 없을 경우 몇 달치 스트림이 전부 후보에 들어가 "최근"이라는 의미가 사라진다.
    let matched_in_window: Vec<&CandidateStream> =
        matched.into_iter().filter(|s| s.ended_at >= start_ms && s.ended_at < end_ms).collect();

    if matched_in_window.is_empty() {
        anyhow::bail!("{NO_DATA_ERROR_CODE}");
    }

    // git 조회 대상 경로는 여기서 계산만 해두고(순수 문자열 선택, I/O 없음), 실제 git 실행은
    // 호출부가 락 해제 후 별도로 수행한다(위 함수 문서 참고).
    let git_target_path = resolve_git_target_path(&matched_in_window).map(str::to_string);

    let headings = resume_headings_for_locale(locale);
    let title_line = headings.title.replace("{project}", project_display_name);

    let mut body = String::new();
    let session_streams = recent_session_streams(&matched_in_window);
    if !session_streams.is_empty() {
        body.push_str(headings.recent_work);
        body.push_str("\n\n");
        body.push_str(&render_recent_work_section(conn, &session_streams, locale)?);
    }

    let gh_ids = github_stream_ids(&matched_in_window);
    let gh_titles = fetch_github_titles(conn, &gh_ids)?;
    if !gh_titles.is_empty() {
        body.push_str(headings.github);
        body.push_str("\n\n");
        body.push_str(&render_github_section(&gh_titles));
        body.push('\n');
    }

    if let Some(last_stream) = last_ended_session_stream(&session_streams) {
        let tail = last_prompt_lines(conn, &last_stream.stream_id, LAST_SESSION_TAIL_COUNT)?;
        if !tail.is_empty() {
            body.push_str(headings.last_session_tail);
            body.push_str("\n\n");
            for line in &tail {
                body.push_str("- ");
                body.push_str(line);
                body.push('\n');
            }
            body.push('\n');
        }
    }

    Ok(ResumeExcerptParts { title_line, body, git_target_path })
}

/// [`ResumeExcerptParts`](제목/본문/git 대상 경로, 순수 데이터)로부터 "지금 상태"(git) 블록을
/// 조회해 제목과 본문 사이에 끼워 넣고 최종 스크럽·길이 안전컷까지 적용한 완성 발췌를 만든다.
/// **`Connection`을 전혀 받지 않는다** — DB 락을 쥔 스코프 안에서 이 함수를 호출하면 안 된다는
/// 뜻이다: `parts`를 만드는 [`build_resume_excerpt_body`](동기 SQL 조회)는 락 안에서 호출하고,
/// 락 해제 후 그 결과(`parts`)를 갖고 이 함수를 호출해야 한다 — 그래야 git 실행(`tokio::process::
/// Command`, await)이 `MutexGuard`(비-Send)를 들고 있지 않게 된다(`generate_resume_briefing`/
/// `lib.rs::preview_resume_input` 양쪽 모두 이 순서를 따른다).
///
/// "지금 상태"(git)는 과거 이벤트 로그가 아니라 로컬 저장소의 현재 사실이라 발췌 맨 위(최우선
/// 근거)에 둔다 — 대상 경로가 없거나 [`is_valid_git_repo`] 검증에 실패하면 이 블록 자체를 생략한다
/// (임의 경로 실행 금지).
///
/// 발송 직전 `scrub::scrub_text` + [`MAX_EXCERPT_CHARS`] 안전컷을 daily/period 발췌와 동일하게
/// 적용한다(외부 CLI/API로 나가는 첫 경로 — 2차 방어). "지금 상태" 블록(git 커밋 메시지 등)도 이
/// 최종 스크럽을 그대로 통과하므로 별도 처리가 필요 없다 — 순서만 기존과 동일하게 유지한다.
async fn finalize_resume_excerpt(parts: ResumeExcerptParts, locale: &str) -> String {
    let headings = resume_headings_for_locale(locale);

    let mut out = String::new();
    out.push_str(&parts.title_line);
    out.push_str("\n\n");

    // is_valid_git_repo(빠른 1차 필터) → canonical_git_repo(symlink 해소, 실행 직전 재검증)
    // 순으로 통과한 경로에서만 git 조회한다(TOCTOU 축소 — 실행에는 canonical 경로를 쓴다).
    if let Some(git_path) = parts.git_target_path.as_deref() {
        if is_valid_git_repo(git_path) {
            if let Some(canon) = canonical_git_repo(git_path) {
                let git_section = render_git_status_section(&canon, locale).await;
                if !git_section.is_empty() {
                    out.push_str(headings.current_state);
                    out.push_str("\n\n");
                    out.push_str(&git_section);
                    out.push('\n');
                }
            }
        }
    }

    out.push_str(&parts.body);

    let scrubbed = scrub::scrub_text(out.trim_end());
    truncate_with_ellipsis(&scrubbed, MAX_EXCERPT_CHARS)
}

/// 재개 브리핑 발췌 생성 — DB를 잠근 짧은 스코프 안에서 [`build_resume_excerpt_body`](동기 SQL
/// 조회)로 파츠를 만들고, 락 해제 후 [`finalize_resume_excerpt`](git 조회 포함, async)로 완성
/// 발췌를 만든다. `lib.rs::preview_resume_input`/[`generate_resume_briefing`]이 공유하는 진입점.
pub async fn build_resume_excerpt(
    db: &crate::Db,
    project_display_name: &str,
    tz: &str,
    locale: &str,
) -> anyhow::Result<String> {
    let parts = {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
        build_resume_excerpt_body(&conn, project_display_name, tz, locale)?
    };
    Ok(finalize_resume_excerpt(parts, locale).await)
}

/// 재개 브리핑 생성 — 발췌(`build_resume_excerpt`) → 프롬프트 조립 → 엔진 호출까지 수행한다.
/// daily/period와 달리 **캐시 저장을 하지 않는다**(개념 설계: 프로젝트 상태는 계속 바뀌므로 항상
/// 최신 발췌로 다시 생성해야 함). 반환 shape은 daily의 반환값을 참고하되 `localDate`/`tz` 대신
/// `project`만 담는다.
///
/// `build_resume_excerpt`가 내부적으로 DB 락을 짧게(SQL 조회만) 쥐었다 놓고, git 조회·엔진 호출
/// (`engine::generate`, 네트워크/프로세스 await)은 락 밖에서 수행한다 — `mod.rs::generate_daily_and_cache`
/// 와 동일한 lock-release-await 원칙(await 구간에서 뮤텍스를 들고 있으면 다른 커맨드가 그 사이
/// 막힌다).
pub async fn generate_resume_briefing(
    db: &crate::Db,
    project: &str,
    tz: &str,
    locale: &str,
    cfg: &SummaryConfig,
) -> anyhow::Result<Value> {
    let excerpt = build_resume_excerpt(db, project, tz, locale).await?;
    let prompt = prompts::resume_prompt(locale);
    let combined = format!("{prompt}\n\n{excerpt}");
    let generated = engine::generate(&combined, cfg).await?;

    Ok(json!({
        "project": project,
        "content": generated.content,
        "engine": generated.engine,
        "model": generated.model,
        "createdAt": now_ms(),
    }))
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).expect("시스템 시간").as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
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
                "logroom-resume-test-{}-{n}-{nanos}",
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

    /// 테스트 전용 동기 헬퍼 — `build_resume_excerpt_body`(SQL 조회, 동기)로 파츠를 만들고
    /// `title_line` + `body`를 그대로 이어붙인 뒤 스크럽·길이컷을 적용한다. 실제 `build_resume_excerpt`
    /// (db: &Db, git 조회 포함, async)와 달리 git "지금 상태" 블록은 만들지 않는다 — 명세대로 실
    /// git/네트워크 실행은 테스트하지 않고, SQL 조회 로직(lookback·매칭·섹션 렌더·스크럽)만 검증한다.
    fn build_resume_excerpt_for_test(
        conn: &Connection,
        project_display_name: &str,
        tz: &str,
        locale: &str,
    ) -> anyhow::Result<String> {
        let parts = build_resume_excerpt_body(conn, project_display_name, tz, locale)?;
        let mut out = String::new();
        out.push_str(&parts.title_line);
        out.push_str("\n\n");
        out.push_str(&parts.body);
        let scrubbed = scrub::scrub_text(out.trim_end());
        Ok(truncate_with_ellipsis(&scrubbed, MAX_EXCERPT_CHARS))
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
            params![id, stream_id, ts, source, event_type, title, body, external_id],
        )
        .unwrap();
    }

    // ── 프로젝트 표시명 매칭 ────────────────────────────────────────

    #[test]
    fn matches_project_uses_last_path_segment() {
        let claude = CandidateStream {
            stream_id: "s1".into(),
            source: "claude_code".into(),
            kind: "session".into(),
            title: None,
            project: "/Users/x/git/logroom".into(),
            ended_at: 1_000,
        };
        let github = CandidateStream {
            stream_id: "s2".into(),
            source: "github".into(),
            kind: "session".into(),
            title: None,
            project: "owner/logroom".into(),
            ended_at: 1_000,
        };
        // 절대경로와 owner/repo 형식이 달라도 마지막 세그먼트("logroom")가 같으면 매칭돼야 한다.
        assert!(matches_project(&claude, "logroom"));
        assert!(matches_project(&github, "logroom"));
        assert!(!matches_project(&claude, "other-project"));
    }

    // ── "지금 상태"(git) 대상 경로 선정/검증(순수함수) ────────────────
    // 실 git 실행·네트워크 호출은 테스트하지 않는다(명세) — 여기서는 경로 선정/검증 로직만 검증한다.

    #[test]
    fn resolve_git_target_path_picks_most_recent_absolute_path_stream() {
        let older = CandidateStream {
            stream_id: "s1".into(),
            source: "claude_code".into(),
            kind: "session".into(),
            title: None,
            project: "/Users/x/git/older-checkout".into(),
            ended_at: 1_000,
        };
        let newer = CandidateStream {
            stream_id: "s2".into(),
            source: "kiro_cli".into(),
            kind: "session".into(),
            title: None,
            project: "/Users/x/git/newer-checkout".into(),
            ended_at: 2_000,
        };
        // GitHub는 project가 절대경로가 아니라(owner/repo) 대상에서 제외돼야 한다.
        let github = CandidateStream {
            stream_id: "s3".into(),
            source: "github".into(),
            kind: "session".into(),
            title: None,
            project: "owner/newer-checkout".into(),
            ended_at: 3_000,
        };
        let matched: Vec<&CandidateStream> = vec![&older, &newer, &github];
        assert_eq!(resolve_git_target_path(&matched), Some("/Users/x/git/newer-checkout"));
    }

    #[test]
    fn resolve_git_target_path_none_when_no_absolute_path_stream() {
        // project가 없거나(Slack/Linear류) 절대경로가 아닌 스트림만 있으면 대상 경로가 없다.
        let github = CandidateStream {
            stream_id: "s1".into(),
            source: "github".into(),
            kind: "session".into(),
            title: None,
            project: "owner/logroom".into(),
            ended_at: 1_000,
        };
        let matched: Vec<&CandidateStream> = vec![&github];
        assert_eq!(resolve_git_target_path(&matched), None);
    }

    #[test]
    fn is_valid_git_repo_false_when_path_does_not_exist() {
        assert!(!is_valid_git_repo("/nonexistent/path/that/should/not/exist-logroom-test"));
    }

    #[test]
    fn is_valid_git_repo_false_when_dir_exists_but_no_dot_git() {
        // 실제 존재하는 디렉토리(임시 디렉토리)지만 .git이 없으면 false여야 한다 — 임의 경로에서
        // git 명령이 실행되지 않도록 막는 최소 방어선(보안 리뷰 필수 검증).
        let tmp = TempDir::new();
        let path_str = tmp.path.to_string_lossy().to_string();
        assert!(!is_valid_git_repo(&path_str));
    }

    #[test]
    fn is_valid_git_repo_true_when_dot_git_exists() {
        let tmp = TempDir::new();
        fs::create_dir_all(tmp.path.join(".git")).unwrap();
        let path_str = tmp.path.to_string_lossy().to_string();
        assert!(is_valid_git_repo(&path_str));
    }

    // ── build_resume_excerpt(통합, seeded_conn) ───────────────────

    #[test]
    fn build_resume_excerpt_returns_no_data_when_no_matching_project() {
        let (_tmp, conn) = seeded_conn();
        let err = build_resume_excerpt_for_test(&conn, "nonexistent", "Asia/Seoul", "ko").unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[test]
    fn build_resume_excerpt_excludes_streams_without_project() {
        let (_tmp, conn) = seeded_conn();
        let now = crate::query::day_range_ms("2026-07-16", "Asia/Seoul").unwrap().0 + 1_000;
        insert_stream(&conn, "claude_code:no-project", "claude_code", "session", None, Some("제목"), now, now + 1_000);
        insert_event(
            &conn,
            "ev-1",
            "claude_code:no-project",
            now + 1_000,
            "claude_code",
            "prompt",
            Some("이 프로젝트는 project가 없어서 제외돼야 합니다"),
            None,
            "ext-1",
        );
        let err = build_resume_excerpt_for_test(&conn, "logroom", "Asia/Seoul", "ko").unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[test]
    fn build_resume_excerpt_uses_last_activity_based_lookback_not_today() {
        // 프로젝트의 마지막 활동이 "오늘"이 아니라 훨씬 이전이어도(방치된 프로젝트), 그 활동
        // 시점 기준 역산 7일 안에 있으면 발췌에 포함돼야 한다.
        let (_tmp, conn) = seeded_conn();
        let old_date = "2020-01-15"; // 아주 오래 전 날짜(오늘 기준으로는 훨씬 밖).
        let (start_ms, _) = crate::query::day_range_ms(old_date, "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:old-sess",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("오래된 세션"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-old",
            "claude_code:old-sess",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("이 프로젝트에서 오래 전에 작업했던 기록입니다"),
            None,
            "ext-old",
        );

        let excerpt = build_resume_excerpt_for_test(&conn, "logroom", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("## 최근 작업"));
        assert!(excerpt.contains("이 프로젝트에서 오래 전에 작업했던 기록입니다"));
    }

    #[test]
    fn build_resume_excerpt_excludes_activity_outside_seven_day_lookback() {
        let (_tmp, conn) = seeded_conn();
        let recent_date = "2026-07-16";
        let (recent_start, _) = crate::query::day_range_ms(recent_date, "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:recent",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("최근 세션"),
            recent_start + 1_000,
            recent_start + 2_000,
        );
        insert_event(
            &conn,
            "ev-recent",
            "claude_code:recent",
            recent_start + 1_000,
            "claude_code",
            "prompt",
            Some("최근 활동이라 발췌에 포함되어야 하는 프롬프트입니다"),
            None,
            "ext-recent",
        );
        // 마지막 활동(recent_date)로부터 7일보다 훨씬 전 — lookback 밖이라 제외돼야 한다.
        let far_date = "2026-06-01";
        let (far_start, _) = crate::query::day_range_ms(far_date, "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:far",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("먼 과거 세션"),
            far_start + 1_000,
            far_start + 2_000,
        );
        insert_event(
            &conn,
            "ev-far",
            "claude_code:far",
            far_start + 1_000,
            "claude_code",
            "prompt",
            Some("먼 과거 활동이라 발췌에서 제외되어야 하는 프롬프트입니다"),
            None,
            "ext-far",
        );

        let excerpt = build_resume_excerpt_for_test(&conn, "logroom", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("최근 활동이라 발췌에 포함되어야 하는 프롬프트입니다"));
        assert!(!excerpt.contains("먼 과거 활동이라 발췌에서 제외되어야 하는 프롬프트입니다"));
    }

    #[test]
    fn build_resume_excerpt_includes_github_commits_and_prs_section() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _) = crate::query::day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "github:owner/logroom",
            "github",
            "session",
            Some("owner/logroom"),
            Some("owner/logroom"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-pr",
            "github:owner/logroom",
            start_ms + 1_000,
            "github",
            "message",
            Some("PR #42 merged: Add feature"),
            None,
            "ext-pr",
        );

        let excerpt = build_resume_excerpt_for_test(&conn, "logroom", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("## 최근 커밋·PR"));
        assert!(excerpt.contains("PR #42 merged: Add feature"));
    }

    #[test]
    fn build_resume_excerpt_last_session_tail_uses_most_recently_ended_stream() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _) = crate::query::day_range_ms("2026-07-16", "Asia/Seoul").unwrap();

        // 먼저 끝난 세션 — 끝부분에 포함되면 안 됨.
        insert_stream(
            &conn,
            "claude_code:earlier",
            "claude_code",
            "session",
            Some("/x/logroom"),
            Some("먼저 끝난 세션"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-e1",
            "claude_code:earlier",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("먼저 끝난 세션의 프롬프트라 마지막 끝부분에 안 나와야 함"),
            None,
            "ext-e1",
        );

        // 나중에 끝난 세션 — 마지막 끝부분에 포함돼야 함.
        insert_stream(
            &conn,
            "claude_code:later",
            "claude_code",
            "session",
            Some("/x/logroom"),
            Some("나중에 끝난 세션"),
            start_ms + 5_000,
            start_ms + 9_000,
        );
        insert_event(
            &conn,
            "ev-l1",
            "claude_code:later",
            start_ms + 5_000,
            "claude_code",
            "prompt",
            Some("나중에 끝난 세션의 첫 프롬프트라 끝부분 3개 안에 안 들어갈 수도 있음"),
            None,
            "ext-l1",
        );
        insert_event(
            &conn,
            "ev-l2",
            "claude_code:later",
            start_ms + 6_000,
            "claude_code",
            "prompt",
            Some("나중에 끝난 세션의 마지막 프롬프트라 끝부분에 반드시 포함되어야 함"),
            None,
            "ext-l2",
        );

        let excerpt = build_resume_excerpt_for_test(&conn, "logroom", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.contains("## 마지막 세션 끝부분"));
        assert!(excerpt.contains("나중에 끝난 세션의 마지막 프롬프트라 끝부분에 반드시 포함되어야 함"));
        // "끝부분" 섹션은 나중에 끝난 스트림 것만 담아야 하므로, 먼저 끝난 세션 프롬프트는 거기 없어야
        // 하지만(최근 작업 섹션에는 남을 수 있음) 최소한 끝부분 헤딩 이후에는 없어야 한다는 것만 검증.
        let tail_pos = excerpt.find("## 마지막 세션 끝부분").unwrap();
        let tail_section = &excerpt[tail_pos..];
        assert!(!tail_section.contains("먼저 끝난 세션의 프롬프트라 마지막 끝부분에 안 나와야 함"));
    }

    #[test]
    fn build_resume_excerpt_scrubs_secrets_before_send() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _) = crate::query::day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:secret-sess",
            "claude_code",
            "session",
            Some("/x/logroom"),
            Some("세션"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-secret",
            "claude_code:secret-sess",
            start_ms + 1_000,
            "claude_code",
            "prompt",
            Some("이 키로 배포해줘 sk-ant-abcdefghijklmnopqrstuvwxyz1234"),
            None,
            "ext-secret",
        );

        let excerpt = build_resume_excerpt_for_test(&conn, "logroom", "Asia/Seoul", "ko").unwrap();
        assert!(!excerpt.contains("sk-ant-abcdefghijklmnopqrstuvwxyz1234"));
        assert!(excerpt.contains("[REDACTED:anthropic]"));
    }

    #[test]
    fn build_resume_excerpt_scrubs_secrets_in_github_and_tail_sections() {
        // 보안 리뷰 Medium: "최근 작업"(prompt) 외에 GitHub title·마지막 세션 끝부분 블록도 발송 직전
        // 스크럽되는지 회귀 검증(로직은 최종 문자열 1회 스크럽이라 안전하나 테스트 공백이었음).
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _) = crate::query::day_range_ms("2026-07-16", "Asia/Seoul").unwrap();

        // GitHub title에 시크릿.
        insert_stream(
            &conn,
            "github:owner/logroom",
            "github",
            "session",
            Some("owner/logroom"),
            Some("owner/logroom"),
            start_ms + 1_000,
            start_ms + 2_000,
        );
        insert_event(
            &conn,
            "ev-gh-secret",
            "github:owner/logroom",
            start_ms + 1_000,
            "github",
            "message",
            Some("PR #1 배포키 sk-ant-ghijklmnopqrstuvwxyz01234567 노출"),
            None,
            "ext-gh-secret",
        );

        // 가장 늦게 끝난 세션(=마지막 세션 끝부분에 채택)의 마지막 prompt에 시크릿.
        insert_stream(
            &conn,
            "claude_code:latest",
            "claude_code",
            "session",
            Some("/x/logroom"),
            Some("마지막 세션"),
            start_ms + 3_000,
            start_ms + 9_000,
        );
        insert_event(
            &conn,
            "ev-tail-secret",
            "claude_code:latest",
            start_ms + 9_000,
            "claude_code",
            "prompt",
            Some("마지막으로 이 키 확인해줘 sk-ant-zyxwvutsrqponmlkjihgfedcba9876"),
            None,
            "ext-tail-secret",
        );

        let excerpt = build_resume_excerpt_for_test(&conn, "logroom", "Asia/Seoul", "ko").unwrap();
        assert!(!excerpt.contains("sk-ant-ghijklmnopqrstuvwxyz01234567"), "GitHub 섹션 시크릿 잔존");
        assert!(!excerpt.contains("sk-ant-zyxwvutsrqponmlkjihgfedcba9876"), "마지막 세션 끝부분 시크릿 잔존");
        assert!(excerpt.contains("[REDACTED:anthropic]"));
    }

    #[test]
    fn build_resume_excerpt_no_data_code_is_locale_independent() {
        let (_tmp, conn) = seeded_conn();
        let err = build_resume_excerpt_for_test(&conn, "nonexistent", "Asia/Seoul", "en").unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[test]
    fn build_resume_excerpt_includes_project_display_name_in_title() {
        let (_tmp, conn) = seeded_conn();
        let (start_ms, _) = crate::query::day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(
            &conn,
            "claude_code:sess-1",
            "claude_code",
            "session",
            Some("/Users/x/git/logroom"),
            Some("세션"),
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
            Some("의미 있는 프롬프트 내용입니다 충분히 길게"),
            None,
            "ext-1",
        );

        let excerpt = build_resume_excerpt_for_test(&conn, "logroom", "Asia/Seoul", "ko").unwrap();
        assert!(excerpt.starts_with("# 재개 브리핑 발췌 — logroom"));

        let excerpt_en = build_resume_excerpt_for_test(&conn, "logroom", "Asia/Seoul", "en").unwrap();
        assert!(excerpt_en.starts_with("# Resume briefing excerpt — logroom"));
    }
}
