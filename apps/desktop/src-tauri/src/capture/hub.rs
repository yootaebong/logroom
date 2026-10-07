//! hub 세션 레포 재귀속.
//!
//! hub 세션 = jsonl 의 cwd 가 git 레포 밖인 Claude 세션(예: agent hub 같은 중간 에이전트가 `~/.agent-hub` 에서
//! 띄운 세션). 이런 세션은 실제로는 여러 레포를 오가며 일하는데 시작 폴더 하나로 뭉쳐 보인다.
//! 여기서는 턴(사용자 prompt 하나 ~ 다음 prompt 직전)마다 도구 호출이 실제로 만진 디렉토리를
//! 레포 신호로 모아 다수결로 레포를 정하고, 그 턴의 이벤트를 `<base_id>@<repo_root>` 자식 스트림으로
//! 옮긴다. 신호가 없는 턴은 base 스트림에 남는다. 제외 레포(`exclude_projects`) 표가 하나라도 있는
//! 턴은 다수결과 무관하게 통째로 지운다. 진행 중 턴이 조각으로 나눠 들어오면 이미 옮긴 앞부분까지
//! 모아 다시 투표하므로 한 번에 처리한 결과와 같다.
//!
//! - 신호 추출은 순수 함수([`extract_dir_candidates`], [`event_repos`])라 resolver 를 인자로 받는다.
//! - 재귀속([`reattribute_hub_stream`])은 base 스트림에 남아 있는 이벤트만 다시 보므로 멱등이다.
//! - 서브에이전트 스트림은 턴 분할 없이 스트림 전체 다수결로 `streams.project` 만 고친다.

use crate::capture::config;
use crate::capture::normalize::find_repo_root;
use crate::capture::policy::is_summarizer_project;
use crate::db;
use crate::model::StreamInput;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// 디렉토리 → 레포 루트(없으면 `None`) 캐시. 실행 중 FS 접근을 줄이기 위해 watch 스레드가 들고 다닌다.
pub type RepoCache = HashMap<String, Option<String>>;

/// 기존 DB 소급 처리(backfill) 완료 마커 — `capture_cursors(source, resource)` 행으로 남긴다.
const BACKFILL_CURSOR_SOURCE: &str = "hub_reattribute";
const BACKFILL_CURSOR_RESOURCE: &str = "v1";
/// 소급 중 재귀속에 실패한 스트림 목록(resource = 스트림 id) — 다음 기동에 이것만 재시도한다.
const BACKFILL_FAILED_CURSOR_SOURCE: &str = "hub_reattribute_failed";
/// 제외 레포 턴을 지운 마지막 prompt ts 기록용 커서 source(resource = base 스트림 id).
/// 지운 턴의 뒷부분(응답 등)이 나중에 들어왔을 때 앞 턴 스트림으로 새지 않게 막는다.
const EXCLUDED_TURN_CURSOR_SOURCE: &str = "hub_excluded_turn";

/// 절대 `file_path`(또는 `notebook_path`)의 부모 디렉토리를 신호로 쓰는 도구들.
const FILE_PATH_TOOLS: &[&str] = &["Read", "Edit", "Write", "MultiEdit", "NotebookEdit"];
/// `input.path`(절대경로일 때만)를 신호로 쓰는 도구들.
const SEARCH_PATH_TOOLS: &[&str] = &["Grep", "Glob"];

// ── 신호 추출(순수 함수) ─────────────────────────────

/// tool_use metadata(JSON 문자열)에서 `input` 객체를 꺼낸다. essential 정책(기본값)은 `input` 대신
/// 512자로 자른 `inputPreview` 문자열만 남기므로, 잘리지 않아 JSON 으로 다시 읽히는 경우에만 쓴다.
fn tool_input(metadata: Option<&str>) -> Option<Value> {
    let meta: Value = serde_json::from_str(metadata?).ok()?;
    if let Some(input) = meta.get("input").filter(|v| v.is_object()) {
        return Some(input.clone());
    }
    let preview = meta.get("inputPreview")?.as_str()?;
    serde_json::from_str::<Value>(preview)
        .ok()
        .filter(Value::is_object)
}

fn input_str<'a>(input: Option<&'a Value>, key: &str) -> Option<&'a str> {
    input?.get(key)?.as_str()
}

/// `~`·`$HOME`·`${HOME}` 를 `home` 으로 펼친다. 결과가 절대경로가 아니면(상대경로·`cd -` 등) 버린다.
fn expand_dir(raw: &str, home: Option<&str>) -> Option<String> {
    let raw = raw.trim();
    let expanded = if raw == "~" || raw == "$HOME" || raw == "${HOME}" {
        home?.to_string()
    } else if let Some(rest) = raw
        .strip_prefix("~/")
        .or_else(|| raw.strip_prefix("$HOME/"))
        .or_else(|| raw.strip_prefix("${HOME}/"))
    {
        format!("{}/{rest}", home?.trim_end_matches('/'))
    } else {
        raw.to_string()
    };
    expanded.starts_with('/').then(|| clean_path(&expanded))
}

/// 절대경로를 글자 단위로 정리한다(`.`·`..`·끝 `/` 제거) — 같은 디렉토리가 다른 캐시 키·레포 키가 되지 않게.
/// 심볼릭 링크는 풀지 않는다(FS 접근 없음).
fn clean_path(path: &str) -> String {
    use std::path::{Component, PathBuf};
    let mut out = PathBuf::new();
    for comp in Path::new(path).components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().to_string()
}

#[derive(Debug, PartialEq)]
enum ShellToken {
    Word(String),
    /// `&&`·`||`·`;`·`|`·`&`·`(`·`)`·개행 — 명령 경계.
    Op,
}

/// 셸 명령을 단어/연산자 토큰으로 나눈다(따옴표 제거·역슬래시 이스케이프만 처리하는 얕은 토크나이저).
fn tokenize_shell(cmd: &str) -> Vec<ShellToken> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();

    let flush = |word: &mut String, in_word: &mut bool, tokens: &mut Vec<ShellToken>| {
        if *in_word {
            tokens.push(ShellToken::Word(std::mem::take(word)));
            *in_word = false;
        }
    };

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(next) = chars.next() {
                                word.push(next);
                            }
                        }
                        _ => word.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    if next != '\n' {
                        word.push(next);
                    }
                }
            }
            '\n' | ';' | '&' | '|' | '(' | ')' => {
                flush(&mut word, &mut in_word, &mut tokens);
                if tokens.last() != Some(&ShellToken::Op) {
                    tokens.push(ShellToken::Op);
                }
            }
            c if c.is_whitespace() => flush(&mut word, &mut in_word, &mut tokens),
            _ => {
                in_word = true;
                word.push(c);
            }
        }
    }
    flush(&mut word, &mut in_word, &mut tokens);
    tokens
}

/// Bash 명령에서 `cd <dir>`(명령 시작/연산자 뒤)와 `git -C <dir>` 의 디렉토리 인자를 뽑는다(펼치기 전 원문).
fn bash_dir_args(command: &str) -> Vec<String> {
    let tokens = tokenize_shell(command);
    let word = |i: usize| match tokens.get(i) {
        Some(ShellToken::Word(w)) => Some(w.as_str()),
        _ => None,
    };
    let mut out = Vec::new();
    for i in 0..tokens.len() {
        let segment_start = i == 0 || tokens[i - 1] == ShellToken::Op;
        if segment_start && word(i) == Some("cd") {
            if let Some(dir) = word(i + 1) {
                out.push(dir.to_string());
            }
        }
        if word(i) == Some("git") && word(i + 1) == Some("-C") {
            if let Some(dir) = word(i + 2) {
                out.push(dir.to_string());
            }
        }
    }
    out
}

