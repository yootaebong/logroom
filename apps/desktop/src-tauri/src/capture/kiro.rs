//! Kiro CLI JSONL 라인 → LogRoom Event/Stream 정규화 (docs/03-capture.md 소스2).
//! 순수함수: 파싱된 `serde_json::Value` 라인들 + 시작 ts만 입력으로 받고 부수효과(파일I/O 등) 없음.
//! 메타(`<sessionId>.json`) 로드·`repo_root` 정규화·backfill 시작 ts 산출은 watch.rs(불순 영역)가 담당해
//! [`KiroContext`]/`start_ts`로 주입한다.

use crate::capture::config::BodyPolicy;
use crate::capture::policy::{apply_tool_result_policy, derive_stream_title, tool_use_metadata};
use crate::model::{EventInput, IngestRequest, StreamInput};
use serde_json::Value;

pub const SOURCE: &str = "kiro_cli";

/// `kiro_cli:<sessionId>` 형태의 stream id. watch.rs도 `max_ts_of_stream` 조회 시 재사용.
pub fn stream_id(session_id: &str) -> String {
    format!("{SOURCE}:{session_id}")
}

/// 파일 하나(세션)의 컨텍스트. 세션 메타(`<sessionId>.json`)에서 watch.rs가 채운다.
/// `project`는 이미 [`normalize::normalize_project`](crate::capture::normalize::normalize_project)로
/// 레포 루트 정규화까지 끝난 값이어야 한다(이 모듈은 파일시스템을 확인하지 않는다).
#[derive(Debug, Clone, Default)]
pub struct KiroContext {
    pub session_id: String,
    pub title: Option<String>,
    pub project: Option<String>,
    pub parent_session_id: Option<String>,
}

fn truncate60(text: &str) -> String {
    text.chars().take(60).collect()
}

/// ToolResults 블록의 `data.content`(문자열 또는 배열)를 body 문자열로 변환.
/// 항목 스키마는 실측 미확정이므로 kiro 관례(`data` 필드)를 우선하고 `text`/원본 문자열을 방어적으로 처리한다.
fn stringify_tool_result_content(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(s) => s.clone(),
                _ => item
                    .get("data")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("text").and_then(Value::as_str))
                    .map(str::to_string)
                    .unwrap_or_else(|| item.to_string()),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn blank_event(
    external_id: String,
    stream_id: &str,
    ts: i64,
    event_type: &str,
    title: Option<String>,
    body: Option<String>,
    metadata: Option<Value>,
) -> EventInput {
    EventInput {
        external_id,
        stream_id: stream_id.to_string(),
        ts,
        source: SOURCE.to_string(),
        event_type: event_type.to_string(),
        title,
        body,
        model: None,
        tokens_in: None,
        tokens_out: None,
        url: None,
        parent_id: None, // Kiro CLI 라인엔 Claude의 parentUuid 같은 스레드 체인이 없다.
        metadata,
    }
}

/// `Prompt.data.content[]` → prompt 이벤트들(`kind:"text"` 블록만).
fn prompt_events(message_id: &str, content: &[Value], stream_id: &str, ts: i64) -> Vec<EventInput> {
    content
        .iter()
        .enumerate()
        .filter(|(_, block)| block.get("kind").and_then(Value::as_str) == Some("text"))
        .filter_map(|(idx, block)| {
            let text = block.get("data").and_then(Value::as_str)?;
            Some(blank_event(
                format!("prompt:{message_id}#{idx}"),
                stream_id,
                ts,
                "prompt",
                Some(truncate60(text)),
                Some(text.to_string()),
                None,
            ))
        })
        .collect()
}

