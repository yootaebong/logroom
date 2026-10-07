//! 캡처 저장 전 body/title 시크릿 스크럽 (docs/03-capture.md 공통 규칙 "민감정보 스크럽",
//! docs/04-privacy-security.md "시크릿 스크럽"). 정적 컴파일된 정규식(`LazyLock`)으로 알려진
//! 시크릿 패턴을 찾아 `[REDACTED:<type>]`로 치환한다. 순수함수(파일I/O 없음) — `scrub_request`가
//! `IngestRequest`(stream.title + 각 event의 title/body, `tool_use` essential 정책의 metadata
//! `inputPreview` 문자열)에 적용한다. `config.rs`의 `scrubSecrets`(기본 true)가 꺼져 있으면
//! watch.rs가 이 모듈을 호출하지 않는다.
//!
//! **한계**(docs/04-privacy-security.md 참고): 이미 저장된 데이터는 소급 적용되지 않는다 —
//! 재수집(backfill) 시에만 새로 스크럽된다.

use crate::model::IngestRequest;
use regex::Regex;
use serde_json::Value;
use std::borrow::Cow;
use std::sync::LazyLock;

// 순서가 중요하다:
// - anthropic(`sk-ant-…`)은 openai(`sk-…`)의 부분집합 패턴이라 반드시 먼저 매치해야 한다.
// - generic_kv는 가장 일반적인(구체 패턴들의 상위) 폴백이라 가장 마지막에 적용한다.
static ANTHROPIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsk-ant-[A-Za-z0-9\-_]{20,}\b").unwrap());
static OPENAI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bsk-[A-Za-z0-9\-_]{20,}\b").unwrap());
static AWS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap());
static GITHUB: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,}\b|\bgithub_pat_[A-Za-z0-9_]{22,}\b")
        .unwrap()
});
static SLACK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bxox[baprs]-[A-Za-z0-9\-]{10,}\b").unwrap());
static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----").unwrap()
});
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/-]{20,}=*").unwrap());
/// 경계문자(그룹1)·키 이름(그룹2)·구분자(그룹3)는 보존하고 값만 치환한다
/// (`scrub_text`에서 `$1$2$3[REDACTED:generic]`로 치환).
///
/// - 그룹1(경계): 원래 `\b`였으나 `_`/`-`가 포함된 SCREAMING_SNAKE 접두어(`STRIPE_SECRET_KEY` 등)
///   앞에서는 `\b`가 성립하지 않아 매치가 안 됐다. `(?:^|[^A-Za-z0-9_-])`로 바꾸고 캡처해 치환 시
///   그대로 보존한다(문자열 시작이면 빈 문자열).
/// - 그룹2(키 이름): 앞뒤에 `[A-Za-z0-9_-]*`를 둬 `STRIPE_SECRET_KEY`/`MY_ACCESS_TOKEN`처럼 키워드에
///   접두어/접미어가 붙은 SCREAMING_SNAKE 변수명도 매치되게 한다(과탐 여지가 있어 접두 확장이 핵심이고
///   접미 확장은 보수적으로 함께 허용).
/// - 그룹3(구분자): 값 앞에 여는 따옴표 소비는 그대로 두되(치환 시 버려짐 — 기존 동작 유지),
///   *키*의 닫는 따옴표(`["']?`)를 구분자 앞에 추가로 허용해 `{"api_key": "value"}` 같은 JSON 형태도
///   매치되게 한다.
/// - 값 문자클래스에 `[`를 추가로 제외했다: anthropic/openai 등 더 구체적인 패턴이 먼저 값을
///   `[REDACTED:xxx]`로 치환한 뒤에는(예: `ANTHROPIC_API_KEY=[REDACTED:anthropic]`) 그룹2의 접두
///   확장 때문에 이 폴백 패턴도 키 부분에 재매치를 시도하는데, `[`를 값 시작으로 허용하면 이미
///   치환된 마커를 다시 "값"으로 오인해 `[REDACTED:generic]`으로 이중 치환해버린다. `[` 제외로
///   이런 재매치를 원천 차단한다(기존 `anthropic_key_is_redacted_and_not_also_matched_by_openai`,
///   `openai_key_is_redacted` 회귀 방지).
static GENERIC_KV: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(^|[^A-Za-z0-9_-])([A-Za-z0-9_-]*(?:api[_-]?key|secret[_-]?key|access[_-]?token|client[_-]?secret|password)[A-Za-z0-9_-]*)(["']?\s*[:=]\s*)["']?[^\s"'`)\],\[]{8,}["']?"#,
    )
    .unwrap()
});

/// 매치가 없으면 할당 없이 입력 `Cow`를 그대로 반환한다(불필요한 복사 방지).
fn redact<'a>(input: Cow<'a, str>, re: &Regex, label: &str) -> Cow<'a, str> {
    if !re.is_match(&input) {
        return input;
    }
    let replacement = format!("[REDACTED:{label}]");
    Cow::Owned(re.replace_all(&input, replacement.as_str()).into_owned())
}

/// `s`에서 알려진 시크릿 패턴(aws/github/anthropic/openai/slack/private_key/bearer/generic_kv)을 찾아
/// `[REDACTED:<type>]`로 치환한다. 매치가 하나도 없으면 할당 없이 `Cow::Borrowed(s)`를 반환한다.
pub fn scrub_text(s: &str) -> Cow<'_, str> {
    let mut out: Cow<'_, str> = Cow::Borrowed(s);
    out = redact(out, &ANTHROPIC, "anthropic");
    out = redact(out, &OPENAI, "openai");
    out = redact(out, &AWS, "aws");
    out = redact(out, &GITHUB, "github");
    out = redact(out, &SLACK, "slack");
    out = redact(out, &PRIVATE_KEY, "private_key");
    out = redact(out, &BEARER, "bearer");

    if GENERIC_KV.is_match(&out) {
        let replaced = GENERIC_KV.replace_all(&out, "$1$2$3[REDACTED:generic]").into_owned();
        out = Cow::Owned(replaced);
    }
    out
}

/// `scrub_text` 결과가 변경됐을 때만 `text`를 교체한다(변경 없으면 할당 없이 그대로 둠).
fn scrub_string_in_place(text: &mut String) {
    if let Cow::Owned(replaced) = scrub_text(text) {
        *text = replaced;
    }
}

/// `req`의 `stream.title` + 각 `events[].title`/`body`를 스크럽한다. metadata는 `tool_use` essential
/// 정책의 `inputPreview`(문자열) 필드만 적용 대상이다 — `input`(full 정책 원본 object)이나 `status` 등
/// 다른 메타 키는 건드리지 않는다(과제 스코프: body/title 중심 스크럽).
pub fn scrub_request(req: &mut IngestRequest) {
    if let Some(stream) = req.stream.as_mut() {
        if let Some(title) = stream.title.as_mut() {
            scrub_string_in_place(title);
        }
    }

    for event in req.events.iter_mut() {
        if let Some(title) = event.title.as_mut() {
            scrub_string_in_place(title);
        }
        if let Some(body) = event.body.as_mut() {
            scrub_string_in_place(body);
        }
        if let Some(Value::Object(map)) = event.metadata.as_mut() {
            if let Some(Value::String(preview)) = map.get_mut("inputPreview") {
                scrub_string_in_place(preview);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EventInput, StreamInput};
    use serde_json::json;

    // ── 패턴별 마스킹 ────────────────────────────────────────────

    #[test]
    fn aws_access_key_is_redacted() {
        let text = "설정에 AKIAABCDEFGHIJKLMNOP 를 넣지 마세요";
        assert_eq!(scrub_text(text), "설정에 [REDACTED:aws] 를 넣지 마세요");
    }

    #[test]
    fn github_ghp_token_is_redacted() {
        let token = format!("ghp_{}", "a".repeat(36));
        let text = format!("token={token}");
        assert_eq!(scrub_text(&text), "token=[REDACTED:github]");
    }

    #[test]
    fn github_fine_grained_pat_is_redacted() {
        let token = format!("github_pat_{}", "A1".repeat(11));
        let text = format!("export TOKEN={token}");
        assert_eq!(scrub_text(&text), "export TOKEN=[REDACTED:github]");
    }

    #[test]
    fn anthropic_key_is_redacted_and_not_also_matched_by_openai() {
        let key = format!("sk-ant-{}", "A".repeat(25));
        let text = format!("ANTHROPIC_API_KEY={key}");
        let out = scrub_text(&text);
        assert_eq!(out, "ANTHROPIC_API_KEY=[REDACTED:anthropic]");
        // openai 패턴("sk-…")의 부분집합이라 이중 치환되면 안 됨.
        assert!(!out.contains("openai"));
    }

    #[test]
    fn openai_key_is_redacted() {
        let key = format!("sk-{}", "B".repeat(25));
        let text = format!("OPENAI_API_KEY={key}");
        assert_eq!(scrub_text(&text), "OPENAI_API_KEY=[REDACTED:openai]");
    }

    #[test]
    fn slack_token_is_redacted() {
        let text = "SLACK_TOKEN=xoxb-1234567890-abcdefghijklmnop";
        assert_eq!(scrub_text(text), "SLACK_TOKEN=[REDACTED:slack]");
    }

    #[test]
    fn private_key_block_is_redacted() {
        let text = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAKtest\n-----END RSA PRIVATE KEY-----\nafter";
        assert_eq!(scrub_text(text), "before\n[REDACTED:private_key]\nafter");
    }

    #[test]
    fn bearer_token_is_redacted() {
        let text = "Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345";
        assert_eq!(scrub_text(text), "Authorization: [REDACTED:bearer]");
    }

    #[test]
    fn generic_kv_password_preserves_key_name_and_separator() {
        let text = r#"password: "supersecretvalue123""#;
        assert_eq!(scrub_text(text), "password: [REDACTED:generic]");
    }

    #[test]
    fn generic_kv_api_key_with_equals_separator_preserves_key_name() {
        let text = "api_key=abcdefghijklmnop";
        assert_eq!(scrub_text(text), "api_key=[REDACTED:generic]");
    }

    #[test]
    fn generic_kv_prefixed_screaming_snake_key_is_redacted() {
        // STRIPE_SECRET_KEY처럼 키워드 앞에 접두어가 붙은 SCREAMING_SNAKE도 매치돼야 한다(그룹2 접두 확장).
        let text = format!("STRIPE_SECRET_KEY=sk_live_{}", "a".repeat(20));
        assert_eq!(scrub_text(&text), "STRIPE_SECRET_KEY=[REDACTED:generic]");
    }

    #[test]
    fn generic_kv_json_form_with_quoted_key_and_value_is_redacted() {
        // "api_key": "value" 형태(키의 닫는 따옴표 + 값의 여는/닫는 따옴표)도 매치돼야 한다(그룹3 확장).
        let text = r#"{"api_key": "abcdefghijklmnop"}"#;
        assert_eq!(scrub_text(text), r#"{"api_key": [REDACTED:generic]}"#);
    }

    #[test]
    fn generic_kv_prefixed_access_token_env_var_is_redacted() {
        let text = format!("export MY_ACCESS_TOKEN={}", "b".repeat(24));
        assert_eq!(scrub_text(&text), "export MY_ACCESS_TOKEN=[REDACTED:generic]");
    }

    #[test]
    fn generic_kv_value_preserves_trailing_backtick_and_paren() {
        // 값 문자클래스가 백틱/닫는 괄호를 제외해 트레일링 구두점이 값에 섞이지 않고 보존돼야 한다.
        let text = "설명(`password=examplevalue`)";
        assert_eq!(scrub_text(text), "설명(`password=[REDACTED:generic]`)");
    }

    #[test]
    fn generic_kv_does_not_double_redact_already_redacted_brand_prefixed_env_vars() {
        // ANTHROPIC_API_KEY/OPENAI_API_KEY처럼 브랜드 접두어 + api_key 형태는 anthropic/openai
        // 패턴이 먼저 값을 [REDACTED:...]로 치환하는데, 그룹2 접두 확장으로 인해 generic_kv가
        // 이 마커를 다시 "값"으로 오인해 재치환하면 안 된다(값 문자클래스의 `[` 제외로 방지).
        let anthropic_key = format!("sk-ant-{}", "A".repeat(25));
        let anthropic_text = format!("ANTHROPIC_API_KEY={anthropic_key}");
        assert_eq!(scrub_text(&anthropic_text), "ANTHROPIC_API_KEY=[REDACTED:anthropic]");

        let openai_key = format!("sk-{}", "B".repeat(25));
        let openai_text = format!("OPENAI_API_KEY={openai_key}");
        assert_eq!(scrub_text(&openai_text), "OPENAI_API_KEY=[REDACTED:openai]");
    }

    // ── false positive 최소화 ────────────────────────────────────

    #[test]
    fn generic_kv_short_value_under_8_chars_is_not_redacted() {
        let text = "api_key=short12"; // 7자, 최소 8자 미달
        assert_eq!(scrub_text(text), text);
    }

    #[test]
    fn normal_korean_text_is_unchanged_and_borrowed() {
        let text = "오늘 회의 결과를 정리해서 팀에 공유했습니다";
        let out = scrub_text(text);
        assert_eq!(out, text);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn normal_code_snippet_without_secrets_is_unchanged() {
        let text = "fn main() {\n    println!(\"hello, world\");\n}";
        let out = scrub_text(text);
        assert_eq!(out, text);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    // ── 다중 시크릿 혼재 ──────────────────────────────────────────

    #[test]
    fn multiple_secrets_mixed_in_one_text_are_all_redacted() {
        let github_token = format!("ghp_{}", "b".repeat(36));
        let text = format!(
            "AWS_KEY=AKIAABCDEFGHIJKLMNOP\nGITHUB_TOKEN={github_token}\npassword: \"anothersecret1\""
        );
        let out = scrub_text(&text);
        assert_eq!(
            out,
            "AWS_KEY=[REDACTED:aws]\nGITHUB_TOKEN=[REDACTED:github]\npassword: [REDACTED:generic]"
        );
    }

    // ── scrub_request(IngestRequest 통합) ───────────────────────

    fn event_with(title: Option<&str>, body: Option<&str>, metadata: Option<Value>) -> EventInput {
        EventInput {
            external_id: "ext-1".to_string(),
            stream_id: "claude_code:sess-1".to_string(),
            ts: 0,
            source: "claude_code".to_string(),
            event_type: "tool_use".to_string(),
            title: title.map(str::to_string),
            body: body.map(str::to_string),
            model: None,
            tokens_in: None,
            tokens_out: None,
            url: None,
            parent_id: None,
            metadata,
        }
    }

    #[test]
    fn scrub_request_scrubs_stream_title_and_event_title_body() {
        let mut req = IngestRequest {
            stream: Some(StreamInput {
                id: "claude_code:sess-1".to_string(),
                source: "claude_code".to_string(),
                kind: None,
                title: Some("AKIAABCDEFGHIJKLMNOP 유출됨".to_string()),
                project: None,
                git_branch: None,
                started_at: None,
                ended_at: None,
                status: None,
                metadata: None,
            }),
            events: vec![event_with(
                Some("ghp_secret_in_title"),
                Some("password: \"supersecretvalue123\""),
                None,
            )],
        };

        scrub_request(&mut req);

        assert_eq!(req.stream.unwrap().title.as_deref(), Some("[REDACTED:aws] 유출됨"));
        let event = &req.events[0];
        assert_eq!(event.title.as_deref(), Some("ghp_secret_in_title")); // 시크릿 패턴 아님(그대로 유지)
        assert_eq!(event.body.as_deref(), Some("password: [REDACTED:generic]"));
    }

    #[test]
    fn scrub_request_only_scrubs_input_preview_key_in_metadata() {
        let key = format!("sk-ant-{}", "C".repeat(25));
        let mut req = IngestRequest {
            stream: None,
            events: vec![event_with(
                None,
                None,
                Some(json!({
                    "inputPreview": format!("command --token {key}"),
                    "status": "success",
                })),
            )],
        };

        scrub_request(&mut req);

        let metadata = req.events[0].metadata.as_ref().unwrap();
        assert_eq!(metadata.get("inputPreview").unwrap(), "command --token [REDACTED:anthropic]");
        // inputPreview 외 다른 메타 키는 스크럽 대상이 아니다.
        assert_eq!(metadata.get("status").unwrap(), "success");
    }

    #[test]
    fn scrub_request_does_not_touch_full_input_object_in_metadata() {
        let mut req = IngestRequest {
            stream: None,
            events: vec![event_with(
                None,
                None,
                Some(json!({ "input": { "command": "echo AKIAABCDEFGHIJKLMNOP" } })),
            )],
        };

        scrub_request(&mut req);

        // full 정책의 원본 input object는 이번 스코프(inputPreview만) 밖 — 그대로 유지.
        let metadata = req.events[0].metadata.as_ref().unwrap();
        assert_eq!(
            metadata.get("input").unwrap().get("command").unwrap(),
            "echo AKIAABCDEFGHIJKLMNOP"
        );
    }
}
