//! 일일 AI 요약 멀티 프로바이더 백엔드(M7-①, ADR-0016) — 모드(auto/cli/api, `SummaryEngine`) ×
//! 프로바이더(CLI: claude/gemini/codex, API: anthropic/openai/gemini)를 조합해 실제 생성을
//! 수행한다. `capture/config.rs::SummaryConfig`의 `engine`/`cliProvider`/`apiProvider`에 따라
//! 백엔드를 고른다. 실제 CLI 실행/HTTP 호출은 네트워크·외부 프로세스 의존이라 단위테스트 대상이
//! 아니다(완료 보고 "수동 확인 포인트" 참고) — 인자 조립([`cli_args`])/응답 파싱(`parse_*_response`)/
//! 엔진 선택([`decide_engine`])은 순수함수로 분리해 그 부분만 테스트한다.

use crate::capture::config::{ApiProvider, CliProvider, SummaryConfig, SummaryEngine};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// CLI 실행 타임아웃(초) — claude/gemini/codex 공통. 180초였을 때 주간 요약이 계속 끊겼다(opus 기준
/// 180초에 출력 약 15k 토큰이 한계인데, 이슈가 많은 주는 그보다 길다 — 성공한 일간도 160초대까지
/// 올라와 있었다). 요약 1건이 몇 분 걸리는 건 정상 범위라 넉넉히 둔다.
const CLI_TIMEOUT_SECS: u64 = 600;
/// API 호출 타임아웃(초) — anthropic/openai/gemini 공통.
const API_TIMEOUT_SECS: u64 = 120;
/// Anthropic Messages API 응답 `max_tokens`(OpenAI/Gemini는 별도 상한 파라미터를 넘기지 않는다 —
/// 프로바이더별 기본 상한에 맡긴다, 실사용 요구 없어 단순화).
const API_MAX_TOKENS: u32 = 1024;
const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const OPENAI_CHAT_COMPLETIONS_URL: &str = "https://api.openai.com/v1/chat/completions";
/// Gemini `generateContent` 엔드포인트 베이스 — 실제 URL은 `{base}/{model}:generateContent`.
const GEMINI_API_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

/// 실제로 사용된(= "auto"가 해석된 뒤의) 엔진 + 프로바이더.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedEngine {
    Cli(CliProvider),
    Api(ApiProvider),
}

impl ResolvedEngine {
    /// `daily_summaries.engine`에 저장되는 문자열 — `"cli:claude"`/`"api:openai"` 형태로 세분화한다
    /// (캐시에 실제 어떤 엔진으로 만들었는지 남도록). 기존 저장값(`"cli"`/`"api"`, 단일 프로바이더
    /// 시절)은 형식이 다르지만 표시 전용 문자열이라 그대로 둔다 — 마이그레이션 불필요.
    pub fn as_str(self) -> String {
        match self {
            ResolvedEngine::Cli(p) => format!("cli:{}", cli_binary_name(p)),
            ResolvedEngine::Api(p) => format!("api:{}", api_provider_name(p)),
        }
    }
}

/// 구분 가능한 실패 상태는 **안정 에러 코드**로 반환한다(리뷰 지적 — 로케일 하드코딩 문자열 대신).
/// FE(DailySummaryCard)가 코드를 i18n 키로 매핑해 언어별 안내를 보여주고, 미지의 코드/그 외
/// 런타임 에러는 일반 오류 문구로 폴백한다. 코드 목록은 lib.rs 커맨드의 `summary_disabled`/
/// `summary_paused`, excerpt.rs의 `summary_no_data`와 한 집합.
fn no_engine_available_message() -> &'static str {
    "summary_no_engine"
}

fn api_key_missing_message() -> &'static str {
    "summary_api_key_missing"
}

/// CLI가 [`CLI_TIMEOUT_SECS`] 안에 끝나지 않은 경우. 일반 오류 문구로 폴백되면 사용자는 "왜" 실패하는지
/// 알 수 없다 — 주간 요약이 시간 초과로 일주일 넘게 실패했는데 화면에는 "요약 생성에 실패했습니다"만 떴다.
fn cli_timeout_message() -> &'static str {
    "summary_timeout"
}

/// CLI 3종의 `--version` 감지 결과(auto 모드는 claude→gemini→codex 순서로 확인한다).
/// `Serialize`는 온보딩/설정 UI가 "무엇이 감지됐는지"를 그대로 보여주기 위한 것이다
/// (lib.rs::detect_summary_engine).
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct CliDetected {
    pub claude: bool,
    pub gemini: bool,
    pub codex: bool,
}