/// tool_use 이벤트 하나(title=도구명, body, metadata)에서 디렉토리 후보(절대경로)를 뽑는다.
pub fn extract_dir_candidates(
    tool: &str,
    body: Option<&str>,
    metadata: Option<&str>,
    home: Option<&str>,
) -> Vec<String> {
    let input = tool_input(metadata);
    if tool == "Bash" {
        let command = body.or_else(|| input_str(input.as_ref(), "command"));
        return command
            .map(bash_dir_args)
            .unwrap_or_default()
            .iter()
            .filter_map(|raw| expand_dir(raw, home))
            .collect();
    }
    if FILE_PATH_TOOLS.contains(&tool) {
        let path = body
            .filter(|b| b.starts_with('/'))
            .or_else(|| input_str(input.as_ref(), "file_path"))
            .or_else(|| input_str(input.as_ref(), "notebook_path"));
        return path
            .filter(|p| p.starts_with('/'))
            .map(clean_path)
            .and_then(|p| {
                Path::new(&p)
                    .parent()
                    .map(|dir| dir.to_string_lossy().to_string())
            })
            .map(|dir| vec![dir])
            .unwrap_or_default();
    }
    if SEARCH_PATH_TOOLS.contains(&tool) {
        return input_str(input.as_ref(), "path")
            .filter(|p| p.starts_with('/'))
            .map(|p| vec![clean_path(p)])
            .unwrap_or_default();
    }
    Vec::new()
}

/// tool_use 이벤트 하나가 던지는 레포 표들. 후보 디렉토리를 `resolve`(레포 루트 탐색)로 바꾸고,
/// 레포가 아니거나 일일 요약 작업 디렉토리면 버린다.
pub fn event_repos(
    tool: &str,
    body: Option<&str>,
    metadata: Option<&str>,
    home: Option<&str>,
    resolve: &mut dyn FnMut(&str) -> Option<String>,
) -> Vec<String> {
    extract_dir_candidates(tool, body, metadata, home)
        .iter()
        .filter_map(|dir| resolve(dir))
        .filter(|repo| !is_summarizer_project(repo))
        .collect()
}

/// 다수결. 동점이면 먼저 나온 레포. 표가 없으면 `None`.
fn majority(votes: &[String]) -> Option<String> {
    let mut order: Vec<&str> = Vec::new();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for v in votes {
        let count = counts.entry(v.as_str()).or_insert(0);
        if *count == 0 {
            order.push(v.as_str());
        }
        *count += 1;
    }
    let mut best: Option<(&str, usize)> = None;
    for repo in order {
        let count = counts[repo];
        if best.is_none_or(|(_, c)| count > c) {
            best = Some((repo, count));
        }
    }
    best.map(|(repo, _)| repo.to_string())
}

fn home_dir_string() -> Option<String> {
    dirs::home_dir().map(|h| h.to_string_lossy().to_string())
}

fn cached_resolver(cache: &mut RepoCache) -> impl FnMut(&str) -> Option<String> + '_ {
    move |dir: &str| {
        cache
            .entry(dir.to_string())
            .or_insert_with(|| find_repo_root(dir))
            .clone()
    }
}

// ── DB 재귀속 ───────────────────────────────────────

struct EventRow {
    id: String,
    /// 지금 들어 있는 스트림(base 또는 자식) — 재이동 뒤 시간 범위를 다시 계산할 대상.
    stream_id: String,
    ts: i64,
    event_type: String,
    external_id: String,
    /// 아래 셋은 tool_use 일 때만 채운다(나머지 타입은 신호가 없어 읽지 않음).
    tool: Option<String>,
    body: Option<String>,
    metadata: Option<String>,
}

/// prompt external_id(`<uuid>#<idx>`)의 uuid 부분 — 같은 라인의 prompt 블록들을 한 턴으로 묶는 키.
fn prompt_uuid(external_id: &str) -> &str {
    external_id.split('#').next().unwrap_or_default()
}

const EVENT_ROW_COLUMNS: &str = "id, stream_id, ts, type, COALESCE(external_id, ''),
    CASE WHEN type = 'tool_use' THEN title END,
    CASE WHEN type = 'tool_use' THEN body END,
    CASE WHEN type = 'tool_use' THEN metadata END";

fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        id: row.get(0)?,
        stream_id: row.get(1)?,
        ts: row.get(2)?,
        event_type: row.get(3)?,
        external_id: row.get(4)?,
        tool: row.get(5)?,
        body: row.get(6)?,
        metadata: row.get(7)?,
    })
}

fn load_events(conn: &Connection, stream_id: &str) -> anyhow::Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {EVENT_ROW_COLUMNS} FROM events WHERE stream_id = ?1
         ORDER BY ts ASC, external_id ASC"
    ))?;
    let rows = stmt
        .query_map(params![stream_id], event_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn votes_of<'a>(
    events: impl Iterator<Item = &'a EventRow>,
    home: Option<&str>,
    resolve: &mut dyn FnMut(&str) -> Option<String>,
) -> Vec<String> {
    events
        .filter(|e| e.event_type == "tool_use")
        .flat_map(|e| {
            event_repos(
                e.tool.as_deref().unwrap_or_default(),
                e.body.as_deref(),
                e.metadata.as_deref(),
                home,
                resolve,
            )
        })
        .collect()
}

/// 턴 시작점 = base·자식 전체에서 uuid 가 바뀌는 prompt(같은 uuid 의 연속 prompt 블록은 한 턴).
struct TurnStart {
    ts: i64,
    external_id: String,
    /// 그 prompt 가 지금 들어 있는 스트림(base 또는 자식).
    stream_id: String,
}

impl TurnStart {
    fn key(&self) -> (i64, &str) {
        (self.ts, self.external_id.as_str())
    }
}

/// 자식 id 는 `<base>@...` 라 `[<base>@, <base>A)` 문자열 범위로 찾는다(`@` 다음 문자가 `A`) —
/// LIKE 를 쓰면 id 의 `_` 가 와일드카드가 되고 인덱스도 못 탄다.
fn child_range(base_id: &str) -> (String, String) {
    (format!("{base_id}@"), format!("{base_id}A"))
}

