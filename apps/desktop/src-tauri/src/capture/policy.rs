//! essential/full body 저장 정책 공용 함수(ADR-0012, docs/07-decisions.md).
//! normalize.rs(Claude Code)와 kiro.rs(Kiro CLI) 양쪽 정규화 모듈이 그대로 참조한다
//! (구현이 3중으로 복붙돼 있던 것을 여기 하나로 모음).

use crate::capture::config::BodyPolicy;
use serde_json::Value;
use std::path::Path;

/// essential 정책에서 `tool_result` body를 저장할 최대 문자(코드포인트) 수(ADR-0012).
pub const TOOL_RESULT_MAX_CHARS: usize = 256;
/// essential 정책에서 `tool_use` metadata의 `inputPreview` 최대 문자(코드포인트) 수(ADR-0012).
pub const TOOL_USE_INPUT_PREVIEW_MAX_CHARS: usize = 512;

/// 슬래시 커맨드 메타/중단 마커 텍스트 블록 판정(시스템 노이즈 prompt 필터).
/// trim한 텍스트가 이 프리픽스들로 **시작**하면 메타로 간주해 prompt 이벤트 생성에서 skip한다.
/// (`isMeta:true` 라인 자체는 이 함수 호출 전 normalize.rs에서 라인 레벨로 이미 skip됨 — 여기는
/// 텍스트 블록 레벨 보완: 배열 content 중 일부 블록만 메타인 경우를 잡는다.)
///
/// `migrations/V3__prune_meta_prompts.sql` / `migrations/V4__prune_injected_prompts.sql`의
/// `body LIKE '<pattern>%'` 조건과 반드시 같은 패턴 집합을 유지한다
/// (신규 캡처 필터 ↔ 기존 데이터 소급 정리 정합).
const META_PROMPT_PREFIXES: &[&str] = &[
    // V3(migrations/V3__prune_meta_prompts.sql): 슬래시 커맨드 메타/중단 마커.
    "<command-name>",
    "<command-message>",
    "<command-args>",
    "<local-command-stdout>",
    "<local-command-caveat>",
    "[Request interrupted by user",
    // V4(migrations/V4__prune_injected_prompts.sql): 시스템 주입 prompt 변종
    // (백그라운드 에이전트 완료 알림 등). `[Image:`로 시작하는 사용자 입력(첨부 이미지)은
    // 이 목록에 포함하지 않는다 — 필터 대상 아님.
    "<task-notification>",
    "<teammate-message",
    "<fork-boilerplate>",
    "[structured-output-enforce]",
    "[SYSTEM NOTIFICATION",
    "<ide_opened_file>",
];

pub fn is_meta_prompt_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    META_PROMPT_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// `text`를 최대 `max_chars` 코드포인트로 절단한다(char 단위 순회라 멀티바이트 경계 안전).
/// 초과분이 있으면 말미에 `…`를 붙인다.
pub fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// `tool_result` body에 저장 정책을 적용한다: essential이면 [`TOOL_RESULT_MAX_CHARS`]로 절단, full이면 원문 그대로.
pub fn apply_tool_result_policy(body: &str, policy: BodyPolicy) -> String {
    match policy {
        BodyPolicy::Essential => truncate_with_ellipsis(body, TOOL_RESULT_MAX_CHARS),
        BodyPolicy::Full => body.to_string(),
    }
}

/// `tool_use.input`을 metadata로 감싼다. essential 정책이면 문자열화 후 512자로 절단한 `inputPreview`만
/// 남기고(원문 전체는 버림), full이면 원문 그대로 `input` 키에 보존한다.
pub fn tool_use_metadata(input: Value, policy: BodyPolicy) -> Value {
    let mut m = serde_json::Map::new();
    match policy {
        BodyPolicy::Full => {
            m.insert("input".to_string(), input);
        }
        BodyPolicy::Essential => {
            let preview = truncate_with_ellipsis(&input.to_string(), TOOL_USE_INPUT_PREVIEW_MAX_CHARS);
            m.insert("inputPreview".to_string(), Value::String(preview));
        }
    }
    Value::Object(m)
}