/// API 3종의 키 보유 여부(auto 모드는 anthropic→openai→gemini 순서로 확인한다).
/// 키 **원문이 아니라 보유 여부(bool)만** 담으므로 FE로 내보내도 시크릿이 새지 않는다.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ApiKeysPresent {
    pub anthropic: bool,
    pub openai: bool,
    pub gemini: bool,
}

impl ApiKeysPresent {
    /// 설정에 저장된 3개 키의 보유 여부를 판정한다. 공백만 있는 값은 미보유로 본다
    /// (`generate`와 `detect_summary_engine`이 같은 기준을 쓰도록 여기 한 곳에만 둔다).
    pub fn from_config(cfg: &SummaryConfig) -> Self {
        let present = |k: &Option<String>| k.as_deref().is_some_and(|k| !k.trim().is_empty());
        Self {
            anthropic: present(&cfg.anthropic_api_key),
            openai: present(&cfg.openai_api_key),
            gemini: present(&cfg.gemini_api_key),
        }
    }

    fn has(self, provider: ApiProvider) -> bool {
        match provider {
            ApiProvider::Anthropic => self.anthropic,
            ApiProvider::Openai => self.openai,
            ApiProvider::Gemini => self.gemini,
        }
    }
}

/// 엔진 선택 순수 결정 로직(mode × 프로바이더 매트릭스) — 실제 CLI 감지 결과(`cli_detected`)와
/// API 키 보유 여부(`api_keys`)만 입력받아 어떤 백엔드를 쓸지 결정한다. 네트워크/프로세스 실행과
/// 분리해 그 자체는 단위테스트 가능하다.
///
/// - `mode = Cli`: 사용자가 선택한 `cli_provider`를 그대로 쓴다(감지 실패해도 PATH 이름으로 시도하는
///   건 호출부 책임 — 기존 claude 단일 프로바이더 시절 관례를 유지).
/// - `mode = Api`: 사용자가 선택한 `api_provider`의 키가 없으면 에러.
/// - `mode = Auto`: CLI 3종을 claude→gemini→codex 순서로, 하나도 없으면 API 키를
///   anthropic→openai→gemini 순서로 확인한다. 둘 다 없으면 에러.
pub fn decide_engine(
    mode: SummaryEngine,
    cli_provider: CliProvider,
    api_provider: ApiProvider,
    cli_detected: CliDetected,
    api_keys: ApiKeysPresent,
) -> Result<ResolvedEngine, String> {
    match mode {
        SummaryEngine::Cli => Ok(ResolvedEngine::Cli(cli_provider)),
        SummaryEngine::Api => {
            if api_keys.has(api_provider) {
                Ok(ResolvedEngine::Api(api_provider))
            } else {
                Err(api_key_missing_message().to_string())
            }
        }
        SummaryEngine::Auto => {
            if cli_detected.claude {
                Ok(ResolvedEngine::Cli(CliProvider::Claude))
            } else if cli_detected.gemini {
                Ok(ResolvedEngine::Cli(CliProvider::Gemini))
            } else if cli_detected.codex {
                Ok(ResolvedEngine::Cli(CliProvider::Codex))
            } else if api_keys.anthropic {
                Ok(ResolvedEngine::Api(ApiProvider::Anthropic))
            } else if api_keys.openai {
                Ok(ResolvedEngine::Api(ApiProvider::Openai))
            } else if api_keys.gemini {
                Ok(ResolvedEngine::Api(ApiProvider::Gemini))
            } else {
                Err(no_engine_available_message().to_string())
            }
        }
    }
}

/// 일일 AI 요약 CLI 백엔드의 고정 작업 디렉토리(`~/.logroom/summarizer`). 없으면 생성한다
/// (ADR-0016 "자기 캡처 루프" 차단 — cwd 고정 + capture/policy.rs::is_summarizer_project 병행).
fn summarizer_dir() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home_dir 없음"))?;
    let dir = home.join(".logroom").join("summarizer");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// PATH 상의 실행 파일명이자 [`ResolvedEngine::as_str`]가 쓰는 라벨.
fn cli_binary_name(provider: CliProvider) -> &'static str {
    match provider {
        CliProvider::Claude => "claude",
        CliProvider::Gemini => "gemini",
        CliProvider::Codex => "codex",
    }
}

fn api_provider_name(provider: ApiProvider) -> &'static str {
    match provider {
        ApiProvider::Anthropic => "anthropic",
        ApiProvider::Openai => "openai",
        ApiProvider::Gemini => "gemini",
    }
}