/// `AssistantMessage.data.content[]` → response/tool_use 이벤트들. `thinking` 블록은 스킵.
fn assistant_events(
    message_id: &str,
    content: &[Value],
    stream_id: &str,
    ts: i64,
    body_policy: BodyPolicy,
) -> Vec<EventInput> {
    let mut events = Vec::new();
    for (idx, block) in content.iter().enumerate() {
        let bkind = block.get("kind").and_then(Value::as_str).unwrap_or("");
        match bkind {
            "thinking" => continue, // 기본 저장 안 함(검색 노이즈·프라이버시, normalize.rs Claude 동일 정책)
            "text" => {
                let Some(text) = block.get("data").and_then(Value::as_str) else {
                    continue;
                };
                events.push(blank_event(
                    format!("response:{message_id}#{idx}"),
                    stream_id,
                    ts,
                    "response",
                    None,
                    Some(text.to_string()),
                    None,
                ));
            }
            "toolUse" => {
                let Some(tool_data) = block.get("data") else {
                    continue;
                };
                let Some(tool_use_id) = tool_data.get("toolUseId").and_then(Value::as_str) else {
                    continue;
                };
                let name = tool_data.get("name").and_then(Value::as_str).unwrap_or_default();
                let input = tool_data.get("input").cloned();
                let metadata = input.map(|inp| tool_use_metadata(inp, body_policy));
                events.push(blank_event(
                    tool_use_id.to_string(),
                    stream_id,
                    ts,
                    "tool_use",
                    Some(name.to_string()),
                    None,
                    metadata,
                ));
            }
            _ => {}
        }
    }
    events
}

/// `ToolResults.data.content[]` → tool_result 이벤트들(`kind:"toolResult"` 블록만).
/// externalId는 `tool_use`와 동일한 `toolUseId`를 쓰므로 `(source, external_id)` UNIQUE 충돌 방지를 위해
/// `#result` suffix 필수(normalize.rs Claude tool_result와 동일 규칙).
fn tool_result_events(
    content: &[Value],
    stream_id: &str,
    ts: i64,
    body_policy: BodyPolicy,
) -> Vec<EventInput> {
    content
        .iter()
        .filter(|block| block.get("kind").and_then(Value::as_str) == Some("toolResult"))
        .filter_map(|block| {
            let result = block.get("data")?;
            let tool_use_id = result.get("toolUseId").and_then(Value::as_str)?;
            let body = result
                .get("content")
                .map(stringify_tool_result_content)
                .map(|b| apply_tool_result_policy(&b, body_policy));
            let metadata = result.get("status").cloned().map(|status| {
                let mut m = serde_json::Map::new();
                m.insert("status".to_string(), status);
                Value::Object(m)
            });
            Some(blank_event(
                format!("{tool_use_id}#result"),
                stream_id,
                ts,
                "tool_result",
                None,
                body,
                metadata,
            ))
        })
        .collect()
}

/// 라인의 `meta.timestamp`(epoch 초, `meta` 자체가 null일 수 있음)를 epoch ms로.
/// 정수가 아닌 소수점 표현(`meta.timestamp: 123.0` 등)도 방어적으로 처리한다.
fn prompt_meta_timestamp_ms(data: &Value) -> Option<i64> {
    let ts = data.pointer("/meta/timestamp")?;
    let sec = ts.as_i64().or_else(|| ts.as_f64().map(|f| f as i64))?;
    Some(sec * 1000)
}