/// base 와 그 자식 스트림 전체의 턴 시작점들((ts, external_id) 오름차순).
fn load_turn_starts(conn: &Connection, base_id: &str) -> anyhow::Result<Vec<TurnStart>> {
    let (lo, hi) = child_range(base_id);
    let mut stmt = conn.prepare(
        "SELECT ts, COALESCE(external_id, ''), stream_id FROM events
         WHERE (stream_id = ?1 OR (stream_id >= ?2 AND stream_id < ?3)) AND type = 'prompt'
         ORDER BY ts ASC, external_id ASC",
    )?;
    let prompts = stmt
        .query_map(params![base_id, lo, hi], |row| {
            Ok(TurnStart {
                ts: row.get(0)?,
                external_id: row.get(1)?,
                stream_id: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut starts: Vec<TurnStart> = Vec::new();
    for p in prompts {
        let same_turn = starts
            .last()
            .is_some_and(|s| prompt_uuid(&s.external_id) == prompt_uuid(&p.external_id));
        if !same_turn {
            starts.push(p);
        }
    }
    Ok(starts)
}

/// 자식 스트림들에서 `[from, to)`((ts, external_id) 기준, `to` 가 `None` 이면 끝까지) 이벤트 —
/// 진행 중 턴 가운데 이미 자식으로 옮겨진 앞부분을 다시 모을 때 쓴다.
fn load_child_turn_events(
    conn: &Connection,
    base_id: &str,
    from: (i64, &str),
    to: Option<(i64, &str)>,
) -> anyhow::Result<Vec<EventRow>> {
    let (lo, hi) = child_range(base_id);
    let mut stmt = conn.prepare(&format!(
        "SELECT {EVENT_ROW_COLUMNS} FROM events
         WHERE stream_id >= ?1 AND stream_id < ?2
           AND (ts > ?3 OR (ts = ?3 AND COALESCE(external_id, '') >= ?4))
           AND (?5 IS NULL OR ts < ?5 OR (ts = ?5 AND COALESCE(external_id, '') < ?6))
         ORDER BY ts ASC, external_id ASC"
    ))?;
    let rows = stmt
        .query_map(
            params![lo, hi, from.0, from.1, to.map(|t| t.0), to.map(|t| t.1)],
            event_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

struct BaseStream {
    source: String,
    title: Option<String>,
    git_branch: Option<String>,
    entrypoint: Option<String>,
}

fn load_base_stream(conn: &Connection, id: &str) -> anyhow::Result<Option<BaseStream>> {
    let row = conn
        .query_row(
            "SELECT source, title, git_branch,
                    CASE WHEN json_valid(metadata) THEN json_extract(metadata, '$.entrypoint') END
             FROM streams WHERE id = ?1",
            params![id],
            |row| {
                Ok(BaseStream {
                    source: row.get(0)?,
                    title: row.get(1)?,
                    git_branch: row.get(2)?,
                    entrypoint: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn child_stream_id(base_id: &str, repo: &str) -> String {
    format!("{base_id}@{repo}")
}

enum Target {
    /// `<base>@<repo>` 자식 스트림으로 이동.
    Move(String),
    /// 제외 레포 — 이벤트 삭제.
    Delete,
}

/// 표들로 귀속을 정한다. 제외 레포 표가 **하나라도** 있으면 다수결과 무관하게 삭제(제외 레포 내용이
/// 다른 레포 스트림으로 새지 않게), 아니면 다수결. 표가 없으면 `None`.
fn decide(votes: &[String], excluded: impl Fn(&str) -> bool) -> Option<Target> {
    if votes.iter().any(|v| excluded(v)) {
        return Some(Target::Delete);
    }
    majority(votes).map(Target::Move)
}

/// base 이벤트 하나가 속한 묶음 — 같은 묶음끼리만 함께 투표·이동한다.
#[derive(Clone, Copy, PartialEq)]
enum Owner {
    /// 앞선 prompt 가 아예 없다(prompt 가 전부 메타로 걸러진 세션 등) — 스스로 투표한다.
    NoPrompt,
    /// 지워진(제외 레포) 턴의 뒷부분 — 지운다.
    ExcludedTail,
    /// `starts[i]` 턴.
    Turn(usize),
}

/// hub base 스트림(`claude_code:<sid>`)의 이벤트를 턴마다 실제 레포 자식 스트림으로 옮긴다.
/// 반환값 = 이동/삭제된 이벤트 수(0 이면 변화 없음). 두 번 돌려도 결과가 같다(base 잔류분만 다시 본다).
///
/// base 이벤트마다 소속 턴 = base·자식 전체에서 (ts, external_id) 가 그 이벤트 이하인 최신 턴 시작
/// prompt. 그 prompt 가 base 에 있으면 일반 턴 투표, 자식에 있으면(진행 중 턴의 뒷조각) 이미 옮겨진
/// 앞부분까지 모아 다시 투표한다 — 조각으로 나눠 들어와도 한 번에 처리한 것과 같게. 신호 없는 옛 턴이
/// base 에 남아 있어도 뒤 턴의 조각이 거기 붙지 않는다.
pub fn reattribute_hub_stream(
    conn: &Connection,
    base_id: &str,
    exclude_projects: &[String],
    cache: &mut RepoCache,
) -> anyhow::Result<usize> {
    let Some(base) = load_base_stream(conn, base_id)? else {
        return Ok(0);
    };
    let events = load_events(conn, base_id)?;
    if events.is_empty() {
        return Ok(0);
    }
    let home = home_dir_string();
    let mut resolve = cached_resolver(cache);
    let excluded = |repo: &str| config::project_matches_exclude(repo, exclude_projects);
    let starts = load_turn_starts(conn, base_id)?;
    let excluded_turn_ts =
        db::get_cursor(conn, EXCLUDED_TURN_CURSOR_SOURCE, base_id)?.map(|(ts, _)| ts);
    let child_prefix = format!("{base_id}@");

    // 1) base 이벤트를 소속 묶음별로 나눈다(이벤트가 정렬돼 있어 같은 묶음은 연속).
    let mut groups: Vec<(Owner, Vec<EventRow>)> = Vec::new();
    for e in events {
        let idx = starts.partition_point(|s| s.key() <= (e.ts, e.external_id.as_str()));
        let turn = idx.checked_sub(1);
        // 제외 커서가 소속 턴 시작과 이 이벤트 사이에 있으면 지워진 턴의 뒷부분이다.
        let tail_of_excluded =
            excluded_turn_ts.is_some_and(|ex| ex <= e.ts && turn.is_none_or(|i| starts[i].ts < ex));
        let owner = match turn {
            _ if tail_of_excluded => Owner::ExcludedTail,
            Some(i) => Owner::Turn(i),
            None => Owner::NoPrompt,
        };
        match groups.last_mut() {
            Some((o, list)) if *o == owner => list.push(e),
            _ => groups.push((owner, vec![e])),
        }
    }

    // 2) 묶음마다 귀속을 정한다. 신호 없는 base 턴은 그대로 둔다.
    let mut plan: Vec<(Vec<EventRow>, Target)> = Vec::new();
    for (owner, group) in groups {
        let (turn, target) = match owner {
            Owner::ExcludedTail => (group, Some(Target::Delete)),
            Owner::NoPrompt => {
                let target = decide(
                    &votes_of(group.iter(), home.as_deref(), &mut resolve),
                    excluded,
                );
                (group, target)
            }
            Owner::Turn(i) => match starts[i].stream_id.strip_prefix(&child_prefix) {
                // 턴 시작 prompt 가 base 에 있다 = 턴 전체가 base 에 있다(턴은 통째로만 옮긴다).
                None => {
                    let target = decide(
                        &votes_of(group.iter(), home.as_deref(), &mut resolve),
                        excluded,
                    );
                    (group, target)
                }
                // 이미 자식으로 옮겨진 턴의 뒷조각 — 앞부분까지 모아 다시 투표, 표가 없으면 그 자식으로.
                Some(current_repo) => {
                    let to = starts.get(i + 1).map(TurnStart::key);
                    let mut turn = load_child_turn_events(conn, base_id, starts[i].key(), to)?;
                    turn.extend(group);
                    turn.sort_by(|a, b| (a.ts, &a.external_id).cmp(&(b.ts, &b.external_id)));
                    let target = decide(
                        &votes_of(turn.iter(), home.as_deref(), &mut resolve),
                        excluded,
                    )
                    .unwrap_or_else(|| Target::Move(current_repo.to_string()));
                    (turn, Some(target))
                }
            },
        };
        if let Some(target) = target {
            plan.push((turn, target));
        }
    }

    if plan.is_empty() {
        return Ok(0);
    }

    let tx = conn.unchecked_transaction()?;
    let mut touched: Vec<String> = Vec::new();
    // 앞부분이 이미 자식으로 가 있던 턴을 다시 옮기거나 지우면 원래 자식의 범위도 다시 잰다.
    let mut sources: Vec<&str> = Vec::new();
    let mut changed = 0usize;
    let mut excluded_prompt_ts: Option<i64> = None;
    for (turn, target) in &plan {
        match target {
            Target::Move(repo) => {
                let child_id = child_stream_id(base_id, repo);
                if !touched.contains(&child_id) {
                    // FK(events.stream_id → streams.id) 때문에 자식 스트림을 먼저 만든다.
                    let child = StreamInput {
                        id: child_id.clone(),
                        source: base.source.clone(),
                        kind: Some("session".to_string()),
                        title: base.title.clone(),
                        project: Some(repo.clone()),
                        git_branch: base.git_branch.clone(),
                        started_at: None,
                        ended_at: None,
                        status: None,
                        metadata: Some(serde_json::json!({
                            "hub": true,
                            "hubStream": base_id,
                            "entrypoint": base.entrypoint,
                        })),
                    };
                    let init_ts = turn.iter().map(|e| e.ts).min().unwrap_or_default();
                    db::upsert_stream(&tx, &child, init_ts)?;
                    touched.push(child_id.clone());
                }
                let mut stmt = tx.prepare_cached(
                    "UPDATE events SET stream_id = ?1 WHERE id = ?2 AND stream_id IS NOT ?1",
                )?;
                for e in turn.iter() {
                    let n = stmt.execute(params![child_id, e.id])?;
                    if n > 0 && !sources.contains(&e.stream_id.as_str()) {
                        sources.push(&e.stream_id);
                    }
                    changed += n;
                }
            }
            Target::Delete => {
                let mut stmt = tx.prepare_cached("DELETE FROM events WHERE id = ?1")?;
                for e in turn.iter() {
                    if !sources.contains(&e.stream_id.as_str()) {
                        sources.push(&e.stream_id);
                    }
                    changed += stmt.execute(params![e.id])?;
                    if e.event_type == "prompt" {
                        excluded_prompt_ts = excluded_prompt_ts.max(Some(e.ts));
                    }
                }
            }
        }
    }
    if let Some(ts) = excluded_prompt_ts {
        db::upsert_cursor(
            &tx,
            EXCLUDED_TURN_CURSOR_SOURCE,
            base_id,
            excluded_turn_ts.max(Some(ts)).unwrap_or(ts),
            None,
        )?;
    }
    db::recalc_stream_bounds(&tx, base_id)?;
    for id in touched.iter().map(String::as_str).chain(sources) {
        if id != base_id {
            db::recalc_stream_bounds(&tx, id)?;
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// hub 서브에이전트 스트림: 턴 분할 없이 스트림 전체 다수결로 `streams.project` 만 고친다(신호 없으면 그대로).
/// 제외 레포 표가 하나라도 있으면 그 스트림 이벤트를 지우고 `metadata.hubExcludedRepo` 로 표시해
/// 이후 들어오는 이벤트도 계속 지운다(제외가 풀리면 다시 투표).
pub fn reattribute_hub_agent_stream(
    conn: &Connection,
    stream_id: &str,
    exclude_projects: &[String],
    cache: &mut RepoCache,
) -> anyhow::Result<usize> {
    let excluded_repo: Option<String> = conn
        .query_row(
            "SELECT CASE WHEN json_valid(metadata) THEN json_extract(metadata, '$.hubExcludedRepo') END
             FROM streams WHERE id = ?1",
            params![stream_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let repo = match excluded_repo.filter(|r| config::project_matches_exclude(r, exclude_projects))
    {
        Some(repo) => repo,
        None => {
            let events = load_events(conn, stream_id)?;
            let home = home_dir_string();
            let mut resolve = cached_resolver(cache);
            let votes = votes_of(events.iter(), home.as_deref(), &mut resolve);
            // 제외 레포 표가 하나라도 있으면 그 레포로 정해 스트림 전체를 지운다(다수결 무관).
            let first_excluded = votes
                .iter()
                .find(|v| config::project_matches_exclude(v, exclude_projects))
                .cloned();
            let Some(repo) = first_excluded.or_else(|| majority(&votes)) else {
                return Ok(0);
            };
            repo
        }
    };

    if config::project_matches_exclude(&repo, exclude_projects) {
        let tx = conn.unchecked_transaction()?;
        let deleted = tx.execute(
            "DELETE FROM events WHERE stream_id = ?1",
            params![stream_id],
        )?;
        tx.execute(
            "UPDATE streams SET metadata = json_set(
               CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END, '$.hubExcludedRepo', ?2)
             WHERE id = ?1",
            params![stream_id, repo],
        )?;
        tx.commit()?;
        return Ok(deleted);
    }
    let updated = conn.execute(
        "UPDATE streams SET project = ?2 WHERE id = ?1 AND project IS NOT ?2",
        params![stream_id, repo],
    )?;
    Ok(updated)
}

/// 스트림이 hub(metadata.hub) 면 종류에 맞게 재귀속한다(agent → 전체 투표, 세션 → 턴 분할).
/// hub 가 아니거나 이미 자식 스트림(metadata.hubStream)이면 아무것도 안 한다.
pub fn reattribute_if_hub(
    conn: &Connection,
    stream_id: &str,
    exclude_projects: &[String],
    cache: &mut RepoCache,
) -> anyhow::Result<usize> {
    let flags: Option<(i64, i64)> = conn
        .query_row(
            "SELECT
               COALESCE(CASE WHEN json_valid(metadata) THEN json_extract(metadata, '$.hub') END, 0),
               CASE WHEN json_valid(metadata) AND json_extract(metadata, '$.hubStream') IS NOT NULL
                 THEN 1 ELSE 0 END
             FROM streams WHERE id = ?1",
            params![stream_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((hub, is_child)) = flags else {
        return Ok(0);
    };
    if hub == 0 || is_child == 1 {
        return Ok(0);
    }
    if stream_id.contains(":agent:") {
        reattribute_hub_agent_stream(conn, stream_id, exclude_projects, cache)
    } else {
        reattribute_hub_stream(conn, stream_id, exclude_projects, cache)
    }
}

/// 기존 DB 소급: 이 기능 이전에 쌓인 claude_code 스트림 중 project 가 (지금 존재하는) 레포 밖
/// 디렉토리인 것을 hub 로 표시하고 재귀속한다. 마커 행이 있으면 전체는 다시 돌지 않는다(한 번만).
/// 실패한 스트림은 `hub_reattribute_failed` 커서 행으로 남겨 다음 기동부터 그것만 재시도하고, 성공하면
/// 그 행을 지운다 — 한 스트림이 계속 실패해도 매 기동 전체를 다시 돌지 않게.
/// UI·인제스트를 막지 않도록 스트림 하나마다 뮤텍스를 잡고 놓는다.
pub fn backfill_hub_streams(db: &crate::Db, exclude_projects: &[String]) -> anyhow::Result<usize> {
    let (retry_only, candidates): (bool, Vec<(String, Option<String>)>) = {
        let conn = db.lock().expect("db mutex poisoned");
        if db::get_cursor(&conn, BACKFILL_CURSOR_SOURCE, BACKFILL_CURSOR_RESOURCE)?.is_some() {
            let mut stmt =
                conn.prepare("SELECT resource FROM capture_cursors WHERE source = ?1")?;
            let rows = stmt
                .query_map(params![BACKFILL_FAILED_CURSOR_SOURCE], |row| {
                    Ok((row.get::<_, String>(0)?, None))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            (true, rows)
        } else {
            // 이미 hub 로 표시된 base 도 다시 본다(재귀속은 멱등). 자식(hubStream)은 뺀다.
            let mut stmt = conn.prepare(
                "SELECT id, project FROM streams
                 WHERE source = ?1 AND project IS NOT NULL
                   AND (NOT json_valid(metadata) OR json_extract(metadata, '$.hubStream') IS NULL)",
            )?;
            let rows = stmt
                .query_map(params![crate::capture::normalize::SOURCE], |row| {
                    Ok((row.get::<_, String>(0)?, Some(row.get::<_, String>(1)?)))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            (false, rows)
        }
    };

    let mut cache = RepoCache::new();
    let mut changed = 0usize;
    let mut failed = 0usize;
    for (id, project) in candidates {
        let conn = db.lock().expect("db mutex poisoned");
        // 전체 실행: 지워진 디렉토리(옛 레포 흔적)는 hub 가 아니다 — normalize_lines 의 판정과 같다.
        // 재시도: 앞선 실행에서 이미 hub 로 표시됐으니 그대로 다시 돌린다.
        if let Some(project) = project {
            if !Path::new(&project).is_dir() || find_repo_root(&project).is_some() {
                continue;
            }
            conn.execute(
                "UPDATE streams SET metadata = json_patch(
                   CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END, '{\"hub\":true}')
                 WHERE id = ?1",
                params![id],
            )?;
        }
        match reattribute_if_hub(&conn, &id, exclude_projects, &mut cache) {
            Ok(n) => {
                changed += n;
                db::delete_cursor(&conn, BACKFILL_FAILED_CURSOR_SOURCE, &id)?;
            }
            Err(e) => {
                failed += 1;
                eprintln!("[logroom] hub backfill {id}: {e}");
                db::upsert_cursor(&conn, BACKFILL_FAILED_CURSOR_SOURCE, &id, 0, None)?;
            }
        }
    }

    if failed > 0 {
        eprintln!("[logroom] hub backfill: {failed}개 스트림 실패 — 다음 기동에 그것만 재시도");
    }
    if !retry_only {
        let conn = db.lock().expect("db mutex poisoned");
        db::upsert_cursor(
            &conn,
            BACKFILL_CURSOR_SOURCE,
            BACKFILL_CURSOR_RESOURCE,
            0,
            None,
        )?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::config::BodyPolicy;
    use crate::capture::normalize::{normalize_lines, FileContext};
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// `watch.rs`/`query.rs` 테스트와 같은 패턴 — `tempfile` 없이 임시 디렉토리를 만든다.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "logroom-hub-test-{}-{n}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// hub 디렉토리 + `.git` 레포 둘(repoA, repoB)을 만든 고정물.
    struct Fixture {
        tmp: TempDir,
        hub: String,
        repo_a: String,
        repo_b: String,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = TempDir::new();
            let hub = tmp.path.join("hub");
            let repo_a = tmp.path.join("repoA");
            let repo_b = tmp.path.join("repoB");
            fs::create_dir_all(&hub).unwrap();
            fs::create_dir_all(repo_a.join(".git")).unwrap();
            fs::create_dir_all(repo_b.join(".git")).unwrap();
            fs::create_dir_all(repo_a.join("src")).unwrap();
            let s = |p: PathBuf| p.to_string_lossy().to_string();
            Self {
                hub: s(hub),
                repo_a: s(repo_a),
                repo_b: s(repo_b),
                tmp,
            }
        }

        fn conn(&self) -> Connection {
            db::open_and_migrate_at(&self.tmp.path.join("logroom.db")).expect("마이그레이션 성공")
        }

        fn ts(sec: u32) -> String {
            format!("2026-10-01T00:00:{sec:02}Z")
        }

        fn prompt(&self, uuid: &str, sec: u32, text: &str) -> Value {
            json!({
                "type": "user", "uuid": uuid, "sessionId": "sess-hub", "cwd": self.hub,
                "entrypoint": "sdk-ts", "timestamp": Self::ts(sec),
                "message": { "content": text }
            })
        }

        fn bash(&self, uuid: &str, tool_id: &str, sec: u32, command: &str) -> Value {
            json!({
                "type": "assistant", "uuid": uuid, "sessionId": "sess-hub", "cwd": self.hub,
                "timestamp": Self::ts(sec),
                "message": { "content": [
                    { "type": "tool_use", "id": tool_id, "name": "Bash", "input": { "command": command } }
                ] }
            })
        }

        fn response(&self, uuid: &str, sec: u32, text: &str) -> Value {
            json!({
                "type": "assistant", "uuid": uuid, "sessionId": "sess-hub", "cwd": self.hub,
                "timestamp": Self::ts(sec),
                "message": { "content": [ { "type": "text", "text": text } ] }
            })
        }

        fn read(&self, uuid: &str, tool_id: &str, sec: u32, file: &str) -> Value {
            json!({
                "type": "assistant", "uuid": uuid, "sessionId": "sess-hub", "cwd": self.hub,
                "timestamp": Fixture::ts(sec),
                "message": { "content": [
                    { "type": "tool_use", "id": tool_id, "name": "Read", "input": { "file_path": file } }
                ] }
            })
        }

        /// watch.rs 처럼 정규화 → ingest → (hub 면) 재귀속.
        fn ingest(
            &self,
            conn: &Connection,
            lines: &[Value],
            exclude: &[String],
            cache: &mut RepoCache,
        ) {
            let req = normalize_lines(lines, &FileContext::default(), BodyPolicy::Essential)
                .expect("요청");
            let stream_id = req.stream.as_ref().unwrap().id.clone();
            db::ingest(conn, &req).unwrap();
            reattribute_if_hub(conn, &stream_id, exclude, cache).unwrap();
        }
    }

    const BASE: &str = "claude_code:sess-hub";

    fn stream_of(conn: &Connection, external_id: &str) -> String {
        conn.query_row(
            "SELECT stream_id FROM events WHERE external_id = ?1",
            params![external_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn count_in(conn: &Connection, stream_id: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM events WHERE stream_id = ?1",
            params![stream_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn resolve_none(_: &str) -> Option<String> {
        None
    }

    // ── 신호 추출 ──

    #[test]
    fn bash_cd_and_git_c_are_extracted_with_quotes_and_home() {
        let home = Some("/Users/me");
        let c = extract_dir_candidates(
            "Bash",
            Some("cd \"/Users/me/git/a b\" && ls; (cd ~/git/x && make) | cat; git -C $HOME/git/y status"),
            None,
            home,
        );
        assert_eq!(
            c,
            vec!["/Users/me/git/a b", "/Users/me/git/x", "/Users/me/git/y"]
        );

        let c = extract_dir_candidates("Bash", Some("cd '${HOME}/w/' && git -C ~ log"), None, home);
        assert_eq!(c, vec!["/Users/me/w", "/Users/me"]);

        let c = extract_dir_candidates("Bash", Some("cd /r/a/../b/./c/"), None, home);
        assert_eq!(c, vec!["/r/b/c"]);
    }

    #[test]
    fn bash_relative_dirs_and_non_command_cd_are_ignored() {
        let c = extract_dir_candidates(
            "Bash",
            Some("cd src && git -C ../x log; echo cd /tmp; cd -"),
            None,
            Some("/Users/me"),
        );
        assert!(c.is_empty(), "상대경로·인자 위치의 cd 는 버린다: {c:?}");
    }

    #[test]
    fn file_tools_use_parent_dir_of_absolute_path() {
        assert_eq!(
            extract_dir_candidates("Read", Some("/r/a/src/main.rs"), None, None),
            vec!["/r/a/src"]
        );
        assert_eq!(
            extract_dir_candidates(
                "NotebookEdit",
                None,
                Some(r#"{"input":{"notebook_path":"/r/b/n.ipynb"}}"#),
                None
            ),
            vec!["/r/b"]
        );
        assert!(extract_dir_candidates("Edit", Some("src/main.rs"), None, None).is_empty());
    }

    #[test]
    fn grep_and_glob_use_absolute_input_path_including_untruncated_preview() {
        assert_eq!(
            extract_dir_candidates(
                "Grep",
                Some("foo"),
                Some(r#"{"input":{"pattern":"foo","path":"/r/c"}}"#),
                None
            ),
            vec!["/r/c"]
        );
        // essential 정책의 inputPreview(잘리지 않은 JSON 문자열)도 읽는다.
        let meta =
            json!({ "inputPreview": json!({ "pattern": "*.rs", "path": "/r/d" }).to_string() })
                .to_string();
        assert_eq!(
            extract_dir_candidates("Glob", Some("*.rs"), Some(&meta), None),
            vec!["/r/d"]
        );
        assert!(extract_dir_candidates(
            "Grep",
            Some("foo"),
            Some(r#"{"input":{"path":"rel"}}"#),
            None
        )
        .is_empty());
    }

    #[test]
    fn event_repos_drops_non_repo_and_summarizer() {
        let mut resolve = |dir: &str| match dir {
            "/r/a/src" => Some("/r/a".to_string()),
            "/x/.logroom/summarizer" => Some("/x/.logroom/summarizer".to_string()),
            _ => None,
        };
        assert_eq!(
            event_repos("Read", Some("/r/a/src/x.rs"), None, None, &mut resolve),
            vec!["/r/a"]
        );
        assert!(event_repos(
            "Bash",
            Some("cd /x/.logroom/summarizer"),
            None,
            None,
            &mut resolve
        )
        .is_empty());
        assert!(event_repos("Bash", Some("cd /nowhere"), None, None, &mut resolve_none).is_empty());
    }

    #[test]
    fn majority_breaks_ties_by_first_seen() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(majority(&v(&["b", "a", "a", "b"])), Some("b".to_string()));
        assert_eq!(majority(&v(&["b", "a", "a"])), Some("a".to_string()));
        assert_eq!(majority(&[]), None);
    }

    // ── 재귀속 ──

    #[test]
    fn normalize_marks_hub_stream_with_entrypoint() {
        let fx = Fixture::new();
        let req = normalize_lines(
            &[fx.prompt("p1", 0, "hello")],
            &FileContext::default(),
            BodyPolicy::Essential,
        )
        .unwrap();
        let meta = req.stream.unwrap().metadata.unwrap();
        assert_eq!(meta["hub"], json!(true));
        assert_eq!(meta["entrypoint"], json!("sdk-ts"));
    }

    #[test]
    fn turns_move_to_repo_child_streams_and_signalless_turn_stays() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let mut cache = RepoCache::new();
        let lines = vec![
            fx.prompt("p1", 0, "턴1 레포A 작업"),
            fx.bash(
                "a1",
                "toolu_a",
                1,
                &format!("cd {} && git status", fx.repo_a),
            ),
            fx.response("r1", 2, "A 끝"),
            fx.prompt("p2", 3, "턴2 레포B 작업"),
            fx.bash("a2", "toolu_b", 4, &format!("git -C {}/ log", fx.repo_b)),
            fx.prompt("p3", 5, "턴3 신호 없음"),
            fx.response("r3", 6, "그냥 답"),
        ];
        fx.ingest(&conn, &lines, &[], &mut cache);

        let child_a = child_stream_id(BASE, &fx.repo_a);
        let child_b = child_stream_id(BASE, &fx.repo_b);
        assert_eq!(stream_of(&conn, "p1#0"), child_a);
        assert_eq!(stream_of(&conn, "toolu_a"), child_a);
        assert_eq!(stream_of(&conn, "r1#0"), child_a);
        assert_eq!(stream_of(&conn, "p2#0"), child_b);
        assert_eq!(stream_of(&conn, "toolu_b"), child_b);
        assert_eq!(stream_of(&conn, "p3#0"), BASE);
        assert_eq!(stream_of(&conn, "r3#0"), BASE);

        let (project, kind, meta, started): (String, String, String, i64) = conn
            .query_row(
                "SELECT project, kind, metadata, started_at FROM streams WHERE id = ?1",
                params![child_a],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(project, fx.repo_a);
        assert_eq!(kind, "session");
        let meta: Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(meta["hub"], json!(true));
        assert_eq!(meta["hubStream"], json!(BASE));
        assert_eq!(meta["entrypoint"], json!("sdk-ts"));
        let base_started: i64 = conn
            .query_row(
                "SELECT started_at FROM streams WHERE id = ?1",
                params![BASE],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            started < base_started,
            "base 범위는 남은 이벤트 기준으로 다시 계산된다"
        );

        // 멱등: 다시 돌려도 아무것도 안 바뀐다.
        assert_eq!(
            reattribute_hub_stream(&conn, BASE, &[], &mut cache).unwrap(),
            0
        );
        assert_eq!(count_in(&conn, &child_a), 3);
        assert_eq!(count_in(&conn, &child_b), 2);
        assert_eq!(count_in(&conn, BASE), 2);
    }

    #[test]
    fn in_progress_turn_moves_together_once_signal_arrives() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let mut cache = RepoCache::new();
        fx.ingest(&conn, &[fx.prompt("p1", 0, "진행 중 턴")], &[], &mut cache);
        assert_eq!(stream_of(&conn, "p1#0"), BASE, "신호 전에는 base 잔류");

        let file = format!("{}/src/lib.rs", fx.repo_a);
        let read = json!({
            "type": "assistant", "uuid": "a1", "sessionId": "sess-hub", "cwd": fx.hub,
            "timestamp": Fixture::ts(1),
            "message": { "content": [
                { "type": "tool_use", "id": "toolu_r", "name": "Read", "input": { "file_path": file } }
            ] }
        });
        fx.ingest(&conn, &[read], &[], &mut cache);
        let child_a = child_stream_id(BASE, &fx.repo_a);
        assert_eq!(stream_of(&conn, "p1#0"), child_a);
        assert_eq!(stream_of(&conn, "toolu_r"), child_a);

        // 이어 온 응답(첫 prompt 앞 이벤트)은 투표 없이 앞 턴의 자식으로 간다.
        fx.ingest(&conn, &[fx.response("r1", 2, "다 읽었다")], &[], &mut cache);
        assert_eq!(stream_of(&conn, "r1#0"), child_a);
        assert_eq!(count_in(&conn, BASE), 0);
    }

    #[test]
    fn excluded_repo_turn_is_deleted_including_its_tail() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let mut cache = RepoCache::new();
        let exclude = vec![fx.repo_b.clone()];
        let lines = vec![
            fx.prompt("p1", 0, "턴1 A"),
            fx.bash("a1", "toolu_a", 1, &format!("cd {}", fx.repo_a)),
            fx.prompt("p2", 2, "턴2 비밀 B"),
            fx.bash("a2", "toolu_b", 3, &format!("cd {}", fx.repo_b)),
        ];
        fx.ingest(&conn, &lines, &exclude, &mut cache);
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE external_id IN ('p2#0','toolu_b')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "제외 레포 턴은 지운다");
        assert_eq!(count_in(&conn, &child_stream_id(BASE, &fx.repo_b)), 0);
        assert_eq!(stream_of(&conn, "p1#0"), child_stream_id(BASE, &fx.repo_a));

        // 지운 턴의 뒷부분(신호 없는 응답)도 앞 턴(A)으로 새지 않고 지워진다.
        fx.ingest(
            &conn,
            &[fx.response("r2", 4, "비밀 답")],
            &exclude,
            &mut cache,
        );
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE external_id = 'r2#0'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    fn exists(conn: &Connection, external_id: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM events WHERE external_id = ?1",
            params![external_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    }

    #[test]
    fn excluded_votes_in_later_chunk_delete_whole_turn_already_moved() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let mut cache = RepoCache::new();
        let exclude = vec![fx.repo_b.clone()];
        let child_a = child_stream_id(BASE, &fx.repo_a);

        fx.ingest(
            &conn,
            &[
                fx.prompt("p1", 0, "턴"),
                fx.bash("a1", "toolu_a", 1, &format!("cd {}", fx.repo_a)),
            ],
            &exclude,
            &mut cache,
        );
        assert_eq!(stream_of(&conn, "p1#0"), child_a);

        let secret = format!("{}/s.rs", fx.repo_b);
        fx.ingest(
            &conn,
            &[
                fx.read("b1", "toolu_b1", 2, &secret),
                fx.read("b2", "toolu_b2", 3, &secret),
                fx.read("b3", "toolu_b3", 4, &secret),
            ],
            &exclude,
            &mut cache,
        );
        fx.ingest(&conn, &[fx.response("r1", 5, "답")], &exclude, &mut cache);

        for ext in [
            "p1#0", "toolu_a", "toolu_b1", "toolu_b2", "toolu_b3", "r1#0",
        ] {
            assert!(!exists(&conn, ext), "{ext} 는 지워져야 한다");
        }
        assert_eq!(count_in(&conn, &child_a), 0);
        assert_eq!(count_in(&conn, BASE), 0);
    }

    #[test]
    fn single_excluded_vote_deletes_turn_despite_majority() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let mut cache = RepoCache::new();
        let exclude = vec![fx.repo_b.clone()];
        let lines = vec![
            fx.prompt("p1", 0, "턴"),
            fx.bash("a1", "toolu_a1", 1, &format!("cd {}", fx.repo_a)),
            fx.bash("a2", "toolu_a2", 2, &format!("cd {}", fx.repo_a)),
            fx.bash("a3", "toolu_b", 3, &format!("cd {}", fx.repo_b)),
        ];
        fx.ingest(&conn, &lines, &exclude, &mut cache);
        for ext in ["p1#0", "toolu_a1", "toolu_a2", "toolu_b"] {
            assert!(!exists(&conn, ext), "{ext} 는 지워져야 한다");
        }
        assert_eq!(count_in(&conn, &child_stream_id(BASE, &fx.repo_a)), 0);
    }

    #[test]
    fn chunked_turn_is_revoted_and_matches_one_shot_result() {
        let fx = Fixture::new();
        // repo_a = 위키(첫 조각에서 잠깐 들른 곳), repo_b = 실제 작업 레포.
        let chunk1 = vec![
            fx.prompt("p1", 0, "턴"),
            fx.bash("a1", "toolu_w", 1, &format!("cd {}", fx.repo_a)),
        ];
        let chunk2 = vec![
            fx.bash("b1", "toolu_b1", 2, &format!("cd {}", fx.repo_b)),
            fx.bash("b2", "toolu_b2", 3, &format!("cd {}", fx.repo_b)),
            fx.bash("b3", "toolu_b3", 4, &format!("cd {}", fx.repo_b)),
        ];
        let snapshot = |conn: &Connection| -> Vec<(String, String)> {
            conn.prepare("SELECT external_id, stream_id FROM events ORDER BY external_id")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let bounds = |conn: &Connection, id: &str| -> (i64, i64) {
            conn.query_row(
                "SELECT started_at, ended_at FROM streams WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };

        let live = fx.conn();
        let mut cache = RepoCache::new();
        fx.ingest(&live, &chunk1, &[], &mut cache);
        fx.ingest(&live, &chunk2, &[], &mut cache);

        let other = Fixture::new();
        // 같은 경로가 되도록 같은 Fixture 의 레포를 쓰되 DB 만 따로 연다.
        let one_shot =
            db::open_and_migrate_at(&other.tmp.path.join("one.db")).expect("마이그레이션 성공");
        let all: Vec<Value> = chunk1.iter().chain(chunk2.iter()).cloned().collect();
        fx.ingest(&one_shot, &all, &[], &mut RepoCache::new());

        let child_b = child_stream_id(BASE, &fx.repo_b);
        assert_eq!(stream_of(&live, "p1#0"), child_b);
        assert_eq!(stream_of(&live, "toolu_w"), child_b);
        assert_eq!(snapshot(&live), snapshot(&one_shot));
        assert_eq!(bounds(&live, &child_b), bounds(&one_shot, &child_b));
        assert_eq!(count_in(&live, &child_stream_id(BASE, &fx.repo_a)), 0);
    }

    #[test]
    fn backfill_failure_is_recorded_and_only_failed_streams_are_retried() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let lines = vec![
            fx.prompt("p1", 0, "옛 hub 세션"),
            fx.bash("a1", "toolu_a", 1, &format!("cd {}", fx.repo_a)),
        ];
        let req = normalize_lines(&lines, &FileContext::default(), BodyPolicy::Essential).unwrap();
        db::ingest(&conn, &req).unwrap();
        conn.execute(
            "UPDATE streams SET metadata = '{}' WHERE id = ?1",
            params![BASE],
        )
        .unwrap();
        // 이벤트 이동을 실패시키는 임시 트리거.
        conn.execute_batch(
            "CREATE TEMP TRIGGER fail_move BEFORE UPDATE OF stream_id ON events
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let failed_row = |conn: &Connection| {
            db::get_cursor(conn, BACKFILL_FAILED_CURSOR_SOURCE, BASE)
                .unwrap()
                .is_some()
        };

        assert_eq!(backfill_hub_streams(&db, &[]).unwrap(), 0);
        {
            let conn = db.lock().unwrap();
            assert!(
                db::get_cursor(&conn, BACKFILL_CURSOR_SOURCE, BACKFILL_CURSOR_RESOURCE)
                    .unwrap()
                    .is_some(),
                "실패가 있어도 전체 마커는 찍는다"
            );
            assert!(failed_row(&conn), "실패 스트림은 따로 남긴다");
            assert_eq!(stream_of(&conn, "p1#0"), BASE);
        }
        // 계속 실패하면 실패 행이 그대로 남는다.
        assert_eq!(backfill_hub_streams(&db, &[]).unwrap(), 0);
        assert!(failed_row(&db.lock().unwrap()));

        db.lock()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_move;")
            .unwrap();
        // 다음 기동: 실패 목록만 재시도하고, 성공하면 그 행을 지운다.
        assert_eq!(backfill_hub_streams(&db, &[]).unwrap(), 2);
        let conn = db.lock().unwrap();
        assert_eq!(stream_of(&conn, "p1#0"), child_stream_id(BASE, &fx.repo_a));
        assert!(!failed_row(&conn));
    }

    fn cursor_ts(conn: &Connection) -> Option<i64> {
        db::get_cursor(conn, EXCLUDED_TURN_CURSOR_SOURCE, BASE)
            .unwrap()
            .map(|(ts, _)| ts)
    }

    fn ts_of(conn: &Connection, external_id: &str) -> i64 {
        conn.query_row(
            "SELECT ts FROM events WHERE external_id = ?1",
            params![external_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// 청크1 [p1, r1] 신호 없음(base 잔류) → 청크2 [p2, toolA] 자식 A.
    fn ingest_signalless_then_repo_a_turn(
        fx: &Fixture,
        conn: &Connection,
        exclude: &[String],
        cache: &mut RepoCache,
    ) {
        fx.ingest(
            conn,
            &[fx.prompt("p1", 0, "옛 턴"), fx.response("r1", 1, "답")],
            exclude,
            cache,
        );
        fx.ingest(
            conn,
            &[
                fx.prompt("p2", 2, "새 턴"),
                fx.bash("a2", "toolu_a", 3, &format!("cd {}", fx.repo_a)),
            ],
            exclude,
            cache,
        );
        assert_eq!(stream_of(conn, "p1#0"), BASE);
        assert_eq!(stream_of(conn, "p2#0"), child_stream_id(BASE, &fx.repo_a));
    }

    #[test]
    fn excluded_chunk_after_signalless_base_turn_deletes_only_in_progress_turn() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let mut cache = RepoCache::new();
        let exclude = vec![fx.repo_b.clone()];
        ingest_signalless_then_repo_a_turn(&fx, &conn, &exclude, &mut cache);
        let p2_ts = ts_of(&conn, "p2#0");

        fx.ingest(
            &conn,
            &[fx.bash("b1", "toolu_b", 4, &format!("cd {}", fx.repo_b))],
            &exclude,
            &mut cache,
        );
        assert_eq!(stream_of(&conn, "p1#0"), BASE, "비제외 옛 턴은 base 잔류");
        assert_eq!(stream_of(&conn, "r1#0"), BASE);
        for ext in ["p2#0", "toolu_a", "toolu_b"] {
            assert!(!exists(&conn, ext), "{ext} 는 지워져야 한다");
        }
        assert_eq!(count_in(&conn, &child_stream_id(BASE, &fx.repo_a)), 0);
        assert_eq!(cursor_ts(&conn), Some(p2_ts), "커서 = 지운 턴의 prompt ts");
    }

    #[test]
    fn tail_chunk_after_signalless_base_turn_joins_in_progress_child_turn() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let mut cache = RepoCache::new();
        ingest_signalless_then_repo_a_turn(&fx, &conn, &[], &mut cache);

        fx.ingest(&conn, &[fx.response("r2", 4, "A 답")], &[], &mut cache);
        assert_eq!(stream_of(&conn, "r2#0"), child_stream_id(BASE, &fx.repo_a));
        assert_eq!(stream_of(&conn, "p1#0"), BASE);
        assert_eq!(stream_of(&conn, "r1#0"), BASE);
        assert_eq!(count_in(&conn, BASE), 2);
    }

    #[test]
    fn consecutive_chunks_after_signalless_base_turn_follow_their_own_turns() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let mut cache = RepoCache::new();
        let child_a = child_stream_id(BASE, &fx.repo_a);
        let child_b = child_stream_id(BASE, &fx.repo_b);
        ingest_signalless_then_repo_a_turn(&fx, &conn, &[], &mut cache);

        // 진행 중 A 턴의 꼬리 + 새 B 턴이 한 청크로 온다.
        fx.ingest(
            &conn,
            &[
                fx.response("r2", 4, "A 답"),
                fx.prompt("p3", 5, "B 턴"),
                fx.bash("b3", "toolu_b", 6, &format!("cd {}", fx.repo_b)),
            ],
            &[],
            &mut cache,
        );
        // B 턴 꼬리 + 신호 없는 새 턴.
        fx.ingest(
            &conn,
            &[
                fx.response("r3", 7, "B 답"),
                fx.prompt("p4", 8, "잡담"),
                fx.response("r4", 9, "잡담 답"),
            ],
            &[],
            &mut cache,
        );

        for (ext, want) in [
            ("p1#0", BASE),
            ("r1#0", BASE),
            ("p2#0", child_a.as_str()),
            ("toolu_a", child_a.as_str()),
            ("r2#0", child_a.as_str()),
            ("p3#0", child_b.as_str()),
            ("toolu_b", child_b.as_str()),
            ("r3#0", child_b.as_str()),
            ("p4#0", BASE),
            ("r4#0", BASE),
        ] {
            assert_eq!(stream_of(&conn, ext), want, "{ext}");
        }
        assert_eq!(
            reattribute_hub_stream(&conn, BASE, &[], &mut cache).unwrap(),
            0,
            "멱등"
        );
    }

    #[test]
    fn agent_stream_gets_whole_stream_majority_project() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let mut cache = RepoCache::new();
        let ctx = FileContext {
            agent_id: Some("ag1".to_string()),
        };
        let lines = vec![
            fx.prompt("ap", 0, "서브에이전트"),
            fx.bash("x1", "toolu_x1", 1, &format!("cd {}", fx.repo_b)),
            fx.bash("x2", "toolu_x2", 2, &format!("cd {}", fx.repo_a)),
            fx.bash("x3", "toolu_x3", 3, &format!("cd {}", fx.repo_b)),
        ];
        let req = normalize_lines(&lines, &ctx, BodyPolicy::Essential).unwrap();
        let id = req.stream.as_ref().unwrap().id.clone();
        let meta = req.stream.as_ref().unwrap().metadata.clone().unwrap();
        assert_eq!(meta["parentStream"], json!(BASE));
        assert_eq!(meta["hub"], json!(true));
        db::ingest(&conn, &req).unwrap();
        reattribute_if_hub(&conn, &id, &[], &mut cache).unwrap();

        let project: String = conn
            .query_row(
                "SELECT project FROM streams WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(project, fx.repo_b);
        assert_eq!(count_in(&conn, &id), 4, "agent 는 이벤트를 옮기지 않는다");

        // 제외 레포면 그 스트림 이벤트를 지운다.
        reattribute_if_hub(&conn, &id, std::slice::from_ref(&fx.repo_b), &mut cache).unwrap();
        assert_eq!(count_in(&conn, &id), 0);
    }

    #[test]
    fn moved_prompt_is_still_found_by_fts() {
        let fx = Fixture::new();
        let conn = fx.conn();
        let mut cache = RepoCache::new();
        let lines = vec![
            fx.prompt("p1", 0, "FTSHUBMARKER 레포 이동 검색"),
            fx.bash("a1", "toolu_a", 1, &format!("cd {}", fx.repo_a)),
        ];
        fx.ingest(&conn, &lines, &[], &mut cache);
        let found: Vec<String> = conn
            .prepare(
                "SELECT e.stream_id FROM events_fts f JOIN events e ON e.rowid = f.rowid
                 WHERE events_fts MATCH ?1",
            )
            .unwrap()
            .query_map(params!["FTSHUBMARKER"], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(found, vec![child_stream_id(BASE, &fx.repo_a)]);
    }

    #[test]
    fn backfill_marks_old_hub_streams_once() {
        let fx = Fixture::new();
        let conn = fx.conn();
        // 이 기능 이전처럼 hub 표시 없이 쌓인 세션을 흉내 낸다.
        let lines = vec![
            fx.prompt("p1", 0, "옛 hub 세션"),
            fx.bash("a1", "toolu_a", 1, &format!("cd {}", fx.repo_a)),
        ];
        let req = normalize_lines(&lines, &FileContext::default(), BodyPolicy::Essential).unwrap();
        db::ingest(&conn, &req).unwrap();
        conn.execute(
            "UPDATE streams SET metadata = '{}' WHERE id = ?1",
            params![BASE],
        )
        .unwrap();
        let db: crate::Db = Arc::new(Mutex::new(conn));

        assert_eq!(backfill_hub_streams(&db, &[]).unwrap(), 2);
        {
            let conn = db.lock().unwrap();
            assert_eq!(stream_of(&conn, "p1#0"), child_stream_id(BASE, &fx.repo_a));
            // 마커 확인용으로 다시 hub 표시를 지우고 base 로 되돌려도
            conn.execute(
                "UPDATE streams SET metadata = '{}' WHERE id = ?1",
                params![BASE],
            )
            .unwrap();
            conn.execute("UPDATE events SET stream_id = ?1", params![BASE])
                .unwrap();
        }
        // 두 번째 실행은 마커 때문에 아무것도 안 한다.
        assert_eq!(backfill_hub_streams(&db, &[]).unwrap(), 0);
        let conn = db.lock().unwrap();
        assert_eq!(stream_of(&conn, "p1#0"), BASE);
    }
}
