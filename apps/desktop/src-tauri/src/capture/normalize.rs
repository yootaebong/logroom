//! Claude Code JSONL 라인 → LogRoom Event/Stream 정규화 (docs/03-capture.md 소스1).
//! 순수함수: 파싱된 `serde_json::Value` 라인들만 입력으로 받고 부수효과(파일I/O 등) 없음.
//! (예외: [`normalize_project`]는 레포 루트 탐색을 위해 파일시스템을 확인한다 — watch.rs 전용.)

use crate::capture::config::BodyPolicy;
use crate::capture::policy::{
    apply_tool_result_policy, derive_stream_title, is_meta_prompt_text, tool_use_metadata,
};
use crate::model::{EventInput, IngestRequest, StreamInput};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const SOURCE: &str = "claude_code";

/// 파일 하나(메인 세션 또는 서브에이전트)의 컨텍스트. 파일 경로에서 watch.rs가 채운다.
#[derive(Debug, Clone, Default)]
pub struct FileContext {
    /// 서브에이전트 파일(`subagents/agent-<id>.jsonl`)이면 그 `<id>`.
    pub agent_id: Option<String>,
}

fn stream_id_for(session_id: &str, ctx: &FileContext) -> String {
    match &ctx.agent_id {
        Some(agent_id) => format!("{SOURCE}:{session_id}:agent:{agent_id}"),
        None => format!("{SOURCE}:{session_id}"),
    }
}

fn truncate60(text: &str) -> String {
    text.chars().take(60).collect()
}