/// PATH 외에 CLI 바이너리를 추가로 찾아볼 후보 경로들 — Finder/launchd로 실행된 패키징 앱은 셸
/// PATH를 물려받지 않아 npm 전역/homebrew/네이티브 설치 경로가 안 잡힌다(패키징 앱 대비). 기존
/// `claude` 전용 후보 목록과 동일한 패턴을 프로바이더별 바이너리명에 그대로 적용한다.
fn cli_binary_candidates(provider: CliProvider) -> Vec<PathBuf> {
    let name = cli_binary_name(provider);
    let mut candidates = vec![PathBuf::from(name)]; // PATH 우선(dev 모드·터미널 실행)
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/bin").join(name)); // 네이티브 인스톨러
        candidates.push(home.join(".npm-global/bin").join(name));
        // nvm으로 설치한 npm 전역 CLI(실측 — 사용자 claude가 ~/.nvm/versions/node/v24.x/bin에
        // 있었음). 버전 디렉토리가 가변이라 글롭으로 존재하는 버전들을 전부 후보에 넣는다
        // (패키징 앱은 셸 PATH를 안 물려받아 이 경로를 알 수 없음 — Finder 실행 대비).
        let nvm_versions = home.join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(&nvm_versions) {
            let mut versions: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path().join("bin").join(name))
                .collect();
            // 최신 버전 우선 시도(사전순 내림차순 — 정확한 semver 정렬은 아니지만 첫 성공만
            // 쓰므로 순서는 최적화일 뿐 정확성에 영향 없음).
            versions.sort();
            versions.reverse();
            candidates.extend(versions);
        }
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin").join(name));
    candidates.push(PathBuf::from("/usr/local/bin").join(name));
    candidates
}

/// CLI 3종을 **병렬** 감지한다 — 온보딩/설정 UI가 "무엇이 깔려 있는지"를 보여주기 위한 것으로,
/// [`generate`]의 auto 경로와 동일한 후보 목록·판정([`detect_cli_binary`])을 쓴다. 순차로 돌면
/// 미설치 CLI마다 후보 경로를 전부 시도해 지연이 누적되므로 `generate`와 같이 `join!`을 쓴다.
pub async fn detect_all_cli() -> CliDetected {
    let (claude, gemini, codex) = tokio::join!(
        detect_cli_binary(CliProvider::Claude),
        detect_cli_binary(CliProvider::Gemini),
        detect_cli_binary(CliProvider::Codex),
    );
    CliDetected { claude: claude.is_some(), gemini: gemini.is_some(), codex: codex.is_some() }
}

/// 후보 경로를 순서대로 `--version` 실행해 첫 성공 바이너리를 반환한다(네트워크 없음, 로컬 실행만).
/// 감지(auto 판정)와 실제 실행이 같은 경로를 쓰도록 결과를 호출부에 넘긴다.
async fn detect_cli_binary(provider: CliProvider) -> Option<PathBuf> {
    for candidate in cli_binary_candidates(provider) {
        let ok = tokio::process::Command::new(&candidate)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|status| status.success())
            .unwrap_or(false);
        if ok {
            return Some(candidate);
        }
    }
    None
}

/// 설정의 `cliConfigDir`(선택)을 실제 주입할 경로로 해석한다 — 선두 `~/`는 홈으로 확장, 빈 값은
/// `None`(CLI 기본 `~/.claude` 동작 유지). 셸 alias 로 CLAUDE_CONFIG_DIR 을 나눠 쓰는 사용자 대응.
/// Claude CLI 전용 설정이라 [`generate_via_cli`]가 `provider == CliProvider::Claude`일 때만 호출한다.
fn resolve_cli_config_dir(raw: Option<&str>) -> Option<PathBuf> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return dirs::home_dir().map(|home| home.join(rest));
    }
    Some(PathBuf::from(trimmed))
}

/// cliModel 설정값(자유 텍스트) 검증 — `-`로 시작하면 외부 CLI의 인자 파서가 의도한 플래그의 값이
/// 아니라 별도 플래그로 재해석할 여지가 있다(보안 리뷰 Medium — 플래그 주입 방어). 빈 값도 거부.
/// 3개 CLI 프로바이더 공용(모두 subprocess 인자로 모델명을 받으므로 검증 기준이 동일하다).
fn validate_cli_model(cli_model: &str) -> anyhow::Result<&str> {
    let trimmed = cli_model.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        anyhow::bail!("잘못된 cliModel 값입니다: {trimmed:?}");
    }
    Ok(trimmed)
}