/// 스트림 제목으로 채택할 최소 "의미 있는" prompt 길이(trim 후 코드포인트 수). "ㄱㄱ" 같은
/// 응답성 첫 프롬프트가 그대로 제목이 되는 것을 막는다(실측: session 111개 중 55개가 첫 prompt
/// 앞 60자 제목이었고 일부가 이런 응답성 문구였음).
pub const MEANINGFUL_PROMPT_MIN_CHARS: usize = 15;
/// 스트림 제목 최대 문자(코드포인트) 수(기존 60자 하드컷 유지).
pub const STREAM_TITLE_MAX_CHARS: usize = 60;

/// 스트림 제목 후보 텍스트에서 선두에 반복되는 마크다운 프리픽스(`#`, `>`, `-`, `*`)와 공백을 strip한다.
fn strip_leading_markdown(text: &str) -> &str {
    text.trim_start_matches(|c: char| matches!(c, '#' | '>' | '-' | '*') || c.is_whitespace())
}

/// prompt 원문 한 건을 스트림 제목 후보로 정리한다:
/// 첫 줄만(첫 개행 경계) 추출 → 선두 마크다운 프리픽스 strip → trim → [`STREAM_TITLE_MAX_CHARS`] 초과 시 절단.
fn clean_stream_title_candidate(text: &str) -> String {
    let first_line = text.split('\n').next().unwrap_or("");
    let trimmed = strip_leading_markdown(first_line).trim();
    truncate_with_ellipsis(trimmed, STREAM_TITLE_MAX_CHARS)
}

/// 스트림 제목을 prompt 본문들(ts 순서)에서 산출한다(ai-title/메타 title이 없을 때 폴백 경로).
/// 순서대로 스캔해 trim 후 [`MEANINGFUL_PROMPT_MIN_CHARS`] 이상인 **첫 "의미 있는" prompt**를 채택하고,
/// 그런 prompt가 없으면 첫 prompt로 폴백한다(그마저 없으면 `None`). 채택된 prompt는
/// [`clean_stream_title_candidate`]로 정리한다.
pub fn derive_stream_title<'a>(prompt_bodies: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut first: Option<&str> = None;
    for body in prompt_bodies {
        if first.is_none() {
            first = Some(body);
        }
        if body.trim().chars().count() >= MEANINGFUL_PROMPT_MIN_CHARS {
            return Some(clean_stream_title_candidate(body));
        }
    }
    first.map(clean_stream_title_candidate)
}