/// ISO8601 `timestamp` → epoch ms. 파싱 실패 시 0(순수함수 유지 위해 시스템 시각 사용 안 함).
fn parse_ts(line: &Value) -> i64 {
    line.get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

/// tool_result 등의 `content`(문자열 또는 블록 배열)를 body 문자열로 변환.
fn stringify_content(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => s.clone(),
                _ => item
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| item.to_string()),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// tool_use `input`에서 command/file_path/pattern 중 있는 첫 값을 body로 사용.
fn primary_arg_body(input: &Value) -> Option<String> {
    for key in ["command", "file_path", "pattern"] {
        if let Some(v) = input.get(key) {
            return Some(match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        }
    }
    None
}

fn prompt_event(
    uuid: &str,
    idx: usize,
    stream_id: &str,
    ts: i64,
    parent_id: Option<String>,
    text: &str,
) -> EventInput {
    EventInput {
        external_id: format!("{uuid}#{idx}"),
        stream_id: stream_id.to_string(),
        ts,
        source: SOURCE.to_string(),
        event_type: "prompt".to_string(),
        title: Some(truncate60(text)),
        body: Some(text.to_string()),
        model: None,
        tokens_in: None,
        tokens_out: None,
        url: None,
        parent_id,
        metadata: None,
    }
}

/// `user` 라인 → prompt/tool_result 이벤트들. `message.content`가 문자열이면 블록 1개(`#0`)로 취급.
/// **시스템 노이즈 필터(신규 캡처, 소급 정리는 `V3__prune_meta_prompts.sql`)**:
/// - `isMeta == true`(원본 공식 마커)면 라인 전체 skip.
/// - text 블록은 [`is_meta_prompt_text`]로 슬래시 커맨드 메타/중단 마커 프리픽스면 skip.
fn normalize_user_line(line: &Value, stream_id: &str, body_policy: BodyPolicy) -> Vec<EventInput> {
    let mut events = Vec::new();
    if line.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return events;
    }

    let uuid = line.get("uuid").and_then(Value::as_str).unwrap_or_default();
    let parent_id = line
        .get("parentUuid")
        .and_then(Value::as_str)
        .map(str::to_string);
    let ts = parse_ts(line);

    let content = line.pointer("/message/content");

    match content {
        Some(Value::String(text)) => {
            if !is_meta_prompt_text(text) {
                events.push(prompt_event(uuid, 0, stream_id, ts, parent_id, text));
            }
        }
        Some(Value::Array(blocks)) => {
            for (idx, block) in blocks.iter().enumerate() {
                let btype = block.get("type").and_then(Value::as_str).unwrap_or("");
                match btype {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            if !is_meta_prompt_text(text) {
                                events.push(prompt_event(
                                    uuid,
                                    idx,
                                    stream_id,
                                    ts,
                                    parent_id.clone(),
                                    text,
                                ));
                            }
                        }
                    }
                    "tool_result" => {
                        if let Some(tool_use_id) = block.get("tool_use_id").and_then(Value::as_str) {
                            let body = block
                                .get("content")
                                .map(stringify_content)
                                .map(|b| apply_tool_result_policy(&b, body_policy));
                            events.push(EventInput {
                                // tool_use 이벤트가 같은 id를 external_id로 쓰므로
                                // (source, external_id) UNIQUE 충돌 방지를 위해 suffix 필수.
                                external_id: format!("{tool_use_id}#result"),
                                stream_id: stream_id.to_string(),
                                ts,
                                source: SOURCE.to_string(),
                                event_type: "tool_result".to_string(),
                                title: None,
                                body,
                                model: None,
                                tokens_in: None,
                                tokens_out: None,
                                url: None,
                                parent_id: parent_id.clone(),
                                metadata: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    events
}

/// `assistant` 라인 → response/tool_use 이벤트들. `thinking` 블록은 스킵.
fn normalize_assistant_line(line: &Value, stream_id: &str, body_policy: BodyPolicy) -> Vec<EventInput> {
    let uuid = line.get("uuid").and_then(Value::as_str).unwrap_or_default();
    let parent_id = line
        .get("parentUuid")
        .and_then(Value::as_str)
        .map(str::to_string);
    let ts = parse_ts(line);
    let model = line
        .pointer("/message/model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let usage = line.pointer("/message/usage");
    let tokens_in = usage.and_then(|u| u.get("input_tokens")).and_then(Value::as_i64);
    let tokens_out = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(Value::as_i64);
    let cache_meta: Option<Value> = usage.and_then(|u| {
        let mut m = serde_json::Map::new();
        if let Some(v) = u.get("cache_creation_input_tokens") {
            m.insert("cache_creation_input_tokens".to_string(), v.clone());
        }
        if let Some(v) = u.get("cache_read_input_tokens") {
            m.insert("cache_read_input_tokens".to_string(), v.clone());
        }
        if m.is_empty() {
            None
        } else {
            Some(Value::Object(m))
        }
    });

    let mut events = Vec::new();
    let Some(blocks) = line.pointer("/message/content").and_then(Value::as_array) else {
        return events;
    };

    // usage(tokens_in/out, cache_meta)는 토큰 이중계산을 막기 위해 라인당 첫 text 블록에만 부여한다.
    let mut usage_assigned = false;

    for (idx, block) in blocks.iter().enumerate() {
        let btype = block.get("type").and_then(Value::as_str).unwrap_or("");
        match btype {
            "thinking" => continue, // 기본 저장 안 함(검색 노이즈·프라이버시)
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    let (block_tokens_in, block_tokens_out, block_cache_meta) = if usage_assigned {
                        (None, None, None)
                    } else {
                        usage_assigned = true;
                        (tokens_in, tokens_out, cache_meta.clone())
                    };
                    events.push(EventInput {
                        external_id: format!("{uuid}#{idx}"),
                        stream_id: stream_id.to_string(),
                        ts,
                        source: SOURCE.to_string(),
                        event_type: "response".to_string(),
                        title: Some(truncate60(text)),
                        body: Some(text.to_string()),
                        model: model.clone(),
                        tokens_in: block_tokens_in,
                        tokens_out: block_tokens_out,
                        url: None,
                        parent_id: parent_id.clone(),
                        metadata: block_cache_meta,
                    });
                }
            }
            "tool_use" => {
                let Some(id) = block.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
                let input = block.get("input").cloned();
                let body = input.as_ref().and_then(primary_arg_body);
                let metadata = input.map(|inp| tool_use_metadata(inp, body_policy));
                events.push(EventInput {
                    external_id: id.to_string(),
                    stream_id: stream_id.to_string(),
                    ts,
                    source: SOURCE.to_string(),
                    event_type: "tool_use".to_string(),
                    title: Some(name.to_string()),
                    body,
                    model: model.clone(),
                    tokens_in: None,
                    tokens_out: None,
                    url: None,
                    parent_id: parent_id.clone(),
                    metadata,
                });
            }
            _ => {}
        }
    }

    events
}

/// 한 파일에서 새로 읽은 라인들(파싱된 JSON) → 하나의 `IngestRequest`.
/// events/title 둘 다 없으면(skip 대상 라인만 있던 경우) `None`.
/// `body_policy`는 tool_result body·tool_use metadata 절단 여부를 결정한다(ADR-0012, config.rs 참고).
pub fn normalize_lines(lines: &[Value], ctx: &FileContext, body_policy: BodyPolicy) -> Option<IngestRequest> {
    let mut events = Vec::new();
    let mut session_id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut ai_title: Option<String> = None;
    let mut entrypoint: Option<String> = None;

    for line in lines {
        if let Some(ep) = line.get("entrypoint").and_then(Value::as_str) {
            entrypoint = Some(ep.to_string());
        }
        if let Some(sid) = line.get("sessionId").and_then(Value::as_str) {
            session_id = Some(sid.to_string());
        }
        if let Some(c) = line.get("cwd").and_then(Value::as_str) {
            cwd = Some(c.to_string());
        }
        if let Some(gb) = line.get("gitBranch").and_then(Value::as_str) {
            git_branch = Some(gb.to_string());
        }

        let Some(sid) = session_id.clone() else { continue };
        let stream_id = stream_id_for(&sid, ctx);
        let line_type = line.get("type").and_then(Value::as_str).unwrap_or("");

        match line_type {
            "ai-title" => {
                if let Some(t) = line.get("aiTitle").and_then(Value::as_str) {
                    ai_title = Some(t.to_string());
                }
            }
            "user" => {
                events.extend(normalize_user_line(line, &stream_id, body_policy));
            }
            "assistant" => {
                events.extend(normalize_assistant_line(line, &stream_id, body_policy));
            }
            // last-prompt / queue-operation / attachment / file-history-snapshot / mode / summary / …
            _ => {}
        }
    }

    // ai-title이 없으면 prompt 본문들(ts 순서)에서 휴리스틱으로 제목을 산출한다(capture/policy.rs).
    let title = ai_title.or_else(|| {
        let prompt_bodies = events
            .iter()
            .filter(|e| e.event_type == "prompt")
            .filter_map(|e| e.body.as_deref());
        derive_stream_title(prompt_bodies)
    });
    if events.is_empty() && title.is_none() {
        return None;
    }

    let session_id = session_id?;
    let stream_id = stream_id_for(&session_id, ctx);
    // hub 세션: cwd가 git 레포 밖(예 `~/.agent-hub`)인 세션. 중간 에이전트가 띄운 세션이 시작 폴더 하나로
    // 뭉치므로, 턴마다 실제 만진 레포로 나누도록 표시만 해 둔다(실제 분할은 capture/hub.rs).
    // 디렉토리가 아예 없으면(지워진 레포 등) hub 로 보지 않는다 — "레포 밖에서 시작"이 아니라 흔적일 뿐이다.
    let repo_root = cwd.as_deref().and_then(find_repo_root);
    let is_hub = repo_root.is_none() && cwd.as_deref().is_some_and(|c| Path::new(c).is_dir());
    let project = repo_root.or_else(|| cwd.clone());

    // 일일 AI 요약 CLI 백엔드(M7-①, ADR-0016)가 `~/.logroom/summarizer`에서 `claude -p`를
    // 실행하면서 남기는 자기 자신의 세션은 캡처에서 통째로 제외한다(자기 캡처 루프 차단,
    // capture/policy.rs::is_summarizer_project 참고).
    if project.as_deref().is_some_and(crate::capture::policy::is_summarizer_project) {
        return None;
    }

    let mut metadata_map = serde_json::Map::new();
    if ctx.agent_id.is_some() {
        metadata_map.insert(
            "parentStream".to_string(),
            Value::String(format!("{SOURCE}:{session_id}")),
        );
    }
    if is_hub {
        metadata_map.insert("hub".to_string(), Value::Bool(true));
        if let Some(ep) = entrypoint {
            metadata_map.insert("entrypoint".to_string(), Value::String(ep));
        }
    }
    let metadata = (!metadata_map.is_empty()).then_some(Value::Object(metadata_map));

    let stream = StreamInput {
        id: stream_id,
        source: SOURCE.to_string(),
        kind: Some(if ctx.agent_id.is_some() { "agent" } else { "session" }.to_string()),
        title,
        project,
        git_branch,
        started_at: None,
        ended_at: None,
        status: None,
        metadata,
    };

    Some(IngestRequest {
        stream: Some(stream),
        events,
    })
}

/// worktree `.git` 파일(`gitdir: <path>` 한 줄)에서 `/.git/worktrees/` 세그먼트 앞부분(본 레포 루트)을
/// 추출한다. 서브모듈(`gitdir: .../.git/modules/<name>`)은 이 패턴이 없으므로 `None`(호출부가 폴백).
fn resolve_worktree_root(git_file_content: &str) -> Option<String> {
    const MARKER: &str = "/.git/worktrees/";
    let gitdir = git_file_content.lines().next()?.trim().strip_prefix("gitdir:")?.trim();
    let idx = gitdir.find(MARKER)?;
    Some(gitdir[..idx].to_string())
}

/// cwd에서 상위로 `.git`이 있는 디렉토리를 찾아 레포 루트로 정규화(없으면 cwd 그대로).
/// `.git`이 파일(git worktree 마커)이면 내용을 읽어 본 레포 루트로 한 번 더 정규화한다(파싱 실패/
/// 서브모듈 등 패턴 불일치 시 그 위치를 그대로 반환 — 기존 폴백과 동일).
/// 파일시스템을 확인하므로 순수함수가 아니다 — watch.rs에서만 호출.
pub fn normalize_project(cwd: &str) -> String {
    find_repo_root(cwd).unwrap_or_else(|| cwd.to_string())
}

/// [`normalize_project`]와 같은 탐색(상위로 `.git` 찾기 + worktree 본 레포 정규화)이되, `.git`을
/// 끝내 못 찾으면 `None`. hub 세션 판정(cwd가 레포 밖인 세션)과 hub 레포 신호 해석(capture/hub.rs)이 쓴다.
pub fn find_repo_root(dir: &str) -> Option<String> {
    let mut dir: PathBuf = Path::new(dir).to_path_buf();
    loop {
        let git_path = dir.join(".git");
        if git_path.is_dir() {
            return Some(dir.to_string_lossy().to_string());
        }
        if git_path.is_file() {
            if let Some(root) = std::fs::read_to_string(&git_path)
                .ok()
                .and_then(|content| resolve_worktree_root(&content))
            {
                return Some(root);
            }
            return Some(dir.to_string_lossy().to_string());
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> FileContext {
        FileContext::default()
    }

    /// 대부분의 테스트는 실제 기본값(essential)으로 검증한다.
    fn essential() -> BodyPolicy {
        BodyPolicy::Essential
    }

    #[test]
    fn user_text_becomes_prompt() {
        let line = json!({
            "type": "user",
            "uuid": "line-1",
            "parentUuid": null,
            "sessionId": "sess-1",
            "cwd": "/tmp/example",
            "gitBranch": "main",
            "timestamp": "2026-07-01T00:00:00Z",
            "isSidechain": false,
            "message": { "content": [ { "type": "text", "text": "안녕하세요 도와주세요" } ] }
        });

        let req = normalize_lines(&[line], &ctx(), essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 1);
        let e = &req.events[0];
        assert_eq!(e.event_type, "prompt");
        assert_eq!(e.external_id, "line-1#0");
        assert_eq!(e.stream_id, "claude_code:sess-1");
        assert_eq!(e.body.as_deref(), Some("안녕하세요 도와주세요"));

        let stream = req.stream.expect("stream 있어야 함");
        assert_eq!(stream.id, "claude_code:sess-1");
        assert_eq!(stream.kind.as_deref(), Some("session"));
        // ai-title 없으므로 첫 prompt 앞부분이 title fallback.
        assert_eq!(stream.title.as_deref(), Some("안녕하세요 도와주세요"));
    }

    #[test]
    fn string_content_is_single_prompt_block() {
        let line = json!({
            "type": "user",
            "uuid": "line-2",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:01Z",
            "message": { "content": "문자열 프롬프트입니다" }
        });

        let req = normalize_lines(&[line], &ctx(), essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 1);
        let e = &req.events[0];
        assert_eq!(e.event_type, "prompt");
        assert_eq!(e.external_id, "line-2#0");
        assert_eq!(e.body.as_deref(), Some("문자열 프롬프트입니다"));
    }

    #[test]
    fn tool_use_block_maps_to_tool_use_event() {
        let line = json!({
            "type": "assistant",
            "uuid": "line-3",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:02Z",
            "message": {
                "model": "claude-sonnet-4-5",
                "content": [
                    { "type": "tool_use", "id": "toolu_abc", "name": "Bash", "input": { "command": "ls -la" } }
                ]
            }
        });

        let req = normalize_lines(&[line], &ctx(), BodyPolicy::Full).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 1);
        let e = &req.events[0];
        assert_eq!(e.event_type, "tool_use");
        assert_eq!(e.external_id, "toolu_abc");
        assert_eq!(e.title.as_deref(), Some("Bash"));
        assert_eq!(e.body.as_deref(), Some("ls -la"));
        assert_eq!(e.model.as_deref(), Some("claude-sonnet-4-5"));
        let input = e.metadata.as_ref().unwrap().get("input").unwrap();
        assert_eq!(input.get("command").unwrap(), "ls -la");
    }

    #[test]
    fn tool_result_block_maps_to_tool_result_event() {
        let line = json!({
            "type": "user",
            "uuid": "line-4",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:03Z",
            "message": {
                "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_abc", "content": "결과 텍스트" }
                ]
            },
            "toolUseResult": {}
        });

        let req = normalize_lines(&[line], &ctx(), essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 1);
        let e = &req.events[0];
        assert_eq!(e.event_type, "tool_result");
        assert_eq!(e.external_id, "toolu_abc#result");
        assert_eq!(e.body.as_deref(), Some("결과 텍스트"));
    }

    #[test]
    fn thinking_block_is_skipped() {
        let line = json!({
            "type": "assistant",
            "uuid": "line-5",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:04Z",
            "message": {
                "model": "claude-sonnet-4-5",
                "content": [
                    { "type": "thinking", "thinking": "내부 사고 과정" },
                    { "type": "text", "text": "답변입니다" }
                ]
            }
        });

        let req = normalize_lines(&[line], &ctx(), essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 1);
        assert_eq!(req.events[0].event_type, "response");
        assert_eq!(req.events[0].body.as_deref(), Some("답변입니다"));
    }

    #[test]
    fn usage_is_assigned_only_to_first_text_block() {
        let line = json!({
            "type": "assistant",
            "uuid": "line-multi",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:06Z",
            "message": {
                "model": "claude-sonnet-4-5",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 20,
                    "cache_read_input_tokens": 5
                },
                "content": [
                    { "type": "text", "text": "첫 번째 응답" },
                    { "type": "tool_use", "id": "toolu_multi", "name": "Bash", "input": { "command": "ls" } },
                    { "type": "text", "text": "두 번째 응답" }
                ]
            }
        });

        let req = normalize_lines(&[line], &ctx(), essential()).expect("이벤트 있어야 함");
        let responses: Vec<_> = req
            .events
            .iter()
            .filter(|e| e.event_type == "response")
            .collect();
        assert_eq!(responses.len(), 2);

        let first = responses[0];
        assert_eq!(first.body.as_deref(), Some("첫 번째 응답"));
        assert_eq!(first.tokens_in, Some(100));
        assert_eq!(first.tokens_out, Some(20));
        assert!(first.metadata.is_some());

        let second = responses[1];
        assert_eq!(second.body.as_deref(), Some("두 번째 응답"));
        assert_eq!(second.tokens_in, None);
        assert_eq!(second.tokens_out, None);
        assert!(second.metadata.is_none());
    }

    #[test]
    fn subagent_stream_id_includes_agent_id() {
        let line = json!({
            "type": "user",
            "uuid": "line-6",
            "sessionId": "sess-1",
            "isSidechain": true,
            "timestamp": "2026-07-01T00:00:05Z",
            "message": { "content": "서브에이전트 프롬프트" }
        });

        let sub_ctx = FileContext { agent_id: Some("agent-123".to_string()) };
        let req = normalize_lines(&[line], &sub_ctx, essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events[0].stream_id, "claude_code:sess-1:agent:agent-123");

        let stream = req.stream.expect("stream 있어야 함");
        assert_eq!(stream.id, "claude_code:sess-1:agent:agent-123");
        assert_eq!(stream.kind.as_deref(), Some("agent"));
        assert_eq!(
            stream.metadata.unwrap().get("parentStream").unwrap(),
            "claude_code:sess-1"
        );
    }

    #[test]
    fn ai_title_sets_stream_title_without_events() {
        let line = json!({
            "type": "ai-title",
            "aiTitle": "제목입니다",
            "sessionId": "sess-1"
        });

        let req = normalize_lines(&[line], &ctx(), essential()).expect("title만 있어도 반환되어야 함");
        assert!(req.events.is_empty());
        assert_eq!(req.stream.unwrap().title.as_deref(), Some("제목입니다"));
    }

    #[test]
    fn skip_only_lines_return_none() {
        let line = json!({
            "type": "queue-operation",
            "sessionId": "sess-1"
        });
        assert!(normalize_lines(&[line], &ctx(), essential()).is_none());
    }

    /// ai-title이 없을 때 첫 prompt가 짧으면("ㄱㄱ" 같은 응답성 문구) 건너뛰고 뒤따르는
    /// "의미 있는" prompt(15자+)를 title로 채택해야 한다(capture/policy.rs::derive_stream_title).
    #[test]
    fn title_fallback_skips_short_first_prompt_and_uses_meaningful_one() {
        let lines = vec![
            json!({
                "type": "user",
                "uuid": "line-short",
                "sessionId": "sess-title",
                "timestamp": "2026-07-01T00:00:00Z",
                "message": { "content": "ㄱㄱ" }
            }),
            json!({
                "type": "user",
                "uuid": "line-long",
                "sessionId": "sess-title",
                "timestamp": "2026-07-01T00:00:01Z",
                "message": { "content": "이 프로젝트 구조를 좀 설명해 주실 수 있을까요" }
            }),
        ];

        let req = normalize_lines(&lines, &ctx(), essential()).expect("이벤트 있어야 함");
        let stream = req.stream.expect("stream 있어야 함");
        assert_eq!(
            stream.title.as_deref(),
            Some("이 프로젝트 구조를 좀 설명해 주실 수 있을까요")
        );
    }

    // ── 시스템 노이즈 prompt 필터(신규 캡처) ─────────────────────────
    // isMeta 라인 전체 skip + 텍스트 블록 레벨 메타 마커 skip(is_meta_prompt_text).
    // 소급 정리는 migrations/V3__prune_meta_prompts.sql, db.rs 통합테스트 참고.

    #[test]
    fn is_meta_line_is_skipped_entirely() {
        let line = json!({
            "type": "user",
            "uuid": "line-meta-1",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:00Z",
            "isMeta": true,
            "message": {
                "content": "<local-command-caveat>Caveat: 로컬 커맨드 실행 중 생성된 메시지입니다.</local-command-caveat>"
            }
        });

        // 이벤트도 title도 없으니 전체 라인이 노이즈로만 구성 → None.
        assert!(normalize_lines(&[line], &ctx(), essential()).is_none());
    }

    #[test]
    fn meta_command_name_string_content_is_skipped() {
        let line = json!({
            "type": "user",
            "uuid": "line-meta-2",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:00Z",
            "message": {
                "content": "<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args></command-args>"
            }
        });

        assert!(normalize_lines(&[line], &ctx(), essential()).is_none());
    }

    #[test]
    fn meta_local_command_stdout_string_content_is_skipped() {
        let line = json!({
            "type": "user",
            "uuid": "line-meta-3",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:00Z",
            "message": {
                "content": "<local-command-stdout>Set model to Opus</local-command-stdout>"
            }
        });

        assert!(normalize_lines(&[line], &ctx(), essential()).is_none());
    }

    #[test]
    fn meta_interrupted_string_content_is_skipped() {
        let line = json!({
            "type": "user",
            "uuid": "line-meta-4",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:00Z",
            "message": { "content": "[Request interrupted by user]" }
        });

        assert!(normalize_lines(&[line], &ctx(), essential()).is_none());
    }

    #[test]
    fn mixed_meta_and_normal_blocks_keep_only_normal_prompt() {
        let line = json!({
            "type": "user",
            "uuid": "line-mixed",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:00Z",
            "message": {
                "content": [
                    { "type": "text", "text": "<command-args></command-args>" },
                    { "type": "text", "text": "정상적인 프롬프트입니다" }
                ]
            }
        });

        let req = normalize_lines(&[line], &ctx(), essential()).expect("정상 블록이 있으므로 반환돼야 함");
        assert_eq!(req.events.len(), 1);
        let e = &req.events[0];
        assert_eq!(e.event_type, "prompt");
        assert_eq!(e.external_id, "line-mixed#1");
        assert_eq!(e.body.as_deref(), Some("정상적인 프롬프트입니다"));
    }

    // ── 일일 AI 요약 자기 캡처 루프 차단(M7-①, ADR-0016, capture/policy.rs::is_summarizer_project) ──

    #[test]
    fn normalize_lines_skips_summarizer_own_session_entirely() {
        let line = json!({
            "type": "user",
            "uuid": "line-summarizer",
            "sessionId": "sess-summarizer",
            "cwd": "/Users/me/.logroom/summarizer",
            "timestamp": "2026-07-16T00:00:00Z",
            "message": { "content": "충분히 긴 정상적인 프롬프트 내용입니다" }
        });

        assert!(
            normalize_lines(&[line], &ctx(), essential()).is_none(),
            "summarizer 고정 작업 디렉토리(cwd)의 세션은 정규화 단계에서 통째로 skip돼야 함"
        );
    }

    #[test]
    fn normalize_lines_keeps_unrelated_project_session() {
        let line = json!({
            "type": "user",
            "uuid": "line-normal-project",
            "sessionId": "sess-normal-project",
            "cwd": "/Users/me/src/logroom",
            "timestamp": "2026-07-16T00:00:00Z",
            "message": { "content": "충분히 긴 정상적인 프롬프트 내용입니다" }
        });

        let req = normalize_lines(&[line], &ctx(), essential())
            .expect("summarizer 디렉토리가 아니면 정상적으로 캡처돼야 함");
        assert_eq!(req.events.len(), 1);
    }

    // ── essential body policy 절단(ADR-0012) ────────────────────
    // truncate_with_ellipsis/apply_tool_result_policy/tool_use_metadata 자체의 단위 테스트는
    // capture::policy(공용 모듈)로 이동했다. 여기서는 normalize_lines 전체 파이프라인을 통한
    // end-to-end 동작만 검증한다.

    #[test]
    fn tool_result_event_body_truncated_end_to_end_under_essential_policy() {
        let long_content = "결과".repeat(200); // 400 코드포인트, 256 초과
        let line = json!({
            "type": "user",
            "uuid": "line-long-result",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:03Z",
            "message": {
                "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_long", "content": long_content }
                ]
            }
        });

        let req = normalize_lines(&[line], &ctx(), essential()).expect("이벤트 있어야 함");
        let e = &req.events[0];
        assert_eq!(e.event_type, "tool_result");
        let body = e.body.as_deref().expect("body 있어야 함");
        assert_eq!(body.chars().count(), 257);
        assert!(body.ends_with('…'));
    }

    #[test]
    fn tool_use_event_metadata_uses_input_preview_end_to_end_under_essential_policy() {
        let line = json!({
            "type": "assistant",
            "uuid": "line-long-tool-use",
            "sessionId": "sess-1",
            "timestamp": "2026-07-01T00:00:02Z",
            "message": {
                "model": "claude-sonnet-4-5",
                "content": [
                    { "type": "tool_use", "id": "toolu_long", "name": "Bash", "input": { "command": "w".repeat(600) } }
                ]
            }
        });

        let req = normalize_lines(&[line], &ctx(), essential()).expect("이벤트 있어야 함");
        let e = &req.events[0];
        assert_eq!(e.event_type, "tool_use");
        let metadata = e.metadata.as_ref().expect("metadata 있어야 함");
        assert!(metadata.get("input").is_none());
        assert!(metadata.get("inputPreview").and_then(Value::as_str).is_some());
        // body(한줄 요약)는 절단 없이 현행 유지.
        assert_eq!(e.body.as_deref(), Some("w".repeat(600)).as_deref());
    }

    #[test]
    fn normalize_project_finds_repo_root() {
        let base = std::env::temp_dir().join(format!("logroom-test-{}", std::process::id()));
        let repo_root = base.join("repo");
        let nested = repo_root.join("apps").join("desktop");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(repo_root.join(".git")).unwrap();

        let normalized = normalize_project(nested.to_str().unwrap());
        assert_eq!(normalized, repo_root.to_string_lossy());

        std::fs::remove_dir_all(&base).unwrap();
    }

    /// git worktree cwd(`.git`이 `gitdir: <본레포>/.git/worktrees/<name>` 한 줄인 파일)는
    /// 본 레포 루트로 정규화돼야 한다(실측 포맷, `git worktree add`로 확인).
    #[test]
    fn normalize_project_resolves_worktree_git_file_to_main_repo_root() {
        let base = std::env::temp_dir().join(format!("logroom-test-worktree-{}", std::process::id()));
        let repo_root = base.join("repo");
        std::fs::create_dir_all(repo_root.join(".git").join("worktrees").join("agent-xyz")).unwrap();

        let worktree_dir = base.join(".claude").join("worktrees").join("agent-xyz");
        std::fs::create_dir_all(&worktree_dir).unwrap();
        std::fs::write(
            worktree_dir.join(".git"),
            format!("gitdir: {}/.git/worktrees/agent-xyz\n", repo_root.to_string_lossy()),
        )
        .unwrap();

        let normalized = normalize_project(worktree_dir.to_str().unwrap());
        assert_eq!(normalized, repo_root.to_string_lossy());

        std::fs::remove_dir_all(&base).unwrap();
    }

    /// `.git` 파일 내용이 `gitdir:` 패턴이 아니거나 `/.git/worktrees/` 세그먼트가 없으면(예: 손상,
    /// 서브모듈) 기존 폴백대로 그 위치(파일이 있던 디렉토리) 자체를 반환해야 한다.
    #[test]
    fn normalize_project_falls_back_when_git_file_is_unparsable() {
        let base = std::env::temp_dir().join(format!("logroom-test-broken-git-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join(".git"), "not a gitdir line\n").unwrap();

        let normalized = normalize_project(base.to_str().unwrap());
        assert_eq!(normalized, base.to_string_lossy());

        std::fs::remove_dir_all(&base).unwrap();
    }

    /// 서브모듈 `.git` 파일(`gitdir: .../.git/modules/<name>`)은 `/.git/worktrees/` 패턴이 없으므로
    /// 자연히 폴백(그 위치 자체를 반환) — worktree 정규화가 서브모듈에 영향을 주지 않는지 확인.
    #[test]
    fn normalize_project_submodule_git_file_is_unaffected() {
        let base = std::env::temp_dir().join(format!("logroom-test-submodule-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join(".git"),
            "gitdir: ../.git/modules/sub\n",
        )
        .unwrap();

        let normalized = normalize_project(base.to_str().unwrap());
        assert_eq!(normalized, base.to_string_lossy());

        std::fs::remove_dir_all(&base).unwrap();
    }
}