/// CLI 프로바이더별 실행 인자 조립(순수함수). 인자 배열 방식(셸 조립 금지)은 **셸 워드스플리팅/
/// 인젝션**을 막지만, 호출 대상 CLI 자신의 인자 파서가 `-`로 시작하는 값을 옵션으로 재해석하는
/// 것까지 막지는 못한다(리뷰 지적 — 주석 과장 정정). 그래서:
/// - claude/gemini: prompt가 `-p` **플래그의 값 위치**라 파서 재해석 여지가 작다.
/// - codex: prompt가 positional이라 **`--` separator로 옵션 파싱을 명시 종료**한다(보안 리뷰
///   Medium — clap 계열 표준 관례).
///
/// 현재 호출부는 항상 정적 프롬프트(prompts.rs, `-`로 시작하지 않음)를 선두에 결합하지만
/// (`mod.rs`/`period.rs`의 `format!("{prompt}\n\n{excerpt}")` 불변식), 이 함수는 그 불변식에
/// 의존하지 않고 스스로 방어한다.
///
/// - claude: `--model {model} -p {prompt}`
/// - gemini: `-m {model} -p {prompt}`
/// - codex: `exec -m {model} -- {prompt}`
pub fn cli_args(provider: CliProvider, model: &str, prompt: &str) -> Vec<String> {
    match provider {
        CliProvider::Claude => {
            vec!["--model".to_string(), model.to_string(), "-p".to_string(), prompt.to_string()]
        }
        CliProvider::Gemini => {
            vec!["-m".to_string(), model.to_string(), "-p".to_string(), prompt.to_string()]
        }
        CliProvider::Codex => {
            vec![
                "exec".to_string(),
                "-m".to_string(),
                model.to_string(),
                "--".to_string(),
                prompt.to_string(),
            ]
        }
    }
}

/// CLI 프로바이더 1개를 `~/.logroom/summarizer`에서 실행한다. `CLAUDECODE`/`CLAUDE_CODE_ENTRYPOINT`
/// 환경변수를 제거해(중첩 세션으로 오인되는 것 방지) 실행하고, 타임아웃([`CLI_TIMEOUT_SECS`])이 지나면
/// 프로세스를 정리(`kill_on_drop`)하고 [`cli_timeout_message`] 코드로 실패한다([`wait_with_timeout`]).
async fn generate_via_cli(
    binary: &Path,
    provider: CliProvider,
    prompt_and_excerpt: &str,
    cli_model: &str,
    cli_config_dir: Option<&str>,
) -> anyhow::Result<String> {
    let cli_model = validate_cli_model(cli_model)?;
    let dir = summarizer_dir()?;
    let args = cli_args(provider, cli_model, prompt_and_excerpt);

    let mut cmd = tokio::process::Command::new(binary);
    cmd.args(&args)
        .current_dir(&dir)
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 타임아웃으로 future가 드롭돼도 자식 프로세스가 좀비로 남지 않게 한다.
        .kill_on_drop(true);
    // CLAUDE_CONFIG_DIR은 Claude CLI 전용 — 셸 alias 로 CLAUDE_CONFIG_DIR 을 나눠 쓰는 사용자를 위한 것으로,
    // gemini/codex CLI는 이런 개념이 없어 주입하지 않는다.
    if provider == CliProvider::Claude {
        if let Some(config_dir) = resolve_cli_config_dir(cli_config_dir) {
            cmd.env("CLAUDE_CONFIG_DIR", config_dir);
        }
    }

    let label = cli_binary_name(provider);
    let child = cmd.spawn().map_err(|e| anyhow::anyhow!("{label} CLI 실행 실패: {e}"))?;
    let output = wait_with_timeout(child, Duration::from_secs(CLI_TIMEOUT_SECS)).await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let snippet: String = stderr.chars().take(500).collect();
        anyhow::bail!("{label} CLI 실행 실패({}): {snippet}", output.status);
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// 자식 프로세스 종료를 `limit`까지 기다린다. 넘으면 FE가 매핑하는 안정 코드([`cli_timeout_message`])로
/// 실패한다 — 호출부가 `kill_on_drop(true)`로 띄웠다면 여기서 future가 드롭될 때 프로세스도 정리된다.
async fn wait_with_timeout(
    child: tokio::process::Child,
    limit: Duration,
) -> anyhow::Result<std::process::Output> {
    let output = tokio::time::timeout(limit, child.wait_with_output()).await.map_err(|_| {
        eprintln!("[logroom] 요약 CLI가 {}초 안에 끝나지 않아 중단했습니다", limit.as_secs());
        anyhow::anyhow!(cli_timeout_message())
    })??;
    Ok(output)
}

fn api_http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(API_TIMEOUT_SECS))
        .build()
        .map_err(Into::into)
}

/// API 호출 공통 에러 처리 — 4xx/429는 상태코드 + 본문 요약(500자, **키 에코 금지**)으로 에러화한다.
/// anthropic/openai/gemini 3개 호출이 공유.
async fn parse_api_json_response(resp: reqwest::Response, provider_label: &str) -> anyhow::Result<Value> {
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let snippet: String = text.chars().take(500).collect();
        anyhow::bail!("{provider_label} API 호출 실패: HTTP {status} — {snippet}");
    }
    resp.json::<Value>().await.map_err(Into::into)
}