/// `project`(정규화된 레포/작업 디렉토리 절대경로)가 일일 AI 요약 CLI 백엔드(M7-①, ADR-0016)의
/// 고정 작업 디렉토리(`~/.logroom/summarizer`)인지 판정한다. `claude -p` 호출이 이 디렉토리에서
/// 실행되므로(engine.rs), 그대로 두면 요약 호출 자체가 `~/.claude*` 감시 대상에 새 세션으로 잡혀
/// "요약이 요약을 부르는" 자기 캡처 루프가 생긴다(실측 확인, ADR-0016 "자기 캡처 루프" 참고).
/// `capture/normalize.rs::normalize_lines`가 정규화 단계에서 이 함수로 판정해 해당 프로젝트의
/// 스트림/이벤트를 통째로 skip한다. 경로 컴포넌트 단위(`Path::ends_with`) 비교라 심볼릭 링크 등으로
/// 표기가 달라져도(`/Users/x/.logroom/summarizer`, `C:\...` 등) 마지막 두 세그먼트만 맞으면 매치된다.
pub fn is_summarizer_project(project: &str) -> bool {
    Path::new(project).ends_with(Path::new(".logroom").join("summarizer"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_with_ellipsis_keeps_short_text_unchanged() {
        assert_eq!(truncate_with_ellipsis("짧은 텍스트", 256), "짧은 텍스트");
    }

    #[test]
    fn truncate_with_ellipsis_exact_boundary_not_truncated() {
        let text: String = "x".repeat(256);
        let out = truncate_with_ellipsis(&text, 256);
        assert_eq!(out.chars().count(), 256);
        assert!(!out.ends_with('…'));
    }

    #[test]
    fn truncate_with_ellipsis_over_boundary_appends_ellipsis() {
        let text: String = "x".repeat(257);
        let out = truncate_with_ellipsis(&text, 256);
        assert_eq!(out.chars().count(), 257); // 256 + '…'
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().take(256).collect::<String>(), "x".repeat(256));
    }

    #[test]
    fn truncate_with_ellipsis_multibyte_boundary_safe() {
        // 한글(멀티바이트)로 300자 입력 → 256자로 절단해도 문자 경계가 깨지지 않아야 함(패닉 없음).
        let text: String = "가".repeat(300);
        let out = truncate_with_ellipsis(&text, 256);
        assert_eq!(out.chars().count(), 257);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().take(256).collect::<String>(), "가".repeat(256));
    }

    #[test]
    fn apply_tool_result_policy_truncates_under_essential() {
        let long = "y".repeat(300);
        let out = apply_tool_result_policy(&long, BodyPolicy::Essential);
        assert_eq!(out.chars().count(), 257);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn apply_tool_result_policy_keeps_full_body_under_full_policy() {
        let long = "y".repeat(300);
        let out = apply_tool_result_policy(&long, BodyPolicy::Full);
        assert_eq!(out, long);
    }

    #[test]
    fn tool_use_metadata_essential_replaces_input_with_truncated_preview() {
        let input = json!({ "command": "z".repeat(600) });
        let metadata = tool_use_metadata(input, BodyPolicy::Essential);
        assert!(metadata.get("input").is_none());
        let preview = metadata.get("inputPreview").and_then(Value::as_str).expect("inputPreview 있어야 함");
        assert!(preview.chars().count() <= 513); // 512 + '…'
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn tool_use_metadata_full_keeps_original_input() {
        let input = json!({ "command": "ls -la" });
        let metadata = tool_use_metadata(input.clone(), BodyPolicy::Full);
        assert_eq!(metadata.get("input"), Some(&input));
        assert!(metadata.get("inputPreview").is_none());
    }

    #[test]
    fn is_meta_prompt_text_detects_each_known_pattern() {
        assert!(is_meta_prompt_text("<command-name>/model</command-name>"));
        assert!(is_meta_prompt_text("<command-message>model</command-message>"));
        assert!(is_meta_prompt_text("<command-args></command-args>"));
        assert!(is_meta_prompt_text(
            "<local-command-stdout>Set model to Opus</local-command-stdout>"
        ));
        assert!(is_meta_prompt_text(
            "<local-command-caveat>Caveat: the messages below…</local-command-caveat>"
        ));
        assert!(is_meta_prompt_text("[Request interrupted by user]"));
        assert!(is_meta_prompt_text("[Request interrupted by user for tool use]"));
    }

    #[test]
    fn is_meta_prompt_text_ignores_leading_whitespace() {
        assert!(is_meta_prompt_text("  \n<command-name>/model</command-name>"));
    }

    #[test]
    fn is_meta_prompt_text_false_for_normal_prompt() {
        assert!(!is_meta_prompt_text("안녕하세요 도와주세요"));
        assert!(!is_meta_prompt_text(
            "이 커맨드에 대해 설명해줘 <command-name> 아님"
        ));
    }

    #[test]
    fn is_meta_prompt_text_detects_each_injected_prompt_pattern() {
        assert!(is_meta_prompt_text(
            "<task-notification>agent-a1 완료</task-notification>"
        ));
        assert!(is_meta_prompt_text(
            "<teammate-message from=\"agent-b\">안녕</teammate-message>"
        ));
        assert!(is_meta_prompt_text("<fork-boilerplate>표준 안내문…</fork-boilerplate>"));
        assert!(is_meta_prompt_text("[structured-output-enforce] JSON으로만 응답하세요"));
        assert!(is_meta_prompt_text(
            "[SYSTEM NOTIFICATION - NOT USER INPUT] 백그라운드 작업이 완료됐습니다"
        ));
        assert!(is_meta_prompt_text("<ide_opened_file>src/main.rs</ide_opened_file>"));
    }

    #[test]
    fn is_meta_prompt_text_false_for_user_image_attachment_and_normal_labels() {
        // `[Image: ...`는 사용자가 첨부한 이미지 프롬프트이므로 필터 대상이 아니다.
        assert!(!is_meta_prompt_text("[Image: 스크린샷.png] 이 화면 좀 봐줘"));
        assert!(!is_meta_prompt_text("[FE] 로그인 페이지 버튼 정렬 확인해줘"));
    }

    // ── derive_stream_title(스트림 제목 휴리스틱) ────────────────────

    #[test]
    fn derive_stream_title_skips_short_first_prompt_and_picks_second() {
        let bodies = ["ㄱㄱ", "이 프로젝트 구조를 좀 설명해 주실 수 있을까요"];
        let title = derive_stream_title(bodies.into_iter()).expect("제목 있어야 함");
        assert_eq!(title, "이 프로젝트 구조를 좀 설명해 주실 수 있을까요");
    }

    #[test]
    fn derive_stream_title_falls_back_to_first_when_all_short() {
        let bodies = ["ㄱㄱ", "ㄴㄴ"];
        let title = derive_stream_title(bodies.into_iter()).expect("제목 있어야 함");
        assert_eq!(title, "ㄱㄱ");
    }

    #[test]
    fn derive_stream_title_none_when_no_prompts() {
        let bodies: [&str; 0] = [];
        assert_eq!(derive_stream_title(bodies.into_iter()), None);
    }

    #[test]
    fn derive_stream_title_uses_first_line_only() {
        let bodies = ["첫 줄입니다 여기까지만 제목\n둘째 줄은 무시돼야 함"];
        let title = derive_stream_title(bodies.into_iter()).expect("제목 있어야 함");
        assert_eq!(title, "첫 줄입니다 여기까지만 제목");
    }

    #[test]
    fn derive_stream_title_strips_leading_markdown_prefix() {
        let bodies = ["## 제목 스타일 프롬프트 정리 테스트"];
        let title = derive_stream_title(bodies.into_iter()).expect("제목 있어야 함");
        assert_eq!(title, "제목 스타일 프롬프트 정리 테스트");
    }

    #[test]
    fn derive_stream_title_truncates_over_60_chars() {
        let long = "x".repeat(70);
        let bodies = [long.as_str()];
        let title = derive_stream_title(bodies.into_iter()).expect("제목 있어야 함");
        assert_eq!(title.chars().count(), 61); // 60 + '…'
        assert!(title.ends_with('…'));
        assert_eq!(title.chars().take(60).collect::<String>(), "x".repeat(60));
    }

    #[test]
    fn derive_stream_title_truncates_korean_boundary_safely() {
        let long = "가".repeat(70);
        let bodies = [long.as_str()];
        let title = derive_stream_title(bodies.into_iter()).expect("제목 있어야 함");
        assert_eq!(title.chars().count(), 61);
        assert!(title.ends_with('…'));
        assert_eq!(title.chars().take(60).collect::<String>(), "가".repeat(60));
    }

    // ── is_summarizer_project(자기 캡처 루프 차단, ADR-0016) ────────────

    #[test]
    fn is_summarizer_project_matches_home_summarizer_dir() {
        assert!(is_summarizer_project("/Users/me/.logroom/summarizer"));
    }

    #[test]
    fn is_summarizer_project_false_for_unrelated_project() {
        assert!(!is_summarizer_project("/Users/me/src/logroom"));
    }

    #[test]
    fn is_summarizer_project_false_for_partial_suffix_match() {
        // "summarizer"만 일치하고 상위가 ".logroom"이 아니면 매치되면 안 된다.
        assert!(!is_summarizer_project("/Users/me/projects/summarizer"));
    }

    #[test]
    fn is_summarizer_project_false_when_summarizer_is_not_last_segment() {
        assert!(!is_summarizer_project("/Users/me/.logroom/summarizer/nested"));
    }
}
