//! 인제스트 계약의 Rust 측 타입 (docs/03-capture.md + packages/core/src/schema.ts).
//! zod 스키마(camelCase)와 serde `rename_all = "camelCase"` 로 필드명을 맞춘다.
//! 동기화는 `packages/core/test/fixtures/ingest-sample.json` 을 양측이 파싱하는
//! 계약 테스트로 검증한다(하단 tests 모듈 + core 의 vitest).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// POST /v1/ingest 의 `stream` (선택 — 있으면 upsert).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInput {
    pub id: String,
    pub source: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub ended_at: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// POST /v1/ingest 의 `events[]` 항목.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventInput {
    pub external_id: String,
    pub stream_id: String,
    pub ts: i64,
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tokens_in: Option<i64>,
    #[serde(default)]
    pub tokens_out: Option<i64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// POST /v1/ingest 바디.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRequest {
    #[serde(default)]
    pub stream: Option<StreamInput>,
    pub events: Vec<EventInput>,
}

/// GET /v1/health 응답.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 공유 fixture(진실소스)를 Rust serde 로 파싱해 zod↔Rust 계약을 검증한다.
    /// 같은 파일을 core 의 vitest(schema.test.ts)도 ingestRequestSchema 로 파싱한다.
    #[test]
    fn shared_fixture_parses() {
        let raw = include_str!(
            "../../../../packages/core/test/fixtures/ingest-sample.json"
        );
        let req: IngestRequest = serde_json::from_str(raw).expect("fixture 파싱 실패");
        assert_eq!(req.events.len(), 2);
        let stream = req.stream.as_ref().expect("stream 있어야 함");
        assert_eq!(stream.source, "claude_code");
        assert_eq!(req.events[0].event_type, "prompt");
        assert_eq!(req.events[1].tokens_in, Some(1200));
        assert_eq!(req.events[1].model.as_deref(), Some("claude-sonnet-4-5"));
    }
}