/// Anthropic Messages API(`POST /v1/messages`) 응답에서 텍스트 추출(순수함수, JSON 픽스처로 테스트).
fn parse_anthropic_response(payload: &Value) -> anyhow::Result<String> {
    payload
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Anthropic API 응답에 content[0].text가 없습니다"))
}

/// OpenAI Chat Completions API(`POST /v1/chat/completions`) 응답에서 텍스트 추출.
fn parse_openai_response(payload: &Value) -> anyhow::Result<String> {
    payload
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("OpenAI API 응답에 choices[0].message.content가 없습니다"))
}

/// Gemini `generateContent` API 응답에서 텍스트 추출.
fn parse_gemini_response(payload: &Value) -> anyhow::Result<String> {
    payload
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Gemini API 응답에 candidates[0].content.parts[0].text가 없습니다"))
}

/// Anthropic Messages API 호출. `x-api-key`/`anthropic-version` 헤더 +
/// `messages: [{role: "user", content: prompt_and_excerpt}]` body.
async fn generate_via_anthropic_api(
    prompt_and_excerpt: &str,
    api_key: &str,
    api_model: &str,
) -> anyhow::Result<String> {
    let client = api_http_client()?;
    let body = serde_json::json!({
        "model": api_model,
        "max_tokens": API_MAX_TOKENS,
        "messages": [{ "role": "user", "content": prompt_and_excerpt }],
    });
    let resp = client
        .post(ANTHROPIC_MESSAGES_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_API_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;
    let payload = parse_api_json_response(resp, "Anthropic").await?;
    parse_anthropic_response(&payload)
}

/// OpenAI Chat Completions API 호출. `Authorization: Bearer {key}` 헤더 +
/// `{model, messages: [{role: "user", content: ...}]}` body.
async fn generate_via_openai_api(
    prompt_and_excerpt: &str,
    api_key: &str,
    api_model: &str,
) -> anyhow::Result<String> {
    let client = api_http_client()?;
    let body = serde_json::json!({
        "model": api_model,
        "messages": [{ "role": "user", "content": prompt_and_excerpt }],
    });
    let resp = client
        .post(OPENAI_CHAT_COMPLETIONS_URL)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;
    let payload = parse_api_json_response(resp, "OpenAI").await?;
    parse_openai_response(&payload)
}

/// Gemini `generateContent` API 호출. 키는 **`x-goog-api-key` 헤더로만** 전달한다(URL 쿼리
/// `?key=...`로 넘기면 접근 로그/프록시/리퍼러에 키가 남을 수 있어 보안 리뷰상 헤더 방식 채택).
/// body는 `{contents: [{parts: [{text: ...}]}]}`.
/// URL 경로에 삽입되는 모델명 검증(Gemini 전용) — 설정 자유 텍스트가 `/`·`?`·`#`·공백 등을 담으면
/// 경로/쿼리가 오염돼 요청이 엉뚱한 리소스로 갈 수 있다(보안 리뷰 Low — self-injection 수준이지만
/// 견고성 차원 방어). 허용: 영숫자·`-`·`.`·`_`·`:`.
fn validate_gemini_url_model(model: &str) -> anyhow::Result<&str> {
    let trimmed = model.trim();
    let valid = !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':'));
    if !valid {
        anyhow::bail!("잘못된 Gemini 모델명입니다: {trimmed:?}");
    }
    Ok(trimmed)
}

async fn generate_via_gemini_api(
    prompt_and_excerpt: &str,
    api_key: &str,
    api_model: &str,
) -> anyhow::Result<String> {
    let api_model = validate_gemini_url_model(api_model)?;
    let client = api_http_client()?;
    let url = format!("{GEMINI_API_BASE_URL}/{api_model}:generateContent");
    let body = serde_json::json!({
        "contents": [{ "parts": [{ "text": prompt_and_excerpt }] }],
    });
    let resp = client
        .post(url)
        .header("x-goog-api-key", api_key)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;
    let payload = parse_api_json_response(resp, "Gemini").await?;
    parse_gemini_response(&payload)
}