/// 한 파일에서 새로 읽은 라인들(파싱된 JSON) → 하나의 `IngestRequest`.
/// 라인 레벨 timestamp가 없으므로 `start_ts`부터 단조증가하는 `ts`를 보간한다:
/// `Prompt`이고 `meta.timestamp`가 있으면 `cur = max(cur+1, timestamp*1000)`(앵커링), 아니면 `cur += 1`.
/// 한 라인의 모든 블록 이벤트는 같은 `cur`를 공유한다(블록 구분은 externalId로 충분).
/// `body_policy`는 tool_result body·tool_use metadata 절단 여부를 결정한다(ADR-0012, config.rs 참고).
pub fn normalize_kiro_lines(
    lines: &[Value],
    ctx: &KiroContext,
    start_ts: i64,
    body_policy: BodyPolicy,
) -> Option<IngestRequest> {
    let sid = stream_id(&ctx.session_id);
    let mut cur = start_ts;
    let mut events = Vec::new();

    for line in lines {
        let kind = line.get("kind").and_then(Value::as_str).unwrap_or("");
        let data = line.get("data");

        cur = if kind == "Prompt" {
            match data.and_then(prompt_meta_timestamp_ms) {
                Some(ts_ms) => (cur + 1).max(ts_ms),
                None => cur + 1,
            }
        } else {
            cur + 1
        };

        let Some(data) = data else { continue };
        let message_id = data.get("message_id").and_then(Value::as_str).unwrap_or_default();
        let content = data.get("content").and_then(Value::as_array);

        match (kind, content) {
            ("Prompt", Some(content)) => {
                events.extend(prompt_events(message_id, content, &sid, cur));
            }
            ("AssistantMessage", Some(content)) => {
                events.extend(assistant_events(message_id, content, &sid, cur, body_policy));
            }
            ("ToolResults", Some(content)) => {
                events.extend(tool_result_events(content, &sid, cur, body_policy));
            }
            // Compaction / 그 외 kind: skip.
            _ => {}
        }
    }

    if events.is_empty() && ctx.title.is_none() {
        return None;
    }

    let metadata = ctx
        .parent_session_id
        .as_ref()
        .map(|pid| serde_json::json!({ "parent_session_id": pid }));

    // 메타(`<sessionId>.json`) title이 우선(watch.rs가 채움). 없을 때만 prompt 본문 휴리스틱으로 폴백
    // (normalize.rs Claude와 동일 규칙, capture/policy.rs).
    let title = ctx.title.clone().or_else(|| {
        let prompt_bodies = events
            .iter()
            .filter(|e| e.event_type == "prompt")
            .filter_map(|e| e.body.as_deref());
        derive_stream_title(prompt_bodies)
    });

    let stream = StreamInput {
        id: sid,
        source: SOURCE.to_string(),
        kind: Some("session".to_string()),
        title,
        project: ctx.project.clone(),
        git_branch: None,
        started_at: None,
        ended_at: None,
        status: None,
        metadata,
    };

    Some(IngestRequest { stream: Some(stream), events })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> KiroContext {
        KiroContext {
            session_id: "sess-1".to_string(),
            title: None,
            project: None,
            parent_session_id: None,
        }
    }

    /// 대부분의 테스트는 실제 기본값(essential)으로 검증한다.
    fn essential() -> BodyPolicy {
        BodyPolicy::Essential
    }

    #[test]
    fn prompt_text_becomes_prompt_event() {
        let line = json!({
            "kind": "Prompt",
            "version": 1,
            "data": {
                "message_id": "msg-1",
                "content": [{ "kind": "text", "data": "안녕하세요 도와주세요" }],
                "meta": null,
            }
        });

        let req = normalize_kiro_lines(&[line], &ctx(), 1000, essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 1);
        let e = &req.events[0];
        assert_eq!(e.event_type, "prompt");
        assert_eq!(e.external_id, "prompt:msg-1#0");
        assert_eq!(e.stream_id, "kiro_cli:sess-1");
        assert_eq!(e.title.as_deref(), Some("안녕하세요 도와주세요"));
        assert_eq!(e.body.as_deref(), Some("안녕하세요 도와주세요"));
    }

    #[test]
    fn assistant_text_becomes_response_event() {
        let line = json!({
            "kind": "AssistantMessage",
            "version": 1,
            "data": {
                "message_id": "msg-2",
                "content": [{ "kind": "text", "data": "답변입니다" }]
            }
        });

        let req = normalize_kiro_lines(&[line], &ctx(), 1000, essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 1);
        let e = &req.events[0];
        assert_eq!(e.event_type, "response");
        assert_eq!(e.external_id, "response:msg-2#0");
        assert_eq!(e.body.as_deref(), Some("답변입니다"));
    }

    #[test]
    fn assistant_thinking_is_skipped() {
        let line = json!({
            "kind": "AssistantMessage",
            "version": 1,
            "data": {
                "message_id": "msg-3",
                "content": [
                    { "kind": "thinking", "data": "내부 사고 과정" },
                    { "kind": "text", "data": "답변입니다" }
                ]
            }
        });

        let req = normalize_kiro_lines(&[line], &ctx(), 1000, essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 1);
        assert_eq!(req.events[0].event_type, "response");
        assert_eq!(req.events[0].body.as_deref(), Some("답변입니다"));
    }

    #[test]
    fn assistant_tool_use_maps_to_tool_use_event() {
        let line = json!({
            "kind": "AssistantMessage",
            "version": 1,
            "data": {
                "message_id": "msg-4",
                "content": [{
                    "kind": "toolUse",
                    "data": { "toolUseId": "tool-abc", "name": "execute_bash", "input": { "command": "ls -la" } }
                }]
            }
        });

        let req = normalize_kiro_lines(&[line], &ctx(), 1000, BodyPolicy::Full).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 1);
        let e = &req.events[0];
        assert_eq!(e.event_type, "tool_use");
        assert_eq!(e.external_id, "tool-abc");
        assert_eq!(e.title.as_deref(), Some("execute_bash"));
        let input = e.metadata.as_ref().unwrap().get("input").unwrap();
        assert_eq!(input.get("command").unwrap(), "ls -la");
    }

    #[test]
    fn tool_results_toolresult_maps_with_result_suffix() {
        let line = json!({
            "kind": "ToolResults",
            "version": 1,
            "data": {
                "message_id": "msg-5",
                "content": [{
                    "kind": "toolResult",
                    "data": { "toolUseId": "tool-abc", "content": "결과 텍스트", "status": "success" }
                }],
                "results": {}
            }
        });

        let req = normalize_kiro_lines(&[line], &ctx(), 1000, essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 1);
        let e = &req.events[0];
        assert_eq!(e.event_type, "tool_result");
        // tool_use와 동일 toolUseId를 쓰므로 UNIQUE(source, external_id) 충돌 방지 위해 suffix 필수.
        assert_eq!(e.external_id, "tool-abc#result");
        assert_eq!(e.body.as_deref(), Some("결과 텍스트"));
        assert_eq!(e.metadata.as_ref().unwrap().get("status").unwrap(), "success");
    }

    #[test]
    fn ts_increments_monotonically_without_meta_timestamp() {
        let lines = vec![
            json!({
                "kind": "Prompt",
                "data": { "message_id": "m1", "content": [{ "kind": "text", "data": "첫 프롬프트" }] }
            }),
            json!({
                "kind": "Compaction",
                "data": { "message_id": "m-compact" }
            }),
            json!({
                "kind": "AssistantMessage",
                "data": { "message_id": "m2", "content": [{ "kind": "text", "data": "응답" }] }
            }),
        ];

        let req = normalize_kiro_lines(&lines, &ctx(), 1000, essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 2);
        // Compaction 라인도 ts를 소비하므로(각 라인 처리 시 +1) 1000 -> 1001(prompt) -> 1002(skip) -> 1003(response).
        assert_eq!(req.events[0].ts, 1001);
        assert_eq!(req.events[1].ts, 1003);
    }

    #[test]
    fn prompt_meta_timestamp_anchors_ts_forward() {
        let lines = vec![
            json!({
                "kind": "Prompt",
                "data": { "message_id": "m1", "content": [{ "kind": "text", "data": "프롬프트" }] }
            }),
            json!({
                "kind": "Prompt",
                "data": {
                    "message_id": "m2",
                    "content": [{ "kind": "text", "data": "나중 프롬프트" }],
                    "meta": { "timestamp": 2_000_000 }
                }
            }),
        ];

        // start_ts(1000) + 1 = 1001 → 두 번째 줄은 meta.timestamp(2_000_000초 = 2_000_000_000ms)로 앵커링.
        let req = normalize_kiro_lines(&lines, &ctx(), 1000, essential()).expect("이벤트 있어야 함");
        assert_eq!(req.events.len(), 2);
        assert_eq!(req.events[0].ts, 1001);
        assert_eq!(req.events[1].ts, 2_000_000_000);
    }

    #[test]
    fn stream_uses_ctx_title_and_project() {
        let line = json!({
            "kind": "Prompt",
            "data": { "message_id": "m1", "content": [{ "kind": "text", "data": "프롬프트" }] }
        });
        let c = KiroContext {
            session_id: "sess-9".to_string(),
            title: Some("세션 제목".to_string()),
            project: Some("/Users/me/src/logroom".to_string()),
            parent_session_id: Some("parent-sess".to_string()),
        };

        let req = normalize_kiro_lines(&[line], &c, 1000, essential()).expect("이벤트 있어야 함");
        let stream = req.stream.expect("stream 있어야 함");
        assert_eq!(stream.id, "kiro_cli:sess-9");
        assert_eq!(stream.title.as_deref(), Some("세션 제목"));
        assert_eq!(
            stream.project.as_deref(),
            Some("/Users/me/src/logroom")
        );
        assert_eq!(
            stream.metadata.unwrap().get("parent_session_id").unwrap(),
            "parent-sess"
        );
    }

    /// ctx.title(메타 title)이 없으면 prompt 본문 휴리스틱으로 폴백해야 한다
    /// (normalize.rs Claude와 동일 규칙, capture/policy.rs::derive_stream_title). 첫 prompt가
    /// 짧으면("ㄱㄱ") 건너뛰고 뒤따르는 "의미 있는" prompt(15자+)를 채택한다.
    #[test]
    fn title_falls_back_to_prompt_heuristic_when_ctx_title_absent() {
        let lines = vec![
            json!({
                "kind": "Prompt",
                "data": { "message_id": "m1", "content": [{ "kind": "text", "data": "ㄱㄱ" }] }
            }),
            json!({
                "kind": "Prompt",
                "data": {
                    "message_id": "m2",
                    "content": [{ "kind": "text", "data": "이 프로젝트 구조를 좀 설명해 주실 수 있을까요" }]
                }
            }),
        ];

        let req = normalize_kiro_lines(&lines, &ctx(), 1000, essential()).expect("이벤트 있어야 함");
        let stream = req.stream.expect("stream 있어야 함");
        assert_eq!(
            stream.title.as_deref(),
            Some("이 프로젝트 구조를 좀 설명해 주실 수 있을까요")
        );
    }

    // ── essential body policy 절단(ADR-0012) ────────────────────
    // truncate_with_ellipsis/apply_tool_result_policy/tool_use_metadata 자체의 단위 테스트는
    // capture::policy(공용 모듈, normalize.rs Claude와 동일 규칙)로 이동했다. 여기서는
    // normalize_kiro_lines 전체 파이프라인을 통한 end-to-end 동작만 검증한다.

    #[test]
    fn tool_result_body_truncated_end_to_end_under_essential_policy() {
        let long_content = "결과".repeat(200); // 400 코드포인트, 256 초과
        let line = json!({
            "kind": "ToolResults",
            "data": {
                "message_id": "msg-long",
                "content": [{
                    "kind": "toolResult",
                    "data": { "toolUseId": "tool-long", "content": long_content, "status": "success" }
                }]
            }
        });

        let req = normalize_kiro_lines(&[line], &ctx(), 1000, essential()).expect("이벤트 있어야 함");
        let e = &req.events[0];
        assert_eq!(e.event_type, "tool_result");
        let body = e.body.as_deref().expect("body 있어야 함");
        assert_eq!(body.chars().count(), 257);
        assert!(body.ends_with('…'));
        // status 등 기존 메타키는 절단과 무관하게 그대로 유지.
        assert_eq!(e.metadata.as_ref().unwrap().get("status").unwrap(), "success");
    }

    #[test]
    fn tool_result_body_not_truncated_under_full_policy_end_to_end() {
        let long_content = "결과".repeat(200);
        let line = json!({
            "kind": "ToolResults",
            "data": {
                "message_id": "msg-long-full",
                "content": [{
                    "kind": "toolResult",
                    "data": { "toolUseId": "tool-long-full", "content": long_content.clone() }
                }]
            }
        });

        let req = normalize_kiro_lines(&[line], &ctx(), 1000, BodyPolicy::Full).expect("이벤트 있어야 함");
        let e = &req.events[0];
        assert_eq!(e.body.as_deref(), Some(long_content.as_str()));
    }

    #[test]
    fn tool_use_input_preview_end_to_end_under_essential_policy() {
        let line = json!({
            "kind": "AssistantMessage",
            "data": {
                "message_id": "msg-long-tool",
                "content": [{
                    "kind": "toolUse",
                    "data": { "toolUseId": "tool-long", "name": "execute_bash", "input": { "command": "w".repeat(600) } }
                }]
            }
        });

        let req = normalize_kiro_lines(&[line], &ctx(), 1000, essential()).expect("이벤트 있어야 함");
        let e = &req.events[0];
        assert_eq!(e.event_type, "tool_use");
        let metadata = e.metadata.as_ref().expect("metadata 있어야 함");
        assert!(metadata.get("input").is_none());
        let preview = metadata.get("inputPreview").and_then(Value::as_str).expect("inputPreview 있어야 함");
        assert!(preview.chars().count() <= 513); // 512 + '…'
        assert!(preview.ends_with('…'));
    }
}