/// 하루 요약을 생성한다: 엔진 결정([`decide_engine`], CLI 감지는 실제로 `--version` 실행) → 선택된
/// 백엔드로 생성. 반환값의 `engine`은 항상 해석된 값(`"cli:claude"` | `"api:openai"` 등)이며 `"auto"`가
/// 그대로 저장되지 않는다(daily_summaries 캐시 재사용 시 실제로 어떤 백엔드가 만들었는지 알 수 있어야
/// 하므로).
pub async fn generate(prompt_and_excerpt: &str, cfg: &SummaryConfig) -> anyhow::Result<GeneratedSummary> {
    let api_keys = ApiKeysPresent::from_config(cfg);

    // CLI 감지: mode=api 고정이면 어차피 안 쓰이므로 감지 자체를 건너뛴다(네트워크/프로세스 절약,
    // 기존 관례). mode=cli는 선택된 프로바이더 하나만, auto는 3종 모두 감지해 우선순위를 가린다.
    let (claude_binary, gemini_binary, codex_binary) = match cfg.engine {
        SummaryEngine::Api => (None, None, None),
        SummaryEngine::Cli => match cfg.cli_provider {
            CliProvider::Claude => (detect_cli_binary(CliProvider::Claude).await, None, None),
            CliProvider::Gemini => (None, detect_cli_binary(CliProvider::Gemini).await, None),
            CliProvider::Codex => (None, None, detect_cli_binary(CliProvider::Codex).await),
        },
        // auto는 3종을 병렬 감지한다(리뷰 제안) — 순차면 미설치 CLI마다 후보 경로 5개씩 시도가
        // 누적돼 CLI가 하나도 없는 사용자의 매 생성마다 지연이 커진다.
        SummaryEngine::Auto => tokio::join!(
            detect_cli_binary(CliProvider::Claude),
            detect_cli_binary(CliProvider::Gemini),
            detect_cli_binary(CliProvider::Codex),
        ),
    };

    let cli_detected = CliDetected {
        claude: claude_binary.is_some(),
        gemini: gemini_binary.is_some(),
        codex: codex_binary.is_some(),
    };
    let resolved = decide_engine(cfg.engine, cfg.cli_provider, cfg.api_provider, cli_detected, api_keys)
        .map_err(|e| anyhow::anyhow!(e))?;

    match resolved {
        ResolvedEngine::Cli(provider) => {
            // engine=cli 고정인데 감지에 실패한 경우도 PATH 이름으로 시도는 해본다(감지가
            // 보수적으로 실패했을 가능성 — 실행 에러가 더 구체적인 메시지를 준다).
            let detected = match provider {
                CliProvider::Claude => claude_binary,
                CliProvider::Gemini => gemini_binary,
                CliProvider::Codex => codex_binary,
            };
            let binary = detected.unwrap_or_else(|| PathBuf::from(cli_binary_name(provider)));
            let content = generate_via_cli(
                &binary,
                provider,
                prompt_and_excerpt,
                &cfg.cli_model,
                cfg.cli_config_dir.as_deref(),
            )
            .await?;
            Ok(GeneratedSummary { engine: resolved.as_str(), model: cfg.cli_model.clone(), content })
        }
        ResolvedEngine::Api(provider) => {
            let content = match provider {
                ApiProvider::Anthropic => {
                    let api_key = cfg
                        .anthropic_api_key
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!(api_key_missing_message()))?;
                    generate_via_anthropic_api(prompt_and_excerpt, api_key, &cfg.api_model).await?
                }
                ApiProvider::Openai => {
                    let api_key = cfg
                        .openai_api_key
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!(api_key_missing_message()))?;
                    generate_via_openai_api(prompt_and_excerpt, api_key, &cfg.api_model).await?
                }
                ApiProvider::Gemini => {
                    let api_key = cfg
                        .gemini_api_key
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!(api_key_missing_message()))?;
                    generate_via_gemini_api(prompt_and_excerpt, api_key, &cfg.api_model).await?
                }
            };
            Ok(GeneratedSummary { engine: resolved.as_str(), model: cfg.api_model.clone(), content })
        }
    }
}

/// 생성 결과 — `daily_summaries` upsert에 그대로 쓰인다.
pub struct GeneratedSummary {
    pub engine: String,
    pub model: String,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_cli_config_dir / validate_cli_model ────────────────

    #[test]
    fn resolve_cli_config_dir_expands_tilde_and_ignores_empty() {
        assert_eq!(resolve_cli_config_dir(None), None);
        assert_eq!(resolve_cli_config_dir(Some("")), None);
        assert_eq!(resolve_cli_config_dir(Some("   ")), None);
        assert_eq!(
            resolve_cli_config_dir(Some("/abs/dir")),
            Some(PathBuf::from("/abs/dir"))
        );
        let home = dirs::home_dir().expect("테스트 환경에 home 필요");
        assert_eq!(
            resolve_cli_config_dir(Some("~/.claude-b")),
            Some(home.join(".claude-b"))
        );
    }

    #[test]
    fn validate_cli_model_rejects_flag_like_and_empty_values() {
        // 플래그 주입 방어(보안 리뷰): `-`로 시작하거나 빈 값은 subprocess 인자로 싣지 않는다.
        assert!(validate_cli_model("--dangerously-skip-permissions").is_err());
        assert!(validate_cli_model("-x").is_err());
        assert!(validate_cli_model("").is_err());
        assert!(validate_cli_model("   ").is_err());
        assert_eq!(validate_cli_model("sonnet").unwrap(), "sonnet");
        assert_eq!(validate_cli_model("  haiku  ").unwrap(), "haiku");
    }

    // ── wait_with_timeout(CLI 시간 초과 → 안정 에러 코드) ───────────

    #[tokio::test]
    async fn wait_with_timeout_fails_with_timeout_code_when_child_outlives_limit() {
        // 시간 초과는 일반 오류 문구가 아니라 FE가 매핑하는 코드여야 한다(SummaryView SUMMARY_ERROR_KEYS).
        let child = tokio::process::Command::new("sleep")
            .arg("5")
            .kill_on_drop(true)
            .spawn()
            .expect("sleep 실행");
        let err = wait_with_timeout(child, Duration::from_millis(50)).await.unwrap_err();
        assert_eq!(err.to_string(), "summary_timeout");
    }

    #[tokio::test]
    async fn wait_with_timeout_returns_output_when_child_finishes_within_limit() {
        let child = tokio::process::Command::new("echo")
            .arg("ok")
            .stdout(Stdio::piped())
            .spawn()
            .expect("echo 실행");
        let output = wait_with_timeout(child, Duration::from_secs(5)).await.expect("제한 안에 종료");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
    }

    // ── cli_args(프로바이더별 인자 조립, 순수함수) ──────────────────

    #[test]
    fn cli_args_claude_uses_model_flag_and_dash_p() {
        assert_eq!(
            cli_args(CliProvider::Claude, "sonnet", "hello"),
            vec!["--model", "sonnet", "-p", "hello"]
        );
    }

    #[test]
    fn cli_args_gemini_uses_short_flags() {
        assert_eq!(
            cli_args(CliProvider::Gemini, "gemini-2.5-flash", "hello"),
            vec!["-m", "gemini-2.5-flash", "-p", "hello"]
        );
    }

    #[test]
    fn validate_gemini_url_model_rejects_path_polluting_chars() {
        assert!(validate_gemini_url_model("gemini-2.5-flash").is_ok());
        assert!(validate_gemini_url_model("models/x").is_err()); // '/' — 경로 오염
        assert!(validate_gemini_url_model("x?key=1").is_err()); // '?' — 쿼리 주입
        assert!(validate_gemini_url_model("x#frag").is_err());
        assert!(validate_gemini_url_model("a b").is_err());
        assert!(validate_gemini_url_model("").is_err());
    }

    #[test]
    fn cli_args_codex_uses_exec_subcommand_with_double_dash_separator() {
        // positional prompt 앞에 `--`로 옵션 파싱을 종료한다(보안 리뷰 Medium — `-`로 시작하는
        // prompt가 와도 codex 인자 파서가 플래그로 재해석할 수 없어야 함).
        assert_eq!(
            cli_args(CliProvider::Codex, "gpt-5", "hello"),
            vec!["exec", "-m", "gpt-5", "--", "hello"]
        );
        assert_eq!(
            cli_args(CliProvider::Codex, "gpt-5", "--dangerous-looking-prompt"),
            vec!["exec", "-m", "gpt-5", "--", "--dangerous-looking-prompt"]
        );
    }

    // ── decide_engine(순수 결정 로직 매트릭스) ──────────────────────

    fn no_cli() -> CliDetected {
        CliDetected::default()
    }

    fn no_keys() -> ApiKeysPresent {
        ApiKeysPresent::default()
    }

    #[test]
    fn decide_engine_cli_mode_always_resolves_to_selected_provider_even_if_undetected() {
        assert_eq!(
            decide_engine(SummaryEngine::Cli, CliProvider::Gemini, ApiProvider::Anthropic, no_cli(), no_keys()),
            Ok(ResolvedEngine::Cli(CliProvider::Gemini))
        );
        // 3종 다 감지됐어도 선택된(codex) 프로바이더를 그대로 쓴다 — auto와 달리 우선순위 재배정 없음.
        let all_detected = CliDetected { claude: true, gemini: true, codex: true };
        assert_eq!(
            decide_engine(
                SummaryEngine::Cli,
                CliProvider::Codex,
                ApiProvider::Anthropic,
                all_detected,
                no_keys()
            ),
            Ok(ResolvedEngine::Cli(CliProvider::Codex))
        );
    }

    #[test]
    fn decide_engine_api_mode_requires_selected_provider_key() {
        let keys = ApiKeysPresent { anthropic: true, openai: true, gemini: false };
        assert_eq!(
            decide_engine(SummaryEngine::Api, CliProvider::Claude, ApiProvider::Openai, no_cli(), keys),
            Ok(ResolvedEngine::Api(ApiProvider::Openai))
        );

        let keys_missing_gemini = ApiKeysPresent { anthropic: true, openai: false, gemini: false };
        let err = decide_engine(
            SummaryEngine::Api,
            CliProvider::Claude,
            ApiProvider::Gemini,
            no_cli(),
            keys_missing_gemini,
        )
        .unwrap_err();
        assert_eq!(err, api_key_missing_message());
    }

    #[test]
    fn decide_engine_auto_prefers_cli_in_claude_gemini_codex_order() {
        assert_eq!(
            decide_engine(
                SummaryEngine::Auto,
                CliProvider::Claude,
                ApiProvider::Anthropic,
                CliDetected { claude: true, gemini: true, codex: true },
                no_keys()
            ),
            Ok(ResolvedEngine::Cli(CliProvider::Claude))
        );
        assert_eq!(
            decide_engine(
                SummaryEngine::Auto,
                CliProvider::Claude,
                ApiProvider::Anthropic,
                CliDetected { claude: false, gemini: true, codex: true },
                no_keys()
            ),
            Ok(ResolvedEngine::Cli(CliProvider::Gemini))
        );
        assert_eq!(
            decide_engine(
                SummaryEngine::Auto,
                CliProvider::Claude,
                ApiProvider::Anthropic,
                CliDetected { claude: false, gemini: false, codex: true },
                no_keys()
            ),
            Ok(ResolvedEngine::Cli(CliProvider::Codex))
        );
    }

    #[test]
    fn decide_engine_auto_falls_back_to_api_key_order_anthropic_openai_gemini() {
        assert_eq!(
            decide_engine(
                SummaryEngine::Auto,
                CliProvider::Claude,
                ApiProvider::Anthropic,
                no_cli(),
                ApiKeysPresent { anthropic: true, openai: true, gemini: true }
            ),
            Ok(ResolvedEngine::Api(ApiProvider::Anthropic))
        );
        assert_eq!(
            decide_engine(
                SummaryEngine::Auto,
                CliProvider::Claude,
                ApiProvider::Anthropic,
                no_cli(),
                ApiKeysPresent { anthropic: false, openai: true, gemini: true }
            ),
            Ok(ResolvedEngine::Api(ApiProvider::Openai))
        );
        assert_eq!(
            decide_engine(
                SummaryEngine::Auto,
                CliProvider::Claude,
                ApiProvider::Anthropic,
                no_cli(),
                ApiKeysPresent { anthropic: false, openai: false, gemini: true }
            ),
            Ok(ResolvedEngine::Api(ApiProvider::Gemini))
        );
    }

    #[test]
    fn decide_engine_auto_errors_when_nothing_available() {
        let err =
            decide_engine(SummaryEngine::Auto, CliProvider::Claude, ApiProvider::Anthropic, no_cli(), no_keys())
                .unwrap_err();
        assert_eq!(err, no_engine_available_message());
    }

    // ── API 응답 파싱(JSON 픽스처) ───────────────────────────────

    #[test]
    fn parse_anthropic_response_extracts_text() {
        let payload = serde_json::json!({ "content": [{ "type": "text", "text": "안녕하세요" }] });
        assert_eq!(parse_anthropic_response(&payload).unwrap(), "안녕하세요");
    }

    #[test]
    fn parse_anthropic_response_errors_when_missing() {
        assert!(parse_anthropic_response(&serde_json::json!({})).is_err());
    }

    #[test]
    fn parse_openai_response_extracts_text() {
        let payload = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "안녕하세요" } }]
        });
        assert_eq!(parse_openai_response(&payload).unwrap(), "안녕하세요");
    }

    #[test]
    fn parse_openai_response_errors_when_missing() {
        assert!(parse_openai_response(&serde_json::json!({ "choices": [] })).is_err());
    }

    #[test]
    fn parse_gemini_response_extracts_text() {
        let payload = serde_json::json!({
            "candidates": [{ "content": { "parts": [{ "text": "안녕하세요" }] } }]
        });
        assert_eq!(parse_gemini_response(&payload).unwrap(), "안녕하세요");
    }

    #[test]
    fn parse_gemini_response_errors_when_missing() {
        assert!(parse_gemini_response(&serde_json::json!({ "candidates": [] })).is_err());
    }
}
