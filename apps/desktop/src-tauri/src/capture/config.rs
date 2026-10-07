//! 캡처 설정 로드(`~/.logroom/config.json`) + 감시 root 해석 (docs/03-capture.md).
//! 설정 파일이 없거나 파싱에 실패해도 앱은 죽지 않고 기본 동작(자동 탐지 + 하드코딩 기본 경로)으로 폴백한다.
//!
//! Claude Code는 `CLAUDE_CONFIG_DIR`로 `~/.claude-a`, `~/.claude-b` 같은 커스텀 경로를 쓸 수 있어
//! 기본 `~/.claude` 하나만 감시하면 실사용 데이터가 캡처되지 않는다(실측). 그래서:
//! 1) home 아래 `.claude*` 패턴 디렉토리를 자동 탐지하고,
//! 2) 설정 파일의 `extraRoots`로 그 외 경로를 추가할 수 있게 한다.
//!
//! `get_capture_config`/`set_capture_config`/`restart_app` Tauri 커맨드(`lib.rs`)가 이 모듈의
//! [`CaptureConfig`]/[`load_config`]/[`update_config`]/[`resolve_roots_detailed`]를 그대로 사용한다.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// 감시 root가 어느 캡처 어댑터에 속하는지. `watch.rs`의 `process_file`에서 경로가 속한 root로 판별하고,
/// `get_capture_config` 응답의 `resolvedRoots[].source` 값(`"claude_code"` | `"kiro_cli"`)으로 그대로
/// 직렬화된다 — [`Source::cursor_source`]가 반환하는 문자열과 동일하다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Source {
    #[serde(rename = "claude_code")]
    Claude,
    #[serde(rename = "kiro_cli")]
    KiroCli,
}

impl Source {
    /// `capture_cursors.source` 값이자 인제스트 이벤트의 `source` 값(watch.rs 커서 조회/갱신 키).
    pub fn cursor_source(self) -> &'static str {
        match self {
            Source::Claude => crate::capture::normalize::SOURCE,
            Source::KiroCli => crate::capture::kiro::SOURCE,
        }
    }
}

/// root가 자동 탐지된 기본값인지, 설정 파일의 `extraRoots`로 추가된 커스텀 값인지.
/// `get_capture_config` 응답의 `resolvedRoots[].origin` 값(`"default"` | `"custom"`)으로 그대로 직렬화된다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Default,
    Custom,
}

fn default_true() -> bool {
    true
}

/// 캡처 시 이벤트 body/metadata 저장 범위(ADR-0012 회고 중심 저장).
/// `essential`(기본): `tool_result` body는 256자, `tool_use` metadata의 input은 512자로 절단해
/// 저장 규모를 줄인다(prompt/response는 항상 전문). `full`: 절단 없이 원문 그대로 저장.
/// `get_capture_config`/`set_capture_config` 응답의 `bodyPolicy` 값(`"essential"` | `"full"`)과 1:1 대응 —
/// [`load_body_policy`]로 watch → normalize/kiro 순수함수에 전달된다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyPolicy {
    #[default]
    Essential,
    Full,
}

/// 소스 1개(Claude Code 또는 Kiro CLI)에 대한 캡처 root 설정.
/// FE 계약(`CaptureSourceConfig`, camelCase)과 1:1 대응.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceConfig {
    #[serde(default)]
    pub extra_roots: Vec<PathBuf>,
    #[serde(default = "default_true")]
    pub use_defaults: bool,
    /// 이 소스 캡처 사용 여부. `false`면 [`resolve_roots_detailed`]가 이 소스의 root(기본+커스텀)를
    /// 아예 해석 결과에서 제외한다 — root가 0개가 되어 `capture/health.rs`의 `inactive` 판정도
    /// 자연히 뒤따른다. 기본 true(기존 동작 유지) — `bool`은 `derive(Default)`가 `false`를 주므로
    /// 필드별 기본값 함수가 필요하다.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            extra_roots: Vec::new(),
            use_defaults: true,
            enabled: true,
        }
    }
}

/// Slack 커넥터(v1) 설정 — 로컬 폴링 + 수동 user token(docs/08-connectors.md, ADR-0015).
/// FE 계약(`SlackConfig`, camelCase)과 1:1 대응. 파일 감시(root) 개념이 없는 폴러형 소스라
/// [`SourceConfig`]와 별도 타입으로 둔다(`capture/slack.rs`가 이 설정을 소비).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SlackConfig {
    /// 수동 발급 user token(`xoxp-...`, `search:read` 스코프 필요). 저장 위치는 다른 캡처 설정과
    /// 동일한 `~/.logroom/config.json`(docs/08-connectors.md "토큰은 로컬 저장" 원칙 — 키체인
    /// 이관은 커넥터가 늘어나는 시점에 일괄 검토).
    #[serde(default)]
    pub token: Option<String>,
    /// 이 커넥터 사용 여부. 기본 false(SaaS 커넥터는 opt-in, 04-privacy-security.md 네트워크 정책).
    #[serde(default)]
    pub enabled: bool,
    /// 폴링 주기(분). 기본 5 — Slack `search.messages`의 Tier 2 레이트리밋을 고려한 기본값
    /// (docs/08-connectors.md "Rate limit 전략").
    #[serde(default = "default_slack_poll_minutes")]
    pub poll_minutes: u32,
}

fn default_slack_poll_minutes() -> u32 {
    5
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            token: None,
            enabled: false,
            poll_minutes: default_slack_poll_minutes(),
        }
    }
}

/// GitHub 커넥터(v1, docs/08-connectors.md) 계정 1개 — 다중 계정 지원(슬랙 v1의 단일 토큰과 다른 점).
/// `token`은 사용자가 직접 발급한 PAT(fine-grained 권장, `repo` 읽기 스코프). `username`은 부팅/폴링
/// 시 `GET /user`로 확인된 로그인명 — 처음 저장할 때는 아직 검증 전이라 `None`이고, 폴러
/// (`capture/github.rs::spawn`)가 검증에 성공하면 이 필드를 채워 저장한다(FE 계정 목록 표시용).
/// 토큰이 무효화되면 `username`은 마지막으로 확인된 값 그대로 남는다(그 계정만 skip+로그,
/// `capture/health.rs`의 헬스 판정에 반영).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GithubAccount {
    pub token: String,
    #[serde(default)]
    pub username: Option<String>,
}

/// GitHub 커넥터(v1) 설정 — 계정 여러 개를 등록할 수 있다(슬랙과의 핵심 차이). FE 계약
/// (`GithubConfig`, camelCase)과 1:1 대응. `capture/github.rs`가 이 설정을 소비한다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GithubConfig {
    #[serde(default)]
    pub accounts: Vec<GithubAccount>,
    /// 이 커넥터 사용 여부. 기본 false(SaaS 커넥터는 opt-in, 04-privacy-security.md 네트워크 정책).
    #[serde(default)]
    pub enabled: bool,
    /// Events(전방 증분) 폴링 주기(분). 기본 5 — Slack과 동일한 기본값(docs/08-connectors.md).
    /// Search 백필은 이 값과 무관하게 자체 rate limit(분당 30회)과 따라잡기 가속 규칙을 따른다.
    #[serde(default = "default_github_poll_minutes")]
    pub poll_minutes: u32,
}

fn default_github_poll_minutes() -> u32 {
    5
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            enabled: false,
            poll_minutes: default_github_poll_minutes(),
        }
    }
}

/// Linear 커넥터(v1, docs/08-connectors.md) 계정(워크스페이스) 1개 — 다중 워크스페이스 지원
/// (GitHub v1의 다중 계정과 동일한 패턴). `token`은 사용자가 직접 발급한 Personal API Key
/// (`lin_api_...`). `viewerId`/`viewerName`은 부팅/폴링 시 `viewer { id name email }`로 확인된 값 —
/// 처음 저장할 때는 아직 검증 전이라 둘 다 `None`이고, 폴러(`capture/linear.rs::spawn`)가 검증에
/// 성공하면 채워 저장한다(FE 계정 목록 표시용 + `resolve_linear_accounts_update`의 계정 매칭 키).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LinearAccount {
    pub token: String,
    #[serde(default)]
    pub viewer_id: Option<String>,
    #[serde(default)]
    pub viewer_name: Option<String>,
}

/// Linear 커넥터(v1) 설정 — 계정(워크스페이스) 여러 개를 등록할 수 있다(GitHub와 동일한 다중 계정
/// 패턴). FE 계약(`LinearConfig`, camelCase)과 1:1 대응. `capture/linear.rs`가 이 설정을 소비한다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LinearConfig {
    #[serde(default)]
    pub accounts: Vec<LinearAccount>,
    /// 이 커넥터 사용 여부. 기본 false(SaaS 커넥터는 opt-in, 04-privacy-security.md 네트워크 정책).
    #[serde(default)]
    pub enabled: bool,
    /// 전방 증분(issues/comments) 폴링 주기(분). 기본 5 — Slack/GitHub와 동일한 기본값
    /// (docs/08-connectors.md). 백필은 이 값과 무관하게 따라잡기 가속 규칙을 따른다.
    #[serde(default = "default_linear_poll_minutes")]
    pub poll_minutes: u32,
}

fn default_linear_poll_minutes() -> u32 {
    5
}

impl Default for LinearConfig {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            enabled: false,
            poll_minutes: default_linear_poll_minutes(),
        }
    }
}

/// Notion 토큰 종류 — 폴러가 `GET /v1/users/me` 응답의 `type`으로 판별한다(사용자가 고르지 않는다,
/// docs/08-connectors.md "Notion v1" §인증). FE 계약 값은 `"personal"` | `"integration"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotionTokenKind {
    /// Personal Access Token(2026-05 도입) — `users/me`가 토큰을 만든 **사람**을 준다. 그 사람이
    /// 볼 수 있는 페이지 전체가 대상이고, 마지막 편집자가 그 사람인 편집만 남긴다.
    Personal,
    /// Internal Integration Secret — `users/me`가 **bot**을 준다. 페이지마다 연결을 붙인 범위만
    /// 보이고, "나"를 알 수 없어 편집자 필터가 없다.
    Integration,
}

/// Notion 커넥터(v1, docs/08-connectors.md "Notion v1") 계정 1개 — Linear/GitHub와 동일한 다중 계정
/// 패턴. `token`은 사용자가 발급한 Personal Access Token 또는 Internal Integration Secret이다(종류는
/// 폴러가 판별해 `kind`에 적는다).
///
/// - `id`: 로컬에서 부여한 계정 식별자(UUID v7). 커서 resource 키이자 설정 왕복 매칭 키다. **PAT는
///   워크스페이스 id를 주지 않고**, 같은 사람이 개인·회사 워크스페이스에 각각 PAT를 만들면 `users/me`가
///   같은 사람을 돌려주므로 API 응답에서 계정을 구분할 값을 얻을 수 없다. 그래서 로컬 id를 쓴다.
///   FE가 새로 추가한 계정은 `None`으로 들어오고 [`resolve_notion_accounts_update`]가 채운다.
/// - `label`: 사용자가 붙인 이름("회사", "개인"). 목록 표시와 스트림 `project`에 쓴다. PAT는
///   워크스페이스명을 알 수 없어서 이 값이 없으면 계정들이 타임라인에서 구분되지 않는다.
/// - `kind`/`userName`/`workspaceId`/`workspaceName`: 폴러(`capture/notion.rs::spawn`)가 검증에 성공하면
///   채워 저장한다(검증 전에는 `None`). PAT면 `userName`, integration이면 `workspace*`가 채워진다.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotionAccount {
    pub token: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub kind: Option<NotionTokenKind>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_name: Option<String>,
}

/// 계정 식별자를 새로 만든다(UUID v7 — `db.rs`의 id 생성과 같은 방식).
pub fn new_notion_account_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// `id`가 없거나 앞 계정과 겹치는 계정에 새 id를 채운다. 바뀐 게 있으면 `true`. 설정 파일을 손으로
/// 고쳤거나 id 도입 전에 저장된 계정이 폴러에 들어올 때 쓴다(`capture/notion.rs::spawn`). id가 겹치면
/// 두 계정이 커서 하나(`search:<id>`)를 나눠 쓰며 서로의 진행 위치를 덮으므로 뒤의 것을 바꾼다.
pub fn assign_missing_notion_account_ids(accounts: &mut [NotionAccount]) -> bool {
    let mut seen = HashSet::new();
    let mut changed = false;
    for account in accounts.iter_mut() {
        let usable = account
            .id
            .as_deref()
            .is_some_and(|id| !id.is_empty() && !seen.contains(id));
        if !usable {
            account.id = Some(new_notion_account_id());
            changed = true;
        }
        if let Some(id) = &account.id {
            seen.insert(id.clone());
        }
    }
    changed
}

/// Notion 커넥터(v1) 설정 — 계정(워크스페이스) 여러 개를 등록할 수 있다(GitHub/Linear와 동일한
/// 다중 계정 패턴). FE 계약(`NotionConfig`, camelCase)과 1:1 대응. `capture/notion.rs`가 이 설정을
/// 소비한다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotionConfig {
    #[serde(default)]
    pub accounts: Vec<NotionAccount>,
    /// 이 커넥터 사용 여부. 기본 false(SaaS 커넥터는 opt-in, 04-privacy-security.md 네트워크 정책).
    #[serde(default)]
    pub enabled: bool,
    /// 폴링 주기(분). 기본 5 — Slack/GitHub/Linear와 동일한 기본값(docs/08-connectors.md). Notion
    /// Search API는 시간 범위 필터가 없어 백필/따라잡기 가속 개념이 없다 — 항상 이 간격으로만 돈다
    /// (`capture/notion.rs` 모듈 문서 "커서" 참고).
    #[serde(default = "default_notion_poll_minutes")]
    pub poll_minutes: u32,
}

fn default_notion_poll_minutes() -> u32 {
    5
}

impl Default for NotionConfig {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            enabled: false,
            poll_minutes: default_notion_poll_minutes(),
        }
    }
}

/// 일일 AI 요약(M7-①, ADR-0016) 엔진 선택 — "자동(감지 시 우선)/CLI/API 키" 3택. 어떤 CLI/API를
/// 쓸지는 이 값과 별개인 [`CliProvider`]/[`ApiProvider`]가 정한다(모드 × 프로바이더 조합).
/// `summary::engine`이 이 값을 읽어 실제 백엔드를 고른다. `get_summary_config`/`set_summary_config`
/// 응답의 `engine` 값(`"auto"` | `"cli"` | `"api"`)과 1:1 대응.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SummaryEngine {
    #[default]
    Auto,
    Cli,
    Api,
}

/// `engine = cli`(또는 `auto`)일 때 실제로 실행할 CLI 프로바이더. `summary::engine::cli_args`가
/// 이 값에 따라 인자 조립 규칙을 고른다. `get_summary_config`/`set_summary_config` 응답의
/// `cliProvider` 값(`"claude"` | `"gemini"` | `"codex"`)과 1:1 대응.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliProvider {
    #[default]
    Claude,
    Gemini,
    Codex,
}

/// `engine = api`(또는 `auto`)일 때 실제로 호출할 API 프로바이더. `apiProvider` 값에 대응하는 키
/// 필드(`anthropicApiKey`/`openaiApiKey`/`geminiApiKey`)를 쓴다. `get_summary_config`/
/// `set_summary_config` 응답의 `apiProvider` 값(`"anthropic"` | `"openai"` | `"gemini"`)과 1:1 대응.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiProvider {
    #[default]
    Anthropic,
    Openai,
    Gemini,
}

fn default_cli_model() -> String {
    "sonnet".to_string()
}

fn default_api_model() -> String {
    "claude-haiku-4-5".to_string()
}

/// 일일 AI 요약(M7-①, ADR-0016) 설정 — 기본 off(명시적 opt-in, 04-privacy-security.md 네트워크
/// 정책과 동일 원칙). FE 계약(`SummaryConfig`, camelCase)과 1:1 대응. 3개 API 키 각각
/// Slack/GitHub/Linear 토큰과 동일한 마스킹 왕복 원칙을 따른다([`mask_summary_api_key`]/
/// [`resolve_summary_api_key_update`] — 필드별로 독립 호출).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SummaryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub engine: SummaryEngine,
    #[serde(default)]
    pub cli_provider: CliProvider,
    #[serde(default)]
    pub api_provider: ApiProvider,
    /// Anthropic API 키. 과거 필드명이 `apiKey`(단일 프로바이더 시절)였던 구버전 `config.json`을
    /// 무손실로 읽기 위해 `alias = "apiKey"`를 둔다 — 마이그레이션 스크립트 없이도 기존 사용자의
    /// 키가 그대로 이 필드로 들어온다.
    #[serde(default, alias = "apiKey")]
    pub anthropic_api_key: Option<String>,
    #[serde(default)]
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub gemini_api_key: Option<String>,
    #[serde(default = "default_cli_model")]
    pub cli_model: String,
    #[serde(default = "default_api_model")]
    pub api_model: String,
    /// CLI 백엔드에 주입할 `CLAUDE_CONFIG_DIR`(선택) — **Claude CLI 전용**(Gemini/Codex CLI는 별도
    /// 설정 디렉토리 개념이 없어 이 값을 쓰지 않는다, `summary::engine::generate_via_cli` 참고).
    /// 셸 alias 로 CLAUDE_CONFIG_DIR 을 나눠 쓰는 사용자는 앱이 실행하는 `claude`가 셸 환경변수
    /// 없이 기본 `~/.claude`로 붙어 미로그인 실패하므로, 로그인된 디렉토리(예: `~/.claude-b`)를
    /// 지정할 수 있게 한다(실사용 요구).
    #[serde(default)]
    pub cli_config_dir: Option<String>,
    /// 매일 자동 생성(캐치업형) — FE(useAutoDailySummary)가 주기적으로 "어제 날짜 요약이 캐시에
    /// 없으면 생성"한다. 자정 크론이 아니라 캐치업이라 앱이 꺼져 있어도 다음 실행 때 따라잡는다
    /// (커넥터 백필과 동일 사상). 아웃바운드가 늘어나는 옵션이므로 기본 off(opt-in).
    #[serde(default)]
    pub auto_generate: bool,
}

impl Default for SummaryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: SummaryEngine::default(),
            cli_provider: CliProvider::default(),
            api_provider: ApiProvider::default(),
            anthropic_api_key: None,
            openai_api_key: None,
            gemini_api_key: None,
            cli_model: default_cli_model(),
            api_model: default_api_model(),
            cli_config_dir: None,
            auto_generate: false,
        }
    }
}

/// 평가 프로필 직무 입력 상한(문자 수) — 평가서 발췌 한 줄에 들어가는 자유 입력이라 짧게 자른다.
pub const REVIEW_ROLE_MAX_CHARS: usize = 60;

/// 평가 프로필에서 받는 연차 구간 값(FE `ReviewLevel`). 빈 문자열 = 미지정.
pub const REVIEW_LEVELS: [&str; 4] = ["junior", "mid", "senior", "lead"];

/// 업무평가서(ADR-0017) 평가 프로필 — 직무·연차 구간·팀원 관리 여부. 전부 선택 항목이다. 평가서
/// 발췌에 그대로 실려 AI로 전송되므로(미리보기로 확인 가능) **연봉 같은 민감 정보는 받지 않는다**
/// (평가 기준을 바꾸지 못하면서 매 생성마다 외부로 나가기 때문). FE 계약(`ReviewProfile`, camelCase)과
/// 1:1 대응.
///
/// `level`을 enum이 아니라 문자열로 두는 이유: `RootConfig`는 한 필드라도 역직렬화에 실패하면 파일
/// 전체가 기본값으로 폴백하고(커넥터 토큰까지), 다음 저장 때 그 기본값이 파일을 덮는다. 손으로 고친
/// 파일이나 이후 버전이 모르는 값을 넣어도 설정 전체가 날아가지 않도록, 알 수 없는 값은
/// [`ReviewProfile::normalized`]에서 미지정으로 떨어뜨린다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReviewProfile {
    pub role: String,
    pub level: String,
    pub manages_people: bool,
}

impl ReviewProfile {
    /// 저장·발췌 직전 정규화 — 직무는 trim 후 [`REVIEW_ROLE_MAX_CHARS`]자로 자르고, 연차는
    /// [`REVIEW_LEVELS`]에 없으면 미지정(빈 문자열)으로 둔다.
    pub fn normalized(&self) -> Self {
        let role: String = self.role.trim().chars().take(REVIEW_ROLE_MAX_CHARS).collect();
        let level = if REVIEW_LEVELS.contains(&self.level.as_str()) {
            self.level.clone()
        } else {
            String::new()
        };
        Self {
            role: role.trim_end().to_string(),
            level,
            manages_people: self.manages_people,
        }
    }

    /// 세 항목이 모두 비어 있는가 — 발췌에 "입력 없음 — 공통 기준"으로 적는다.
    pub fn is_empty(&self) -> bool {
        self.role.is_empty() && self.level.is_empty() && !self.manages_people
    }
}

/// 요약 API 키 마스킹 — Slack/GitHub/Linear 토큰과 동일한 표기(`"••••" + 마지막 4자`)를 재사용한다
/// (커넥터/요약 시크릿마다 다른 접두사를 쓸 이유가 없어 [`mask_slack_token`]에 위임하는 얇은 별칭).
pub fn mask_summary_api_key(key: &str) -> String {
    mask_slack_token(key)
}

/// `set_summary_config`로 들어온 `apiKey` 값을 해석한다 — [`resolve_slack_token_update`]와 동일한
/// 원칙(마스킹 값이면 기존 키 보존, 그 외에는 trim 후 채택, 빈 문자열/`None`이면 삭제)을 그대로
/// 위임하는 얇은 별칭이다(단일 값이라 배열 매칭 로직은 필요 없음 — 마스킹 값 판별은
/// [`resolve_slack_token_update`] 내부의 [`is_masked_slack_token`]이 담당한다).
pub fn resolve_summary_api_key_update(
    previous_api_key: Option<&str>,
    incoming: Option<String>,
) -> Option<String> {
    resolve_slack_token_update(previous_api_key, incoming)
}

/// FE 왕복 시 slack 토큰을 감추는 데 쓰는 접두사. `get_capture_config`가 이 접두사로 마스킹된 값을
/// 돌려주고, `set_capture_config`는 들어온 값이 이 접두사로 시작하면 "FE가 변경 없이 그대로 되돌려
/// 보냈다"고 판단해 기존 토큰을 보존한다([`is_masked_slack_token`]/[`resolve_slack_token_update`] 참고).
pub const SLACK_TOKEN_MASK_PREFIX: &str = "••••";

/// slack 토큰을 FE 응답에 실을 때 원문 대신 쓰는 마스킹 값: `"••••" + 마지막 4자`.
/// (`get_capture_config`가 사용 — 토큰 원문이 FE/네트워크 왕복에 그대로 노출되지 않게 한다).
pub fn mask_slack_token(token: &str) -> String {
    let visible_len = token.len().min(4);
    let visible = &token[token.len() - visible_len..];
    format!("{SLACK_TOKEN_MASK_PREFIX}{visible}")
}

/// `value`가 [`mask_slack_token`]이 만든 마스킹 값인지(= FE가 변경 없이 그대로 되돌려보낸 값인지).
pub fn is_masked_slack_token(value: &str) -> bool {
    value.starts_with(SLACK_TOKEN_MASK_PREFIX)
}

/// `set_capture_config`로 들어온 slack.token 값을 해석한다(순수함수 — 실제 이전 토큰 로드는
/// 호출부인 `lib.rs::set_capture_config`가 담당). `incoming`이 `None`이면 사용자가 명시적으로 지운
/// 것으로 보아 그대로 `None`(토큰 삭제). [`is_masked_slack_token`]이 참이면 FE가 마스킹 값을 변경
/// 없이 그대로 되돌려보낸 것이라 `previous_token`을 그대로 유지한다. 그 외에는 새로 입력된 값으로
/// 보아 앞뒤 공백을 trim한 뒤 저장한다(trim 후 빈 문자열이면 `None` — FE도 동일하게 trim하지만
/// 이중 방어, 04-privacy-security.md/08-connectors.md 토큰 취급 원칙).
pub fn resolve_slack_token_update(previous_token: Option<&str>, incoming: Option<String>) -> Option<String> {
    match incoming {
        None => None,
        Some(value) if is_masked_slack_token(&value) => previous_token.map(str::to_string),
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
    }
}

/// GitHub PAT 마스킹 — Slack 토큰과 동일한 표기(`"••••" + 마지막 4자`)를 그대로 재사용한다
/// (커넥터마다 다른 접두사를 쓸 이유가 없어 [`mask_slack_token`]에 위임하는 얇은 별칭).
pub fn mask_github_token(token: &str) -> String {
    mask_slack_token(token)
}

/// `value`가 [`mask_github_token`]이 만든 마스킹 값인지.
pub fn is_masked_github_token(value: &str) -> bool {
    is_masked_slack_token(value)
}

/// `set_capture_config`로 들어온 `github.accounts` 배열을 해석한다 — [`resolve_slack_token_update`]의
/// 배열 확장판(다중 계정이라 토큰 1개가 아니라 목록 전체를 다뤄야 한다). 각 incoming 계정에 대해:
/// - 토큰이 마스킹 값이 **아니면**(사용자가 새로 입력/수정) trim한 뒤 그대로 채택한다. trim 후
///   빈 문자열이면 사용자가 명시적으로 지운 것으로 보아 해당 계정 자체를 결과에서 제거한다.
/// - 마스킹 값이면 FE가 변경 없이 그대로 되돌려보낸 것 — 기존 계정 목록에서 원본 토큰을 복원한다.
///   먼저 `username`으로 매칭한다(계정 삭제로 배열 순서가 바뀌어도 안전 — 인덱스만 보면 삭제된
///   계정 뒤의 항목들이 한 칸씩 당겨져 서로 다른 계정의 토큰과 잘못 매칭될 위험이 있다). `username`이
///   아직 없으면(폴러가 처음 검증하기 전) 마스킹 값이 유일하게 일치하는 계정([`find_unique_by_mask`]),
///   그것도 안 되면 같은 인덱스로 폴백 매칭한다. 그마저 못 찾으면(비정상 상황 — 예: 값 조작) 마스킹
///   값을 그대로 저장해 토큰을 훼손하는 대신 해당 계정을 제거한다.
pub fn resolve_github_accounts_update(
    previous: &[GithubAccount],
    incoming: Vec<GithubAccount>,
) -> Vec<GithubAccount> {
    incoming
        .into_iter()
        .enumerate()
        .filter_map(|(idx, account)| {
            if !is_masked_github_token(&account.token) {
                let trimmed = account.token.trim();
                if trimmed.is_empty() {
                    return None;
                }
                return Some(GithubAccount {
                    token: trimmed.to_string(),
                    username: account.username,
                });
            }
            account
                .username
                .as_deref()
                .and_then(|uname| previous.iter().find(|p| p.username.as_deref() == Some(uname)))
                .or_else(|| find_unique_by_mask(previous, &account.token, |p| &p.token))
                .or_else(|| previous.get(idx))
                .cloned()
        })
        .collect()
}

/// Linear API 키 마스킹 — Slack/GitHub 토큰과 동일한 표기(`"••••" + 마지막 4자`)를 재사용한다
/// (커넥터마다 다른 접두사를 쓸 이유가 없어 [`mask_slack_token`]에 위임하는 얇은 별칭).
pub fn mask_linear_token(token: &str) -> String {
    mask_slack_token(token)
}

/// `value`가 [`mask_linear_token`]이 만든 마스킹 값인지.
pub fn is_masked_linear_token(value: &str) -> bool {
    is_masked_slack_token(value)
}

/// `set_capture_config`로 들어온 `linear.accounts` 배열을 해석한다 — [`resolve_github_accounts_update`]와
/// 동일한 원칙을 `viewerId` 매칭으로 적용한 버전이다(GitHub는 `username`, Linear는 `viewerId`가 계정
/// 매칭 키). 마스킹 값이 들어오면 `viewerId`로 먼저 매칭하고(계정 삭제로 배열 순서가 바뀌어도 안전),
/// `viewerId`가 아직 없으면(폴러가 처음 검증하기 전) 마스킹 값이 유일하게 일치하는 계정
/// ([`find_unique_by_mask`]), 그것도 안 되면 같은 인덱스로 폴백 매칭한다. 그마저 못 찾으면 마스킹
/// 값을 그대로 저장해 토큰을 훼손하는 대신 해당 계정을 제거한다.
pub fn resolve_linear_accounts_update(
    previous: &[LinearAccount],
    incoming: Vec<LinearAccount>,
) -> Vec<LinearAccount> {
    incoming
        .into_iter()
        .enumerate()
        .filter_map(|(idx, account)| {
            if !is_masked_linear_token(&account.token) {
                let trimmed = account.token.trim();
                if trimmed.is_empty() {
                    return None;
                }
                return Some(LinearAccount {
                    token: trimmed.to_string(),
                    viewer_id: account.viewer_id,
                    viewer_name: account.viewer_name,
                });
            }
            account
                .viewer_id
                .as_deref()
                .and_then(|vid| previous.iter().find(|p| p.viewer_id.as_deref() == Some(vid)))
                .or_else(|| find_unique_by_mask(previous, &account.token, |p| &p.token))
                .or_else(|| previous.get(idx))
                .cloned()
        })
        .collect()
}

/// FE가 돌려보낸 마스킹 값(`"••••" + 마지막 4자`)과 일치하는 원본 계정을 찾는다 — **유일하게 걸릴
/// 때만** 돌려주고, 마지막 4자가 겹치는 계정이 둘 이상이면 어느 쪽인지 알 수 없으므로 `None`(호출부가
/// 인덱스 폴백에 맡긴다). 마스킹 값은 원본 토큰에서 파생되므로 배열 순서가 바뀌어도 따라간다.
///
/// **식별 키(`username`/`viewerId`)가 아직 없는 계정을 곧장 인덱스로 매칭하면 조용한 계정 뒤바뀜이
/// 생긴다**: 미검증 계정 A·B를 등록한 뒤 A를 지우고 저장하면, 남은 B가 인덱스 0으로 내려와
/// `previous[0]`(= A)에 매칭돼 **지우려던 A의 토큰이 부활하고 남기려던 B가 사라진다.** 커넥터를 꺼 둔
/// 채 계정만 등록하면 폴러가 돌지 않아 식별 키가 무기한 비어 있으므로 흔히 걸린다.
fn find_unique_by_mask<'a, T>(
    previous: &'a [T],
    masked: &str,
    token_of: impl Fn(&T) -> &str,
) -> Option<&'a T> {
    let mut hits = previous
        .iter()
        .filter(|p| mask_slack_token(token_of(p)) == masked);
    let first = hits.next();
    if hits.next().is_none() {
        first
    } else {
        None
    }
}

/// Notion Integration Secret 마스킹 — Slack/GitHub/Linear 토큰과 동일한 표기(`"••••" + 마지막 4자`)를
/// 재사용한다(커넥터마다 다른 접두사를 쓸 이유가 없어 [`mask_slack_token`]에 위임하는 얇은 별칭).
pub fn mask_notion_token(token: &str) -> String {
    mask_slack_token(token)
}

/// `value`가 [`mask_notion_token`]이 만든 마스킹 값인지.
pub fn is_masked_notion_token(value: &str) -> bool {
    is_masked_slack_token(value)
}

/// `set_capture_config`로 들어온 `notion.accounts` 배열을 해석한다 — [`resolve_linear_accounts_update`]와
/// 같은 원칙이되 매칭 키가 로컬 계정 `id`다. 마스킹 값이 들어오면 아래 순서로 원본 계정을 찾는다:
///
/// 1. **`id` 일치** — 계정을 추가할 때 이 함수가 id를 부여하므로 한 번 저장된 계정은 항상 이 단계에서
///    잡힌다. 검증 여부와 무관하고 배열 순서가 바뀌어도 따라간다.
/// 2. **`workspaceId` 일치** — id 도입 전에 저장된 계정용.
/// 3. **마스킹 값 일치**([`find_unique_by_mask`]) — 배열 순서가 바뀌어도 따라간다. 마지막 4자가
///    겹치는 계정이 둘 이상이면 건너뛴다.
/// 4. **같은 인덱스** — 위가 모두 실패했을 때의 최후 폴백.
///
/// 그마저 못 찾으면 마스킹 값을 그대로 저장해 토큰을 훼손하는 대신 해당 계정을 제거한다.
///
/// 3번이 인덱스보다 앞서야 하는 이유는 [`find_unique_by_mask`] 참고(GitHub·Linear와 같은 단계).
///
/// 사용자가 편집하는 `label`은 원본이 아니라 incoming 값을 쓴다(목록에서 이름을 바꾼 경우). 폴러가
/// 채우는 필드(`kind`/`userName`/`workspace*`)는 원본 값을 유지한다.
pub fn resolve_notion_accounts_update(
    previous: &[NotionAccount],
    incoming: Vec<NotionAccount>,
) -> Vec<NotionAccount> {
    incoming
        .into_iter()
        .enumerate()
        .filter_map(|(idx, account)| {
            let label = account
                .label
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string);
            if !is_masked_notion_token(&account.token) {
                let trimmed = account.token.trim();
                if trimmed.is_empty() {
                    return None;
                }
                return Some(NotionAccount {
                    token: trimmed.to_string(),
                    id: account
                        .id
                        .filter(|id| !id.is_empty())
                        .or_else(|| Some(new_notion_account_id())),
                    label,
                    ..account
                });
            }
            let original = account
                .id
                .as_deref()
                .filter(|id| !id.is_empty())
                .and_then(|id| previous.iter().find(|p| p.id.as_deref() == Some(id)))
                .or_else(|| {
                    account.workspace_id.as_deref().and_then(|wid| {
                        previous
                            .iter()
                            .find(|p| p.workspace_id.as_deref() == Some(wid))
                    })
                })
                .or_else(|| find_unique_by_mask(previous, &account.token, |p| &p.token))
                .or_else(|| previous.get(idx))?;
            let mut resolved = original.clone();
            if resolved.id.as_deref().is_none_or(str::is_empty) {
                resolved.id = Some(new_notion_account_id());
            }
            resolved.label = label;
            Some(resolved)
        })
        .collect()
}

/// `get_capture_config`/`set_capture_config` 이 주고받는 전체 설정. FE 계약(`CaptureConfig`, camelCase)과
/// 1:1 대응. 파일에는 `RootConfig`(`{"capture": ...}` 래퍼)로 저장된다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptureConfig {
    pub claude_code: SourceConfig,
    pub kiro_cli: SourceConfig,
    pub body_policy: BodyPolicy,
    /// Slack 커넥터(v1) 설정(docs/08-connectors.md). 파일 감시 root 개념이 없어 `claude_code`/
    /// `kiro_cli`와 다른 타입([`SlackConfig`])을 쓴다.
    #[serde(default)]
    pub slack: SlackConfig,
    /// GitHub 커넥터(v1) 설정(docs/08-connectors.md) — 다중 계정 지원이 Slack과의 핵심 차이라
    /// 별도 타입([`GithubConfig`])을 쓴다.
    #[serde(default)]
    pub github: GithubConfig,
    /// Linear 커넥터(v1) 설정(docs/08-connectors.md) — GitHub와 동일한 다중 계정(워크스페이스)
    /// 패턴이라 별도 타입([`LinearConfig`])을 쓴다.
    #[serde(default)]
    pub linear: LinearConfig,
    /// Notion 커넥터(v1) 설정(docs/08-connectors.md "Notion v1") — GitHub/Linear와 동일한 다중 계정
    /// (워크스페이스) 패턴이라 별도 타입([`NotionConfig`])을 쓴다. Search API에 시간 범위 필터가
    /// 없어 이중 커서(전방 증분+후방 백필) 대신 단일 커서+조기 중단을 쓴다는 점이 다른 세 커넥터와
    /// 다르다(`capture/notion.rs` 모듈 문서 참고).
    #[serde(default)]
    pub notion: NotionConfig,
    /// 시크릿 스크럽(M4, docs/03-capture.md 공통 규칙·04-privacy-security.md) 사용 여부.
    /// 기본 true(프라이버시 기본 on) — `bool`은 `derive(Default)`가 `false`를 주므로
    /// 필드별 기본값 함수로 명시해야 한다(container `#[serde(default)]`만으로는 false가 됨).
    #[serde(default = "default_true")]
    pub scrub_secrets: bool,
    /// 캡처 일시정지(M4, docs/04-privacy-security.md "일시정지 의미론") 여부. true면 파일감시는
    /// cursor만 전진(영구 미기록)하고 HTTP 인제스트(`/v1/ingest`)는 저장 없이 응답한다.
    /// 재시작에도 유지되며, 런타임에는 `lib.rs`의 `Arc<AtomicBool>`로 즉시 반영된다(재시작 불필요).
    /// 기본 false(캡처 on) — `bool`의 `#[serde(default)]`(필드 기본)는 그대로 `false`라 별도 함수가 필요 없다.
    #[serde(default)]
    pub capture_paused: bool,
    /// 캡처에서 제외할 프로젝트 절대경로 prefix 목록(기본 빈 목록). `watch.rs`가 파일 처리 시작 전
    /// 한 번만 로드해([`load_exclude_projects`]) ingest 직전 `req.stream.project`(정규화된 레포 루트)와
    /// [`project_matches_exclude`]로 비교한다 — 매치되면 `capture_paused`와 동일하게 cursor만 전진시키고
    /// 저장은 영구 스킵한다(소급 삭제 아님, 매치 이후 활동만 미기록). HTTP 인제스트(`/v1/ingest`)는
    /// project 개념이 희박해(외부 클라이언트가 자유 지정) 이 목록의 적용 범위 밖이다.
    #[serde(default)]
    pub exclude_projects: Vec<String>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            claude_code: SourceConfig::default(),
            kiro_cli: SourceConfig::default(),
            body_policy: BodyPolicy::default(),
            slack: SlackConfig::default(),
            github: GithubConfig::default(),
            linear: LinearConfig::default(),
            notion: NotionConfig::default(),
            scrub_secrets: true,
            capture_paused: false,
            exclude_projects: Vec::new(),
        }
    }
}

/// 현재 Slack 커넥터 설정을 읽는다(`capture/slack.rs`의 폴러 부팅 + `capture/health.rs`의 헬스
/// 판정이 사용한다). 파일 부재/파싱 실패는 [`load_config`]와 동일하게 기본값(비활성)으로 폴백.
pub fn load_slack_config() -> SlackConfig {
    load_config().slack
}

/// 현재 GitHub 커넥터 설정을 읽는다(`capture/github.rs`의 폴러 부팅 + `capture/health.rs`의 헬스
/// 판정이 사용한다). 파일 부재/파싱 실패는 [`load_config`]와 동일하게 기본값(비활성)으로 폴백.
pub fn load_github_config() -> GithubConfig {
    load_config().github
}

/// 현재 Linear 커넥터 설정을 읽는다(`capture/linear.rs`의 폴러 부팅 + `capture/health.rs`의 헬스
/// 판정이 사용한다). 파일 부재/파싱 실패는 [`load_config`]와 동일하게 기본값(비활성)으로 폴백.
pub fn load_linear_config() -> LinearConfig {
    load_config().linear
}

/// 현재 Notion 커넥터 설정을 읽는다(`capture/notion.rs`의 폴러 부팅 + `capture/health.rs`의 헬스
/// 판정이 사용한다). 파일 부재/파싱 실패는 [`load_config`]와 동일하게 기본값(비활성)으로 폴백.
pub fn load_notion_config() -> NotionConfig {
    load_config().notion
}

/// `~/.logroom/config.json` 파일 자체의 최상위 shape(`{"capture": ..., "checkUpdates": ...}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RootConfig {
    capture: CaptureConfig,
    /// 자동 업데이트 백그라운드 체크(M5, ADR-0013) 사용 여부. `capture`와 도메인이 달라 최상위에 둔다.
    /// 기본 true — `bool`은 `derive(Default)`가 `false`를 주므로 아래 수기 `impl Default`로 맞춘다
    /// (`SourceConfig`/`CaptureConfig`와 동일한 이유, 위 주석 참고).
    #[serde(default = "default_true")]
    check_updates: bool,
    /// 트레이 메뉴 로케일("en" | "ko"). FE `set_app_locale` 커맨드가 저장하며, FE의 실제 표시
    /// 언어(store.ts `locale`이 "system"이면 resolveLocale로 해석된 값)를 그대로 받는다 — Rust
    /// 쪽에는 "system" 개념이 없다. `None`이면(첫 실행 등 FE가 아직 저장하지 않은 상태) 트레이는
    /// 기본값(영어)으로 표시한다. 적용은 앱 재시작 후(tray::setup이 부팅 시 1회 로드).
    #[serde(default)]
    locale: Option<String>,
    /// 일일 AI 요약(M7-①, ADR-0016) 설정. `capture`와 도메인이 달라 최상위에 둔다(checkUpdates와
    /// 동일한 이유).
    #[serde(default)]
    summary: SummaryConfig,
    /// 업무평가서 평가 프로필(ADR-0017). `summary`와 따로 두는 이유: 요약 설정은 FE가 객체 통째로
    /// 왕복 저장하므로(`set_summary_config`) 거기 섞으면 설정 화면 저장이 프로필을 덮을 수 있다.
    #[serde(default)]
    review_profile: ReviewProfile,
}

impl Default for RootConfig {
    fn default() -> Self {
        Self {
            capture: CaptureConfig::default(),
            check_updates: true,
            locale: None,
            summary: SummaryConfig::default(),
            review_profile: ReviewProfile::default(),
        }
    }
}

/// config를 실제 경로 하나로 해석한 결과. `get_capture_config`의 `resolvedRoots[]` 원본이며,
/// 감시 대상 산출(`resolve_roots`)의 중간 표현이기도 하다. FE 계약(`ResolvedCaptureRoot`, camelCase)과
/// 1:1 대응 — `lib.rs::get_capture_config`가 `serde_json::to_value`로 그대로 직렬화한다.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRoot {
    pub source: Source,
    #[serde(serialize_with = "serialize_path_lossy")]
    pub path: PathBuf,
    pub origin: Origin,
    pub exists: bool,
}

/// `PathBuf`를 문자열로 직렬화한다. serde 기본 `Serialize for PathBuf`는 비-UTF8 경로에서
/// 에러를 내지만, 여기서는 기존 `to_string_lossy()` 동작(항상 문자열, 손실 변환 허용)을 유지한다.
fn serialize_path_lossy<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&path.to_string_lossy())
}

fn config_path(home: &Path) -> PathBuf {
    home.join(".logroom").join("config.json")
}

/// 설정 파일 전체(`RootConfig`)를 읽어 파싱한다. 파일 부재는 정상 케이스(기본 동작)로 조용히
/// 기본값을 돌려주고, 파일은 있는데 파싱에 실패하면 `eprintln!` 경고 후 기본값으로 폴백한다
/// (크래시 금지). `load_config_from_home`/`load_check_updates`가 공유하는 내부 진입점 —
/// `capture`/`checkUpdates` 중 하나만 저장할 때도 이 함수로 전체를 읽어 다른 필드를 보존한다.
fn load_root_config_from_home(home: &Path) -> RootConfig {
    let path = config_path(home);
    match fs::read_to_string(&path) {
        Ok(raw) => {
            // 마이그레이션: 과거 버전이 기본 생성 권한(보통 0644)으로 만들었을 수 있는 기존
            // config.json 권한을 0600으로 교정한다(slack 토큰 등 시크릿을 담으므로). 실패해도
            // 설정 로드 자체는 막지 않는다(경고만 남김).
            if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
                eprintln!("[logroom] {}: 권한 교정 실패({e})", path.display());
            }
            serde_json::from_str::<RootConfig>(&raw).unwrap_or_else(|e| {
                eprintln!("[logroom] {}: 설정 파싱 실패({e}) — 기본 동작으로 진행", path.display());
                RootConfig::default()
            })
        }
        Err(_) => RootConfig::default(),
    }
}

/// 설정 파일을 읽어 파싱한다. 파일 부재/파싱 실패 시 폴백 동작은 [`load_root_config_from_home`] 참고.
fn load_config_from_home(home: &Path) -> CaptureConfig {
    load_root_config_from_home(home).capture
}

/// 현재 캡처 설정을 읽는다(`get_capture_config` 커맨드가 사용). 파일 부재/파싱 실패는 기본값으로 폴백.
pub fn load_config() -> CaptureConfig {
    let home = dirs::home_dir().expect("home_dir 없음");
    load_config_from_home(&home)
}

/// 자동 업데이트 백그라운드 체크(M5, ADR-0013) 사용 여부(`get_check_updates` 커맨드 +
/// `update::spawn_periodic_check`가 매 주기 다시 읽는다). 파일 부재/파싱 실패는 기본값(true)으로 폴백.
pub fn load_check_updates() -> bool {
    let home = dirs::home_dir().expect("home_dir 없음");
    load_root_config_from_home(&home).check_updates
}

/// 트레이 메뉴 로케일("en" | "ko") 조회 — `tray::setup`이 부팅 시 1회 읽는다. 파일 부재/파싱
/// 실패 또는 아직 저장된 적 없으면 `None`(트레이는 기본값 영어로 폴백, [`tray`] 모듈 참고).
pub fn load_locale() -> Option<String> {
    let home = dirs::home_dir().expect("home_dir 없음");
    load_root_config_from_home(&home).locale
}

/// `home`을 인자로 받는 버전 — 테스트에서 tempdir을 주입하기 위함(`save_check_updates_to_home`과
/// 동일 패턴). 기존 파일의 `capture`/`checkUpdates`는 먼저 로드해 그대로 보존한다(#7과 동일한
/// 클래스의 부분 갱신 원칙).
fn save_locale_to_home(home: &Path, locale: Option<String>) -> anyhow::Result<()> {
    let _guard = lock_config_writes();
    let mut root = load_root_config_from_home(home);
    root.locale = locale;
    save_root_config_to_home(home, &root)
}

/// 트레이 메뉴 로케일 저장(`set_app_locale` 커맨드가 사용). 재시작 후에만 트레이 메뉴에 반영된다
/// (FE Settings "언어" 섹션 안내 문구 참고).
pub fn save_locale(locale: Option<String>) -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home_dir 없음"))?;
    save_locale_to_home(&home, locale)
}

/// 일일 AI 요약(M7-①, ADR-0016) 설정 조회(`get_summary_config`/`generate_daily_summary` 커맨드가
/// 사용). 파일 부재/파싱 실패는 [`load_config`]와 동일하게 기본값(비활성)으로 폴백.
pub fn load_summary_config() -> SummaryConfig {
    let home = dirs::home_dir().expect("home_dir 없음");
    load_root_config_from_home(&home).summary
}

/// `home`을 인자로 받는 버전 — 테스트에서 tempdir을 주입하기 위함(`save_locale_to_home`과 동일
/// 패턴). 기존 파일의 `capture`/`checkUpdates`/`locale`은 먼저 로드해 그대로 보존한다(#7과 동일한
/// 클래스의 부분 갱신 원칙).
fn save_summary_config_to_home(home: &Path, cfg: &SummaryConfig) -> anyhow::Result<()> {
    let _guard = lock_config_writes();
    let mut root = load_root_config_from_home(home);
    root.summary = cfg.clone();
    save_root_config_to_home(home, &root)
}

/// 일일 AI 요약 설정 저장(`set_summary_config` 커맨드가 사용). 재시작 없이 다음 생성 요청부터
/// 반영된다(캡처 root 설정과 달리 감시 스레드 재기동이 필요 없음).
pub fn save_summary_config(cfg: &SummaryConfig) -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home_dir 없음"))?;
    save_summary_config_to_home(&home, cfg)
}

/// 평가 프로필 조회(`get_review_profile` 커맨드와 평가서 생성이 사용). 파일 부재/파싱 실패는
/// [`load_config`]와 동일하게 기본값(전부 비어 있음)으로 폴백하고, 읽은 값도 정규화해 돌려준다.
pub fn load_review_profile() -> ReviewProfile {
    let home = dirs::home_dir().expect("home_dir 없음");
    load_root_config_from_home(&home).review_profile.normalized()
}

/// `home`을 인자로 받는 버전(테스트용). 다른 최상위 필드는 먼저 로드해 보존하고, 읽기부터 쓰기까지
/// [`CONFIG_WRITE_LOCK`]을 잡는다 — 커넥터 폴러가 부팅 직후 캡처 설정을 되쓰는 것과 겹쳐도 한쪽
/// 변경이 다른 쪽 스냅샷에 덮이지 않게 하기 위함(#76과 같은 경합).
fn save_review_profile_to_home(home: &Path, profile: &ReviewProfile) -> anyhow::Result<()> {
    let _guard = lock_config_writes();
    let mut root = load_root_config_from_home(home);
    root.review_profile = profile.normalized();
    save_root_config_to_home(home, &root)
}

/// 평가 프로필 저장(`set_review_profile` 커맨드가 사용). 다음 평가서 생성부터 반영된다.
pub fn save_review_profile(profile: &ReviewProfile) -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home_dir 없음"))?;
    save_review_profile_to_home(&home, profile)
}

/// 캡처 시 body/metadata 절단 정책(watch.rs가 파일 처리 시작 전 한 번 읽어 정규화 함수들에 전달).
/// 파일 부재/파싱 실패는 [`load_config`]와 동일하게 기본값(essential)으로 폴백.
pub fn load_body_policy() -> BodyPolicy {
    load_config().body_policy
}

/// 시크릿 스크럽(M4) 사용 여부(watch.rs가 파일 처리 시작 전 한 번 읽어 ingest 직전 적용 여부를 결정).
/// 파일 부재/파싱 실패는 [`load_config`]와 동일하게 기본값(true, 스크럽 on)으로 폴백.
pub fn load_scrub_secrets() -> bool {
    load_config().scrub_secrets
}

/// 캡처 일시정지(M4) 여부의 초기값(`lib.rs` setup이 런타임 `Arc<AtomicBool>` 초기화에 한 번만 사용).
/// 파일 부재/파싱 실패는 [`load_config`]와 동일하게 기본값(false, 캡처 on)으로 폴백.
pub fn load_capture_paused() -> bool {
    load_config().capture_paused
}

/// 캡처 제외 프로젝트 목록(watch.rs가 파일 처리 시작 전 한 번 읽어 이후 재사용).
/// 파일 부재/파싱 실패는 [`load_config`]와 동일하게 기본값(빈 목록)으로 폴백.
pub fn load_exclude_projects() -> Vec<String> {
    load_config().exclude_projects
}

/// `project` 경로가 `exclude_projects` 중 하나의 prefix에 매치되는지 확인한다(watch.rs가 ingest
/// 직전 확인). 문자열 단순 `starts_with`가 아니라 경로 컴포넌트 단위(`Path::starts_with`)로
/// 비교한다 — 그렇지 않으면 `/foo/bar`가 `/foo/barbaz`에도 매치되는 오탐이 생긴다.
pub fn project_matches_exclude(project: &str, exclude_projects: &[String]) -> bool {
    let project_path = Path::new(project);
    exclude_projects.iter().any(|ex| project_path.starts_with(Path::new(ex)))
}

/// `~/.logroom/config.json`에 `RootConfig` 전체를 pretty JSON으로 저장한다. 디렉토리(`~/.logroom`)가
/// 없으면 생성한다. 같은 디렉토리의 임시 파일(`config.json.tmp`)에 먼저 쓴 뒤 `fs::rename`으로
/// 교체하는 원자적 쓰기 — 쓰는 도중 크래시/전원 손실이 나도 기존 설정 파일이 부분쓰기로 손상되지 않는다.
/// 디렉토리는 0700, 임시 파일(rename 전)은 0600으로 명시한다(slack 토큰 등 시크릿을 담으므로 —
/// db.rs/ingest.rs와 동일한 권한 강제 패턴). rename 후에도 파일 자체의 권한은 그대로 유지된다.
fn save_root_config_to_home(home: &Path, root: &RootConfig) -> anyhow::Result<()> {
    let dir = home.join(".logroom");
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let json = serde_json::to_string_pretty(root)?;
    let tmp_path = dir.join("config.json.tmp");
    fs::write(&tmp_path, json)?;
    fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp_path, config_path(home))?;
    Ok(())
}

/// `cfg`(캡처 설정)만 바꿔 저장한다([`update_config`]만 부른다 — 캡처 설정 쓰기는 전부 그 락을 거친다).
/// 기존 파일의 `checkUpdates`는
/// 먼저 로드해 그대로 보존한다 — 그렇지 않으면 캡처 설정을 저장할 때마다 업데이트 체크 설정이
/// 기본값으로 되돌아가는 유실 버그가 생긴다(#7 bodyPolicy 유실과 동일한 클래스의 실수).
fn save_config_to_home(home: &Path, cfg: &CaptureConfig) -> anyhow::Result<()> {
    let mut root = load_root_config_from_home(home);
    root.capture = cfg.clone();
    save_root_config_to_home(home, &root)
}

/// 캡처 설정 read-modify-write 직렬화 락([`update_config`]).
///
/// 커넥터 폴러는 계정마다 태스크가 따로 돌고, 부팅 직후 거의 동시에 토큰 검증 결과를 설정에 되쓴다.
/// 락 없이 각자 읽고 쓰면 **나중에 쓴 쪽이 먼저 쓴 쪽의 변경을 되돌린다**(읽은 시점의 스냅샷 전체를
/// 쓰므로) — 그 사이 사용자가 설정 화면에서 계정을 지웠다면 지운 계정이 되살아난다. 임시 파일
/// 이름(`config.json.tmp`)도 하나라 쓰기 자체가 섞일 수 있다.
static CONFIG_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// [`CONFIG_WRITE_LOCK`]을 잡는다. `config.json`은 파일 하나에 최상위 섹션이 여럿이고(capture·summary·
/// locale·checkUpdates·reviewProfile) 어느 섹션을 저장하든 파일 전체를 읽어 다시 쓴다 — 그래서 섹션이
/// 달라도 쓰기끼리는 같은 락으로 직렬화해야 한 쪽 스냅샷이 다른 쪽 변경을 되돌리지 않는다(설정 화면
/// "AI 요약"에 요약 설정과 평가 프로필 저장 버튼이 나란히 있다). 다른 스레드가 락을 쥔 채 panic해도
/// 설정 파일 자체는 원자적 쓰기라 온전하다 — 계속 진행한다.
fn lock_config_writes() -> std::sync::MutexGuard<'static, ()> {
    CONFIG_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 캡처 설정을 읽어 `update`로 고치고, `update`가 `true`를 돌려주면 저장한다 — 읽기부터 저장까지를
/// [`CONFIG_WRITE_LOCK`]으로 직렬화한다. 캡처 설정을 부분 갱신하는 경로(폴러의 검증 결과 저장,
/// `set_capture_config`, 트레이 일시정지)는 전부 이 함수를 거친다. 적용(자동 탐지/extraRoots 반영)은
/// 앱 재시작 후(`restart_app` 커맨드). 저장했으면 `true`.
pub fn update_config(update: impl FnOnce(&mut CaptureConfig) -> bool) -> anyhow::Result<bool> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home_dir 없음"))?;
    update_config_in_home(&home, update)
}

fn update_config_in_home(
    home: &Path,
    update: impl FnOnce(&mut CaptureConfig) -> bool,
) -> anyhow::Result<bool> {
    let _guard = lock_config_writes();
    let mut cfg = load_config_from_home(home);
    if !update(&mut cfg) {
        return Ok(false);
    }
    save_config_to_home(home, &cfg)?;
    Ok(true)
}

/// 자동 업데이트 백그라운드 체크 사용 여부를 저장한다. 기존 `capture` 설정은 먼저 로드해 그대로
/// 보존한다(위 [`save_config_to_home`]과 동일한 부분 갱신 원칙).
fn save_check_updates_to_home(home: &Path, enabled: bool) -> anyhow::Result<()> {
    let _guard = lock_config_writes();
    let mut root = load_root_config_from_home(home);
    root.check_updates = enabled;
    save_root_config_to_home(home, &root)
}

/// 자동 업데이트 백그라운드 체크 사용 여부를 저장한다(`set_check_updates` 커맨드가 사용). 재시작
/// 없이 다음 백그라운드 체크 주기부터 반영된다(`update::spawn_periodic_check`가 매 주기 재로드).
pub fn save_check_updates(enabled: bool) -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home_dir 없음"))?;
    save_check_updates_to_home(&home, enabled)
}

/// home 아래 `.claude*` 패턴 디렉토리(`.claude`, `.claude-a`, `.claude-b` 등) 중
/// `projects/` 하위 디렉토리가 있는 것만 root로 자동 탐지한다.
/// `.claude.json` 같은 **파일**은 `is_dir()` 필터로 제외된다.
fn detect_claude_roots(home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(home) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(".claude") {
            continue;
        }
        let projects = path.join("projects");
        if projects.is_dir() {
            out.push(projects);
        }
    }
    out.sort();
    out
}

fn kiro_cli_dir(home: &Path) -> PathBuf {
    home.join(".kiro").join("sessions").join("cli")
}

/// 중복 제거 기준 key. `canonicalize`(심볼릭 링크·상대 표기 차이를 흡수)를 시도하고,
/// 경로가 존재하지 않아 실패하면 원본 경로를 그대로 key로 쓴다.
fn canon_key(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `out`/`seen`에 root 하나를 추가한다(존재 여부와 무관하게 추가 — UI에서 부재 root도 보여줘야 함).
/// 중복 제거는 [`canon_key`] 기준.
fn add_resolved(out: &mut Vec<ResolvedRoot>, seen: &mut HashSet<PathBuf>, source: Source, path: PathBuf, origin: Origin) {
    let key = canon_key(&path);
    if seen.insert(key) {
        let exists = path.exists();
        out.push(ResolvedRoot { source, path, origin, exists });
    }
}

/// `home`을 인자로 받는 버전 — 테스트에서 tempdir을 주입하기 위함.
/// 소스가 `enabled = false`면 기본/커스텀 root를 모두 제외한다(root 0개 → `capture/health.rs`가
/// 자연히 `inactive`로 판정). FE는 이 목록이 아니라 `config.{claudeCode,kiroCli}.enabled`로
/// 체크박스 상태를 표시한다.
fn resolve_roots_detailed_with_home(home: &Path) -> Vec<ResolvedRoot> {
    let cfg = load_config_from_home(home);
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    if cfg.claude_code.enabled {
        if cfg.claude_code.use_defaults {
            for root in detect_claude_roots(home) {
                add_resolved(&mut out, &mut seen, Source::Claude, root, Origin::Default);
            }
        }
        for root in &cfg.claude_code.extra_roots {
            add_resolved(&mut out, &mut seen, Source::Claude, root.clone(), Origin::Custom);
        }
    }

    if cfg.kiro_cli.enabled {
        if cfg.kiro_cli.use_defaults {
            add_resolved(&mut out, &mut seen, Source::KiroCli, kiro_cli_dir(home), Origin::Default);
        }
        for root in &cfg.kiro_cli.extra_roots {
            add_resolved(&mut out, &mut seen, Source::KiroCli, root.clone(), Origin::Custom);
        }
    }

    out
}

/// 캡처 root 전체를 해석한다: 설정 파일(`~/.logroom/config.json`) 로드 →
/// 자동 탐지(useDefaults) + extraRoots → 존재 확인(`exists`) + 중복 제거.
/// 감시(watch)와 달리 부재 root도 포함한다 — `get_capture_config` 커맨드가 UI에 그대로 보여준다.
pub fn resolve_roots_detailed() -> Vec<ResolvedRoot> {
    let home = dirs::home_dir().expect("home_dir 없음");
    resolve_roots_detailed_with_home(&home)
}

/// `home`을 인자로 받는 버전 — 테스트에서 tempdir을 주입하기 위함.
fn resolve_roots_with_home(home: &Path) -> Vec<(Source, PathBuf)> {
    resolve_roots_detailed_with_home(home)
        .into_iter()
        .filter_map(|r| {
            if r.exists {
                return Some((r.source, r.path));
            }
            if r.origin == Origin::Custom {
                eprintln!("[logroom] extraRoots 경로 없음, 스킵: {}", r.path.display());
            }
            None
        })
        .collect()
}

/// 감시(watch) 대상 root 목록. [`resolve_roots_detailed`]에서 `exists == true`인 것만 남긴다
/// (watch.rs가 존재하지 않는 경로를 notify에 등록하면 에러가 나므로).
pub fn resolve_roots() -> Vec<(Source, PathBuf)> {
    let home = dirs::home_dir().expect("home_dir 없음");
    resolve_roots_with_home(&home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// `tempfile` 크레이트 없이 테스트용 임시 디렉토리를 만든다(새 크레이트 도입 금지 제약).
    /// Drop 시 디렉토리를 정리한다.
    struct TempHome {
        path: PathBuf,
    }

    impl TempHome {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("logroom-config-test-{}-{n}-{nanos}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_config(home: &Path, raw: &str) {
        let dir = home.join(".logroom");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.json"), raw).unwrap();
    }

    // ── 설정 파싱 ──────────────────────────────────────────────

    #[test]
    fn load_config_missing_file_returns_default() {
        let tmp = TempHome::new();
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.claude_code.use_defaults);
        assert!(cfg.claude_code.extra_roots.is_empty());
        assert!(cfg.kiro_cli.use_defaults);
        assert!(cfg.kiro_cli.extra_roots.is_empty());
    }

    #[test]
    fn load_config_empty_json_returns_default() {
        let tmp = TempHome::new();
        write_config(tmp.path(), "{}");
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.claude_code.use_defaults);
        assert!(cfg.kiro_cli.use_defaults);
    }

    #[test]
    fn load_config_broken_json_falls_back_to_default() {
        let tmp = TempHome::new();
        write_config(tmp.path(), "{ not valid json");
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.claude_code.use_defaults);
        assert!(cfg.kiro_cli.use_defaults);
    }

    #[test]
    fn load_config_partial_fields_use_defaults_for_missing() {
        let tmp = TempHome::new();
        write_config(
            tmp.path(),
            r#"{ "capture": { "claudeCode": { "useDefaults": false } } }"#,
        );
        let cfg = load_config_from_home(tmp.path());
        assert!(!cfg.claude_code.use_defaults);
        assert!(cfg.claude_code.extra_roots.is_empty());
        // kiroCli 자체가 없으므로 기본값 유지.
        assert!(cfg.kiro_cli.use_defaults);
    }

    #[test]
    fn load_config_full_fields_parse() {
        let tmp = TempHome::new();
        write_config(
            tmp.path(),
            r#"{
                "capture": {
                    "claudeCode": { "extraRoots": ["/tmp/foo/projects"], "useDefaults": true },
                    "kiroCli": { "extraRoots": ["/tmp/bar/cli"], "useDefaults": false }
                }
            }"#,
        );
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.claude_code.use_defaults);
        assert_eq!(cfg.claude_code.extra_roots, vec![PathBuf::from("/tmp/foo/projects")]);
        assert!(!cfg.kiro_cli.use_defaults);
        assert_eq!(cfg.kiro_cli.extra_roots, vec![PathBuf::from("/tmp/bar/cli")]);
    }

    #[test]
    fn save_config_then_load_config_round_trips() {
        let tmp = TempHome::new();
        let home = tmp.path();
        let cfg = CaptureConfig {
            claude_code: SourceConfig {
                extra_roots: vec![PathBuf::from("/tmp/foo/projects")],
                use_defaults: false,
                enabled: false,
            },
            kiro_cli: SourceConfig {
                extra_roots: vec![PathBuf::from("/tmp/bar/cli")],
                use_defaults: true,
                enabled: true,
            },
            body_policy: BodyPolicy::Full,
            slack: SlackConfig {
                token: Some("xoxp-test-token".to_string()),
                enabled: true,
                poll_minutes: 10,
            },
            github: GithubConfig {
                accounts: vec![GithubAccount {
                    token: "ghp_test_token".to_string(),
                    username: Some("octocat".to_string()),
                }],
                enabled: true,
                poll_minutes: 5,
            },
            linear: LinearConfig {
                accounts: vec![LinearAccount {
                    token: "lin_api_test_token".to_string(),
                    viewer_id: Some("viewer-1".to_string()),
                    viewer_name: Some("Ada".to_string()),
                }],
                enabled: true,
                poll_minutes: 5,
            },
            notion: NotionConfig {
                accounts: vec![NotionAccount {
                    token: "ntn_test_token".to_string(),
                    id: Some("account-1".to_string()),
                    label: Some("회사".to_string()),
                    kind: Some(NotionTokenKind::Integration),
                    user_name: None,
                    workspace_id: Some("workspace-1".to_string()),
                    workspace_name: Some("Acme".to_string()),
                }],
                enabled: true,
                poll_minutes: 5,
            },
            scrub_secrets: false,
            capture_paused: true,
            exclude_projects: vec!["/tmp/excluded/project".to_string()],
        };

        save_config_to_home(home, &cfg).unwrap();
        let loaded = load_config_from_home(home);
        assert_eq!(loaded, cfg);

        // 파일 shape도 `{"capture": ...}` 래퍼(camelCase) 그대로인지 확인.
        let raw = fs::read_to_string(config_path(home)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["capture"]["claudeCode"]["useDefaults"], false);
        assert_eq!(value["capture"]["claudeCode"]["enabled"], false);
        assert_eq!(value["capture"]["kiroCli"]["useDefaults"], true);
        assert_eq!(value["capture"]["kiroCli"]["enabled"], true);
        assert_eq!(value["capture"]["bodyPolicy"], "full");
        assert_eq!(value["capture"]["slack"]["token"], "xoxp-test-token");
        assert_eq!(value["capture"]["slack"]["enabled"], true);
        assert_eq!(value["capture"]["slack"]["pollMinutes"], 10);
        assert_eq!(value["capture"]["github"]["accounts"][0]["token"], "ghp_test_token");
        assert_eq!(value["capture"]["github"]["accounts"][0]["username"], "octocat");
        assert_eq!(value["capture"]["github"]["enabled"], true);
        assert_eq!(value["capture"]["github"]["pollMinutes"], 5);
        assert_eq!(value["capture"]["linear"]["accounts"][0]["token"], "lin_api_test_token");
        assert_eq!(value["capture"]["linear"]["accounts"][0]["viewerId"], "viewer-1");
        assert_eq!(value["capture"]["linear"]["accounts"][0]["viewerName"], "Ada");
        assert_eq!(value["capture"]["linear"]["enabled"], true);
        assert_eq!(value["capture"]["linear"]["pollMinutes"], 5);
        assert_eq!(
            value["capture"]["notion"]["accounts"][0]["token"],
            "ntn_test_token"
        );
        assert_eq!(
            value["capture"]["notion"]["accounts"][0]["workspaceId"],
            "workspace-1"
        );
        assert_eq!(
            value["capture"]["notion"]["accounts"][0]["workspaceName"],
            "Acme"
        );
        assert_eq!(value["capture"]["notion"]["accounts"][0]["id"], "account-1");
        assert_eq!(value["capture"]["notion"]["accounts"][0]["label"], "회사");
        assert_eq!(
            value["capture"]["notion"]["accounts"][0]["kind"],
            "integration"
        );
        assert_eq!(value["capture"]["notion"]["enabled"], true);
        assert_eq!(value["capture"]["notion"]["pollMinutes"], 5);
        assert_eq!(value["capture"]["scrubSecrets"], false);
        assert_eq!(value["capture"]["capturePaused"], true);
        assert_eq!(value["capture"]["excludeProjects"][0], "/tmp/excluded/project");
    }

    // ── slack 커넥터(v1, docs/08-connectors.md) ──────────────────

    #[test]
    fn load_config_missing_slack_defaults_to_disabled_with_no_token() {
        let tmp = TempHome::new();
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.slack, SlackConfig::default());
        assert!(!cfg.slack.enabled);
        assert_eq!(cfg.slack.token, None);
        assert_eq!(cfg.slack.poll_minutes, 5);
    }

    #[test]
    fn load_config_parses_slack_fields() {
        let tmp = TempHome::new();
        write_config(
            tmp.path(),
            r#"{ "capture": { "slack": { "token": "xoxp-abc", "enabled": true, "pollMinutes": 15 } } }"#,
        );
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.slack.token.as_deref(), Some("xoxp-abc"));
        assert!(cfg.slack.enabled);
        assert_eq!(cfg.slack.poll_minutes, 15);
    }

    #[test]
    fn load_config_slack_partial_fields_use_defaults_for_missing() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "capture": { "slack": { "enabled": true } } }"#);
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.slack.enabled);
        assert_eq!(cfg.slack.token, None);
        assert_eq!(cfg.slack.poll_minutes, 5, "pollMinutes 누락 시 기본값 5 유지돼야 함");
    }

    // ── github 커넥터(v1, docs/08-connectors.md) ──────────────────

    #[test]
    fn load_config_missing_github_defaults_to_disabled_with_no_accounts() {
        let tmp = TempHome::new();
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.github, GithubConfig::default());
        assert!(!cfg.github.enabled);
        assert!(cfg.github.accounts.is_empty());
        assert_eq!(cfg.github.poll_minutes, 5);
    }

    #[test]
    fn load_config_parses_github_fields() {
        let tmp = TempHome::new();
        write_config(
            tmp.path(),
            r#"{ "capture": { "github": {
                "accounts": [
                    { "token": "ghp_abc", "username": "octocat" },
                    { "token": "ghp_def" }
                ],
                "enabled": true,
                "pollMinutes": 15
            } } }"#,
        );
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.github.accounts.len(), 2);
        assert_eq!(cfg.github.accounts[0].token, "ghp_abc");
        assert_eq!(cfg.github.accounts[0].username.as_deref(), Some("octocat"));
        assert_eq!(cfg.github.accounts[1].token, "ghp_def");
        assert_eq!(cfg.github.accounts[1].username, None);
        assert!(cfg.github.enabled);
        assert_eq!(cfg.github.poll_minutes, 15);
    }

    #[test]
    fn load_config_github_partial_fields_use_defaults_for_missing() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "capture": { "github": { "enabled": true } } }"#);
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.github.enabled);
        assert!(cfg.github.accounts.is_empty());
        assert_eq!(cfg.github.poll_minutes, 5, "pollMinutes 누락 시 기본값 5 유지돼야 함");
    }

    // ── github 토큰 마스킹(FE 왕복 보호, 보안 리뷰) ───────────────

    #[test]
    fn mask_github_token_keeps_last_four_chars() {
        assert_eq!(mask_github_token("ghp_1234567890abcdef"), "••••cdef");
    }

    #[test]
    fn is_masked_github_token_detects_own_output() {
        let masked = mask_github_token("ghp_abcdefgh");
        assert!(is_masked_github_token(&masked));
        assert!(!is_masked_github_token("ghp_abcdefgh"));
    }

    fn account(token: &str, username: Option<&str>) -> GithubAccount {
        GithubAccount {
            token: token.to_string(),
            username: username.map(str::to_string),
        }
    }

    #[test]
    fn resolve_github_accounts_update_new_token_replaces_and_trims() {
        let previous = vec![account("ghp_original", Some("octocat"))];
        let incoming = vec![account("  ghp_new_token  ", Some("octocat"))];
        let resolved = resolve_github_accounts_update(&previous, incoming);
        assert_eq!(resolved, vec![account("ghp_new_token", Some("octocat"))]);
    }

    #[test]
    fn resolve_github_accounts_update_blank_new_token_removes_account() {
        let previous = vec![account("ghp_original", Some("octocat"))];
        let incoming = vec![account("   ", Some("octocat"))];
        let resolved = resolve_github_accounts_update(&previous, incoming);
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_github_accounts_update_masked_value_preserves_previous_token_by_username() {
        // 계정이 삭제돼 배열 순서가 바뀌어도(인덱스 0이던 계정이 없어짐) username으로 정확히
        // 매칭돼야 한다 — 인덱스만 봤다면 다른 계정의 토큰과 잘못 매칭될 위험이 있다.
        let previous = vec![
            account("ghp_alice_token", Some("alice")),
            account("ghp_bob_token", Some("bob")),
        ];
        let masked_bob = mask_github_token("ghp_bob_token");
        // alice 계정이 삭제된 채로 들어옴 — bob만 남았지만 인덱스는 0.
        let incoming = vec![account(&masked_bob, Some("bob"))];
        let resolved = resolve_github_accounts_update(&previous, incoming);
        assert_eq!(resolved, vec![account("ghp_bob_token", Some("bob"))]);
    }

    #[test]
    fn resolve_github_accounts_update_masked_value_falls_back_to_index_when_username_unresolved() {
        // 폴러가 아직 /user 검증에 성공하지 못해 username이 None인 상태 — 인덱스로 폴백 매칭.
        let previous = vec![account("ghp_pending_token", None)];
        let masked = mask_github_token("ghp_pending_token");
        let incoming = vec![account(&masked, None)];
        let resolved = resolve_github_accounts_update(&previous, incoming);
        assert_eq!(resolved, vec![account("ghp_pending_token", None)]);
    }

    #[test]
    fn resolve_github_accounts_update_unmatched_masked_value_is_dropped() {
        // 매칭되는 이전 계정이 전혀 없는데 마스킹 값이 들어온 비정상 상황 — 마스킹 값을 그대로
        // 저장해 토큰을 훼손하는 대신 계정을 제거한다.
        let masked = mask_github_token("ghp_unknown_token");
        let incoming = vec![account(&masked, Some("ghost"))];
        let resolved = resolve_github_accounts_update(&[], incoming);
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_github_accounts_update_unverified_accounts_survive_deleting_the_first() {
        // 회귀 방지: 미검증 계정 A·B 중 A를 지우면 남은 B가 인덱스 0으로 내려와 previous[0](= A)에
        // 매칭됐다 — 지운 A가 부활하고 B가 사라졌다. 마스킹 값으로 먼저 찾아 B를 따라가야 한다.
        let previous = vec![account("ghp_aaaa1111", None), account("ghp_bbbb2222", None)];
        let incoming = vec![account(&mask_github_token("ghp_bbbb2222"), None)];
        let resolved = resolve_github_accounts_update(&previous, incoming);
        assert_eq!(resolved, vec![account("ghp_bbbb2222", None)]);
    }

    #[test]
    fn resolve_github_accounts_update_ambiguous_mask_falls_back_to_index() {
        // 마지막 4자가 같은 토큰 둘 — 마스킹 값으로는 구분할 수 없어 인덱스로 맞춘다.
        let previous = vec![account("ghp_first_9999", None), account("ghp_second_9999", None)];
        let masked = mask_github_token("ghp_first_9999");
        assert_eq!(masked, mask_github_token("ghp_second_9999"));
        let incoming = vec![account(&masked, None), account(&masked, None)];
        let resolved = resolve_github_accounts_update(&previous, incoming);
        assert_eq!(resolved, previous);
    }

    #[test]
    fn resolve_github_accounts_update_multiple_accounts_mixed_new_and_masked() {
        let previous = vec![
            account("ghp_alice_token", Some("alice")),
            account("ghp_bob_token", Some("bob")),
        ];
        let masked_alice = mask_github_token("ghp_alice_token");
        let incoming = vec![
            account(&masked_alice, Some("alice")),
            account("ghp_new_carol_token", None),
        ];
        let resolved = resolve_github_accounts_update(&previous, incoming);
        assert_eq!(
            resolved,
            vec![
                account("ghp_alice_token", Some("alice")),
                account("ghp_new_carol_token", None),
            ]
        );
    }

    // ── linear 커넥터(v1, docs/08-connectors.md) ──────────────────

    #[test]
    fn load_config_missing_linear_defaults_to_disabled_with_no_accounts() {
        let tmp = TempHome::new();
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.linear, LinearConfig::default());
        assert!(!cfg.linear.enabled);
        assert!(cfg.linear.accounts.is_empty());
        assert_eq!(cfg.linear.poll_minutes, 5);
    }

    #[test]
    fn load_config_parses_linear_fields() {
        let tmp = TempHome::new();
        write_config(
            tmp.path(),
            r#"{ "capture": { "linear": {
                "accounts": [
                    { "token": "lin_api_abc", "viewerId": "v1", "viewerName": "Ada" },
                    { "token": "lin_api_def" }
                ],
                "enabled": true,
                "pollMinutes": 15
            } } }"#,
        );
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.linear.accounts.len(), 2);
        assert_eq!(cfg.linear.accounts[0].token, "lin_api_abc");
        assert_eq!(cfg.linear.accounts[0].viewer_id.as_deref(), Some("v1"));
        assert_eq!(cfg.linear.accounts[0].viewer_name.as_deref(), Some("Ada"));
        assert_eq!(cfg.linear.accounts[1].token, "lin_api_def");
        assert_eq!(cfg.linear.accounts[1].viewer_id, None);
        assert!(cfg.linear.enabled);
        assert_eq!(cfg.linear.poll_minutes, 15);
    }

    #[test]
    fn load_config_linear_partial_fields_use_defaults_for_missing() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "capture": { "linear": { "enabled": true } } }"#);
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.linear.enabled);
        assert!(cfg.linear.accounts.is_empty());
        assert_eq!(cfg.linear.poll_minutes, 5, "pollMinutes 누락 시 기본값 5 유지돼야 함");
    }

    // ── linear 토큰 마스킹(FE 왕복 보호, 보안 리뷰) ───────────────

    #[test]
    fn mask_linear_token_keeps_last_four_chars() {
        assert_eq!(mask_linear_token("lin_api_1234567890abcdef"), "••••cdef");
    }

    #[test]
    fn is_masked_linear_token_detects_own_output() {
        let masked = mask_linear_token("lin_api_abcdefgh");
        assert!(is_masked_linear_token(&masked));
        assert!(!is_masked_linear_token("lin_api_abcdefgh"));
    }

    fn linear_account(token: &str, viewer_id: Option<&str>, viewer_name: Option<&str>) -> LinearAccount {
        LinearAccount {
            token: token.to_string(),
            viewer_id: viewer_id.map(str::to_string),
            viewer_name: viewer_name.map(str::to_string),
        }
    }

    #[test]
    fn resolve_linear_accounts_update_new_token_replaces_and_trims() {
        let previous = vec![linear_account("lin_api_original", Some("v1"), Some("Ada"))];
        let incoming = vec![linear_account("  lin_api_new_token  ", Some("v1"), Some("Ada"))];
        let resolved = resolve_linear_accounts_update(&previous, incoming);
        assert_eq!(resolved, vec![linear_account("lin_api_new_token", Some("v1"), Some("Ada"))]);
    }

    #[test]
    fn resolve_linear_accounts_update_blank_new_token_removes_account() {
        let previous = vec![linear_account("lin_api_original", Some("v1"), Some("Ada"))];
        let incoming = vec![linear_account("   ", Some("v1"), Some("Ada"))];
        let resolved = resolve_linear_accounts_update(&previous, incoming);
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_linear_accounts_update_masked_value_preserves_previous_token_by_viewer_id() {
        // 계정이 삭제돼 배열 순서가 바뀌어도(인덱스 0이던 계정이 없어짐) viewerId로 정확히
        // 매칭돼야 한다 — 인덱스만 봤다면 다른 계정의 토큰과 잘못 매칭될 위험이 있다.
        let previous = vec![
            linear_account("lin_api_alice_token", Some("v-alice"), Some("Alice")),
            linear_account("lin_api_bob_token", Some("v-bob"), Some("Bob")),
        ];
        let masked_bob = mask_linear_token("lin_api_bob_token");
        let incoming = vec![linear_account(&masked_bob, Some("v-bob"), Some("Bob"))];
        let resolved = resolve_linear_accounts_update(&previous, incoming);
        assert_eq!(resolved, vec![linear_account("lin_api_bob_token", Some("v-bob"), Some("Bob"))]);
    }

    #[test]
    fn resolve_linear_accounts_update_masked_value_falls_back_to_index_when_viewer_id_unresolved() {
        // 폴러가 아직 viewer 검증에 성공하지 못해 viewerId가 None인 상태 — 인덱스로 폴백 매칭.
        let previous = vec![linear_account("lin_api_pending_token", None, None)];
        let masked = mask_linear_token("lin_api_pending_token");
        let incoming = vec![linear_account(&masked, None, None)];
        let resolved = resolve_linear_accounts_update(&previous, incoming);
        assert_eq!(resolved, vec![linear_account("lin_api_pending_token", None, None)]);
    }

    #[test]
    fn resolve_linear_accounts_update_unmatched_masked_value_is_dropped() {
        let masked = mask_linear_token("lin_api_unknown_token");
        let incoming = vec![linear_account(&masked, Some("ghost"), None)];
        let resolved = resolve_linear_accounts_update(&[], incoming);
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_linear_accounts_update_unverified_accounts_survive_deleting_the_first() {
        // 회귀 방지: 미검증 계정 A·B 중 A를 지우면 남은 B가 인덱스 0으로 내려와 previous[0](= A)에
        // 매칭됐다 — 지운 A가 부활하고 B가 사라졌다. 마스킹 값으로 먼저 찾아 B를 따라가야 한다.
        let previous = vec![
            linear_account("lin_api_aaaa1111", None, None),
            linear_account("lin_api_bbbb2222", None, None),
        ];
        let incoming = vec![linear_account(&mask_linear_token("lin_api_bbbb2222"), None, None)];
        let resolved = resolve_linear_accounts_update(&previous, incoming);
        assert_eq!(resolved, vec![linear_account("lin_api_bbbb2222", None, None)]);
    }

    #[test]
    fn resolve_linear_accounts_update_ambiguous_mask_falls_back_to_index() {
        // 마지막 4자가 같은 토큰 둘 — 마스킹 값으로는 구분할 수 없어 인덱스로 맞춘다.
        let previous = vec![
            linear_account("lin_api_first_9999", None, None),
            linear_account("lin_api_second_9999", None, None),
        ];
        let masked = mask_linear_token("lin_api_first_9999");
        assert_eq!(masked, mask_linear_token("lin_api_second_9999"));
        let incoming = vec![
            linear_account(&masked, None, None),
            linear_account(&masked, None, None),
        ];
        let resolved = resolve_linear_accounts_update(&previous, incoming);
        assert_eq!(resolved, previous);
    }

    // ── notion 커넥터(v1, docs/08-connectors.md "Notion v1") ──────

    #[test]
    fn load_config_missing_notion_defaults_to_disabled_with_no_accounts() {
        let tmp = TempHome::new();
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.notion, NotionConfig::default());
        assert!(!cfg.notion.enabled);
        assert!(cfg.notion.accounts.is_empty());
        assert_eq!(cfg.notion.poll_minutes, 5);
    }

    #[test]
    fn load_config_parses_notion_fields() {
        let tmp = TempHome::new();
        write_config(
            tmp.path(),
            r#"{ "capture": { "notion": {
                "accounts": [
                    { "token": "ntn_abc", "id": "id-1", "label": "회사", "kind": "integration",
                      "workspaceId": "w1", "workspaceName": "Acme" },
                    { "token": "ntn_def", "kind": "personal", "userName": "alex" }
                ],
                "enabled": true,
                "pollMinutes": 15
            } } }"#,
        );
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.notion.accounts.len(), 2);
        assert_eq!(cfg.notion.accounts[0].token, "ntn_abc");
        assert_eq!(cfg.notion.accounts[0].workspace_id.as_deref(), Some("w1"));
        assert_eq!(
            cfg.notion.accounts[0].workspace_name.as_deref(),
            Some("Acme")
        );
        assert_eq!(cfg.notion.accounts[0].id.as_deref(), Some("id-1"));
        assert_eq!(cfg.notion.accounts[0].label.as_deref(), Some("회사"));
        assert_eq!(
            cfg.notion.accounts[0].kind,
            Some(NotionTokenKind::Integration)
        );
        assert_eq!(cfg.notion.accounts[1].token, "ntn_def");
        assert_eq!(cfg.notion.accounts[1].id, None);
        assert_eq!(cfg.notion.accounts[1].kind, Some(NotionTokenKind::Personal));
        assert_eq!(cfg.notion.accounts[1].user_name.as_deref(), Some("alex"));
        assert_eq!(cfg.notion.accounts[1].workspace_id, None);
        assert!(cfg.notion.enabled);
        assert_eq!(cfg.notion.poll_minutes, 15);
    }

    #[test]
    fn load_config_notion_partial_fields_use_defaults_for_missing() {
        let tmp = TempHome::new();
        write_config(
            tmp.path(),
            r#"{ "capture": { "notion": { "enabled": true } } }"#,
        );
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.notion.enabled);
        assert!(cfg.notion.accounts.is_empty());
        assert_eq!(
            cfg.notion.poll_minutes, 5,
            "pollMinutes 누락 시 기본값 5 유지돼야 함"
        );
    }

    // ── notion 토큰 마스킹(FE 왕복 보호, 보안 리뷰) ────────────────

    #[test]
    fn mask_notion_token_keeps_last_four_chars() {
        assert_eq!(mask_notion_token("ntn_1234567890abcdef"), "••••cdef");
    }

    #[test]
    fn is_masked_notion_token_detects_own_output() {
        let masked = mask_notion_token("ntn_abcdefgh");
        assert!(is_masked_notion_token(&masked));
        assert!(!is_masked_notion_token("ntn_abcdefgh"));
    }

    fn notion_account(token: &str, id: Option<&str>, workspace_id: Option<&str>) -> NotionAccount {
        NotionAccount {
            token: token.to_string(),
            id: id.map(str::to_string),
            workspace_id: workspace_id.map(str::to_string),
            ..NotionAccount::default()
        }
    }

    fn notion_tokens(accounts: &[NotionAccount]) -> Vec<&str> {
        accounts.iter().map(|a| a.token.as_str()).collect()
    }

    #[test]
    fn resolve_notion_accounts_update_new_token_replaces_and_trims_keeping_id() {
        let previous = vec![notion_account("ntn_original", Some("id-1"), Some("w1"))];
        let incoming = vec![notion_account(
            "  ntn_new_token  ",
            Some("id-1"),
            Some("w1"),
        )];
        let resolved = resolve_notion_accounts_update(&previous, incoming);
        assert_eq!(
            resolved,
            vec![notion_account("ntn_new_token", Some("id-1"), Some("w1"))]
        );
    }

    #[test]
    fn resolve_notion_accounts_update_new_accounts_get_distinct_ids() {
        // FE가 새로 추가한 계정은 id 없이 들어온다 — 저장 시점에 부여해야 커서 키와 이후 왕복
        // 매칭이 검증 여부와 무관하게 안정적이다.
        let incoming = vec![
            notion_account("ntn_personal", None, None),
            notion_account("ntn_company", Some(""), None),
        ];
        let resolved = resolve_notion_accounts_update(&[], incoming);
        let ids: Vec<&str> = resolved.iter().filter_map(|a| a.id.as_deref()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| !id.is_empty()));
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn resolve_notion_accounts_update_blank_new_token_removes_account() {
        let previous = vec![notion_account("ntn_original", Some("id-1"), Some("w1"))];
        let incoming = vec![notion_account("   ", Some("id-1"), Some("w1"))];
        let resolved = resolve_notion_accounts_update(&previous, incoming);
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_notion_accounts_update_masked_value_matches_by_id_after_reorder() {
        // 같은 사람이 개인·회사 워크스페이스에 PAT를 하나씩 만든 경우 — users/me가 같은 사람을
        // 돌려주고 워크스페이스 id도 없다. id로만 구분된다. 앞 계정을 지워 배열이 당겨져도 따라가야 한다.
        let previous = vec![
            notion_account("ntn_personal_token", Some("id-personal"), None),
            notion_account("ntn_company_token", Some("id-company"), None),
        ];
        let masked_company = mask_notion_token("ntn_company_token");
        let incoming = vec![notion_account(&masked_company, Some("id-company"), None)];
        let resolved = resolve_notion_accounts_update(&previous, incoming);
        assert_eq!(resolved, vec![previous[1].clone()]);
    }

    #[test]
    fn resolve_notion_accounts_update_masked_value_keeps_verified_fields_and_takes_incoming_label()
    {
        // 폴러가 채운 필드(kind/userName)는 원본을 유지하고, 사용자가 붙이는 label은 incoming 값을 쓴다.
        let previous = vec![NotionAccount {
            token: "ntn_company_token".to_string(),
            id: Some("id-company".to_string()),
            label: Some("회사".to_string()),
            kind: Some(NotionTokenKind::Personal),
            user_name: Some("alex".to_string()),
            ..NotionAccount::default()
        }];
        let incoming = vec![NotionAccount {
            token: mask_notion_token("ntn_company_token"),
            id: Some("id-company".to_string()),
            label: Some("  회사 노션  ".to_string()),
            ..NotionAccount::default()
        }];
        let resolved = resolve_notion_accounts_update(&previous, incoming);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].token, "ntn_company_token");
        assert_eq!(resolved[0].label.as_deref(), Some("회사 노션"));
        assert_eq!(resolved[0].kind, Some(NotionTokenKind::Personal));
        assert_eq!(resolved[0].user_name.as_deref(), Some("alex"));
    }

    #[test]
    fn resolve_notion_accounts_update_blank_label_becomes_none() {
        let incoming = vec![NotionAccount {
            token: "ntn_new".to_string(),
            label: Some("   ".to_string()),
            ..NotionAccount::default()
        }];
        let resolved = resolve_notion_accounts_update(&[], incoming);
        assert_eq!(resolved[0].label, None);
    }

    #[test]
    fn resolve_notion_accounts_update_legacy_account_matches_by_workspace_id_and_gets_id() {
        // id 도입 전에 저장된 계정 — workspaceId로 매칭하고, 이번 저장에서 id를 부여한다.
        let previous = vec![
            notion_account("ntn_alice_token", None, Some("w-alice")),
            notion_account("ntn_bob_token", None, Some("w-bob")),
        ];
        let masked_bob = mask_notion_token("ntn_bob_token");
        let incoming = vec![notion_account(&masked_bob, None, Some("w-bob"))];
        let resolved = resolve_notion_accounts_update(&previous, incoming);
        assert_eq!(notion_tokens(&resolved), vec!["ntn_bob_token"]);
        assert!(resolved[0].id.is_some());
    }

    #[test]
    fn resolve_notion_accounts_update_masked_value_matches_by_mask_when_index_would_be_wrong() {
        // 회귀 방지(코드 리뷰 지적): 식별 키가 없는 계정 A·B를 등록해 둔 상태에서 A를 삭제하면, 남은
        // B가 인덱스 0으로 내려와 previous[0](= A)에 잘못 매칭됐다 — 저장하고 나면 지우려던 A의
        // 토큰이 부활하고 남기려던 B가 사라진다. 마스킹 값으로 먼저 매칭하면 B를 정확히 따라간다.
        let previous = vec![
            notion_account("ntn_aaaa1111", None, None),
            notion_account("ntn_bbbb2222", None, None),
        ];
        let masked_b = mask_notion_token("ntn_bbbb2222");
        let incoming = vec![notion_account(&masked_b, None, None)];
        let resolved = resolve_notion_accounts_update(&previous, incoming);
        assert_eq!(notion_tokens(&resolved), vec!["ntn_bbbb2222"]);
    }

    #[test]
    fn resolve_notion_accounts_update_ambiguous_mask_falls_back_to_index() {
        // 마스킹 값은 "•••• + 마지막 4자"라 서로 다른 토큰이 같은 마스킹 값을 가질 수 있다 — 그때는
        // 어느 계정인지 알 수 없으므로 마스킹 매칭을 건너뛰고 인덱스 폴백에 맡긴다.
        let previous = vec![
            notion_account("ntn_first_9999", None, None),
            notion_account("ntn_second_9999", None, None),
        ];
        let masked = mask_notion_token("ntn_first_9999");
        assert_eq!(masked, mask_notion_token("ntn_second_9999"));
        let incoming = vec![
            notion_account(&masked, None, None),
            notion_account(&masked, None, None),
        ];
        let resolved = resolve_notion_accounts_update(&previous, incoming);
        assert_eq!(
            notion_tokens(&resolved),
            vec!["ntn_first_9999", "ntn_second_9999"]
        );
    }

    #[test]
    fn resolve_notion_accounts_update_unmatched_masked_value_is_dropped() {
        let masked = mask_notion_token("ntn_unknown_token");
        let incoming = vec![notion_account(&masked, Some("ghost"), Some("ghost"))];
        let resolved = resolve_notion_accounts_update(&[], incoming);
        assert!(resolved.is_empty());
    }

    #[test]
    fn assign_missing_notion_account_ids_fills_only_missing() {
        let mut accounts = vec![
            notion_account("ntn_a", Some("id-a"), None),
            notion_account("ntn_b", None, None),
            notion_account("ntn_c", Some(""), None),
        ];
        assert!(assign_missing_notion_account_ids(&mut accounts));
        assert_eq!(accounts[0].id.as_deref(), Some("id-a"));
        assert!(accounts[1].id.as_deref().is_some_and(|id| !id.is_empty()));
        assert!(accounts[2].id.as_deref().is_some_and(|id| !id.is_empty()));
        assert!(!assign_missing_notion_account_ids(&mut accounts));
    }

    #[test]
    fn assign_missing_notion_account_ids_replaces_duplicate_id() {
        // id가 겹치면 두 계정이 커서 하나를 나눠 쓴다 — 앞 계정은 두고 뒤 계정만 바꾼다.
        let mut accounts = vec![
            notion_account("ntn_a", Some("id-dup"), None),
            notion_account("ntn_b", Some("id-dup"), None),
        ];
        assert!(assign_missing_notion_account_ids(&mut accounts));
        assert_eq!(accounts[0].id.as_deref(), Some("id-dup"));
        assert_ne!(accounts[1].id.as_deref(), Some("id-dup"));
    }

    // ── 설정 쓰기 직렬화(update_config) ─────────────────────────────

    #[test]
    fn update_config_in_home_saves_only_when_changed() {
        let tmp = TempHome::new();
        let saved = update_config_in_home(tmp.path(), |cfg| {
            cfg.linear.poll_minutes = 9;
            true
        })
        .unwrap();
        assert!(saved);
        assert_eq!(load_config_from_home(tmp.path()).linear.poll_minutes, 9);

        let saved = update_config_in_home(tmp.path(), |cfg| {
            cfg.linear.poll_minutes = 1;
            false
        })
        .unwrap();
        assert!(!saved, "false를 돌려주면 저장하지 않아야 함");
        assert_eq!(load_config_from_home(tmp.path()).linear.poll_minutes, 9);
    }

    // ── bodyPolicy(ADR-0012) ───────────────────────────────────

    #[test]
    fn load_config_missing_body_policy_defaults_to_essential() {
        let tmp = TempHome::new();
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.body_policy, BodyPolicy::Essential);
    }

    #[test]
    fn load_config_parses_full_body_policy() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "capture": { "bodyPolicy": "full" } }"#);
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.body_policy, BodyPolicy::Full);
    }

    #[test]
    fn load_config_parses_essential_body_policy_explicitly() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "capture": { "bodyPolicy": "essential" } }"#);
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(cfg.body_policy, BodyPolicy::Essential);
    }

    // ── scrubSecrets(M4) ───────────────────────────────────────

    #[test]
    fn load_config_missing_scrub_secrets_defaults_to_true() {
        let tmp = TempHome::new();
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.scrub_secrets);
    }

    #[test]
    fn load_config_parses_scrub_secrets_false() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "capture": { "scrubSecrets": false } }"#);
        let cfg = load_config_from_home(tmp.path());
        assert!(!cfg.scrub_secrets);
    }

    // ── capturePaused(M4) ──────────────────────────────────────

    #[test]
    fn load_config_missing_capture_paused_defaults_to_false() {
        let tmp = TempHome::new();
        let cfg = load_config_from_home(tmp.path());
        assert!(!cfg.capture_paused);
    }

    #[test]
    fn load_config_parses_capture_paused_true() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "capture": { "capturePaused": true } }"#);
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.capture_paused);
    }

    // ── sourceConfig.enabled(소스 on/off) ───────────────────────

    #[test]
    fn load_config_missing_enabled_defaults_to_true() {
        let tmp = TempHome::new();
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.claude_code.enabled);
        assert!(cfg.kiro_cli.enabled);
    }

    #[test]
    fn load_config_parses_enabled_false() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "capture": { "claudeCode": { "enabled": false } } }"#);
        let cfg = load_config_from_home(tmp.path());
        assert!(!cfg.claude_code.enabled);
        // kiroCli는 건드리지 않았으므로 기본값(true) 유지.
        assert!(cfg.kiro_cli.enabled);
    }

    // ── excludeProjects(프로젝트 제외) ───────────────────────────

    #[test]
    fn load_config_missing_exclude_projects_defaults_to_empty() {
        let tmp = TempHome::new();
        let cfg = load_config_from_home(tmp.path());
        assert!(cfg.exclude_projects.is_empty());
    }

    #[test]
    fn load_config_parses_exclude_projects() {
        let tmp = TempHome::new();
        write_config(
            tmp.path(),
            r#"{ "capture": { "excludeProjects": ["/tmp/secret-project", "/tmp/another"] } }"#,
        );
        let cfg = load_config_from_home(tmp.path());
        assert_eq!(
            cfg.exclude_projects,
            vec!["/tmp/secret-project".to_string(), "/tmp/another".to_string()]
        );
    }

    #[test]
    fn project_matches_exclude_matches_prefix_by_path_component() {
        let exclude = vec!["/tmp/secret-project".to_string()];
        // 정확히 일치.
        assert!(project_matches_exclude("/tmp/secret-project", &exclude));
        // 하위 디렉토리도 매치.
        assert!(project_matches_exclude("/tmp/secret-project/nested", &exclude));
        // 컴포넌트 단위 비교라 이름이 비슷한 다른 디렉토리는 매치되지 않아야 함
        // (문자열 단순 prefix면 오탐: "/tmp/secret-project-2".starts_with("/tmp/secret-project") == true).
        assert!(!project_matches_exclude("/tmp/secret-project-2", &exclude));
        // 전혀 다른 경로.
        assert!(!project_matches_exclude("/tmp/other", &exclude));
    }

    #[test]
    fn project_matches_exclude_empty_list_never_matches() {
        assert!(!project_matches_exclude("/tmp/anything", &[]));
    }

    // ── root 자동 탐지 / 해석 ──────────────────────────────────

    #[test]
    fn detects_multiple_claude_star_projects_dirs() {
        let tmp = TempHome::new();
        let home = tmp.path();
        fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        fs::create_dir_all(home.join(".claude-a").join("projects")).unwrap();
        fs::create_dir_all(home.join(".claude-b").join("projects")).unwrap();

        let roots = detect_claude_roots(home);
        assert_eq!(roots.len(), 3);
        assert!(roots.contains(&home.join(".claude").join("projects")));
        assert!(roots.contains(&home.join(".claude-a").join("projects")));
        assert!(roots.contains(&home.join(".claude-b").join("projects")));
    }

    #[test]
    fn ignores_claude_json_file_and_dirs_without_projects() {
        let tmp = TempHome::new();
        let home = tmp.path();
        // 파일(디렉토리 아님) — 자동 탐지에서 제외돼야 함.
        fs::write(home.join(".claude.json"), "{}").unwrap();
        // .claude*  디렉토리이지만 projects/ 없음 — 제외돼야 함.
        fs::create_dir_all(home.join(".claude-empty")).unwrap();
        // .claude로 시작 안 하는 디렉토리 — 제외돼야 함.
        fs::create_dir_all(home.join(".other").join("projects")).unwrap();

        let roots = detect_claude_roots(home);
        assert!(roots.is_empty());
    }

    #[test]
    fn resolve_roots_with_home_default_config_autodetects_and_includes_kiro() {
        let tmp = TempHome::new();
        let home = tmp.path();
        fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        fs::create_dir_all(home.join(".claude-a").join("projects")).unwrap();
        fs::create_dir_all(kiro_cli_dir(home)).unwrap();

        let roots = resolve_roots_with_home(home);
        let claude_roots: Vec<_> = roots.iter().filter(|(s, _)| *s == Source::Claude).collect();
        let kiro_roots: Vec<_> = roots.iter().filter(|(s, _)| *s == Source::KiroCli).collect();
        assert_eq!(claude_roots.len(), 2);
        assert_eq!(kiro_roots.len(), 1);
    }

    #[test]
    fn resolve_roots_with_home_missing_defaults_are_silently_skipped() {
        let tmp = TempHome::new();
        let home = tmp.path();
        // .claude/projects, .kiro/sessions/cli 둘 다 없음 — 빈 결과, 패닉 없음.
        let roots = resolve_roots_with_home(home);
        assert!(roots.is_empty());
    }

    #[test]
    fn resolve_roots_with_home_extra_roots_added_and_missing_ones_skipped() {
        let tmp = TempHome::new();
        let home = tmp.path();
        let extra = home.join("elsewhere").join("projects");
        fs::create_dir_all(&extra).unwrap();

        write_config(
            home,
            &format!(
                r#"{{ "capture": {{ "claudeCode": {{ "extraRoots": ["{}", "{}"], "useDefaults": false }} }} }}"#,
                extra.display(),
                home.join("does-not-exist").display(),
            ),
        );

        let roots = resolve_roots_with_home(home);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0], (Source::Claude, extra));
    }

    #[test]
    fn resolve_roots_with_home_dedupes_autodetected_and_extra_roots() {
        let tmp = TempHome::new();
        let home = tmp.path();
        let projects = home.join(".claude").join("projects");
        fs::create_dir_all(&projects).unwrap();

        // extraRoots에 자동 탐지된 것과 같은(하지만 canonicalize 전 표기가 다른) 경로를 중복으로 넣는다.
        write_config(
            home,
            &format!(
                r#"{{ "capture": {{ "claudeCode": {{ "extraRoots": ["{}"], "useDefaults": true }} }} }}"#,
                projects.display(),
            ),
        );

        let roots = resolve_roots_with_home(home);
        let claude_roots: Vec<_> = roots.iter().filter(|(s, _)| *s == Source::Claude).collect();
        assert_eq!(claude_roots.len(), 1);
    }

    #[test]
    fn use_defaults_false_disables_autodetection() {
        let tmp = TempHome::new();
        let home = tmp.path();
        fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        write_config(
            home,
            r#"{ "capture": { "claudeCode": { "useDefaults": false } } }"#,
        );

        let roots = resolve_roots_with_home(home);
        assert!(roots.iter().all(|(s, _)| *s != Source::Claude));
    }

    #[test]
    fn enabled_false_disables_source_entirely_default_and_custom() {
        let tmp = TempHome::new();
        let home = tmp.path();
        fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        let extra = home.join("elsewhere").join("projects");
        fs::create_dir_all(&extra).unwrap();

        // useDefaults는 true(기본)지만 enabled: false라 기본 root도, extraRoots도 모두 빠져야 함
        // (useDefaults false는 기본 root만 빼지만, enabled false는 소스 전체를 끈다).
        write_config(
            home,
            &format!(
                r#"{{ "capture": {{ "claudeCode": {{ "extraRoots": ["{}"], "enabled": false }} }} }}"#,
                extra.display(),
            ),
        );

        let roots = resolve_roots_with_home(home);
        assert!(roots.iter().all(|(s, _)| *s != Source::Claude));

        let detailed = resolve_roots_detailed_with_home(home);
        assert!(
            detailed.iter().all(|r| r.source != Source::Claude),
            "enabled=false 소스는 detailed 결과에도 포함되지 않아야 함"
        );
    }

    #[test]
    fn resolve_roots_detailed_marks_origin_and_exists() {
        let tmp = TempHome::new();
        let home = tmp.path();
        // 기본 root(존재함): .claude/projects.
        fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        // 커스텀 root(존재하지 않음).
        let missing_extra = home.join("elsewhere").join("projects");

        write_config(
            home,
            &format!(
                r#"{{ "capture": {{ "claudeCode": {{ "extraRoots": ["{}"], "useDefaults": true }} }} }}"#,
                missing_extra.display(),
            ),
        );

        let roots = resolve_roots_detailed_with_home(home);

        let default_root = roots
            .iter()
            .find(|r| r.origin == Origin::Default && r.source == Source::Claude)
            .expect("자동 탐지 root가 있어야 함");
        assert!(default_root.exists);

        let custom_root = roots
            .iter()
            .find(|r| r.origin == Origin::Custom && r.source == Source::Claude)
            .expect("커스텀 extraRoots가 있어야 함");
        assert!(!custom_root.exists);
        assert_eq!(custom_root.path, missing_extra);

        // kiro 기본 root도 부재 상태로 포함돼야 함(watch용 resolve_roots와 달리 UI 용은 감추지 않음).
        let kiro_default = roots
            .iter()
            .find(|r| r.source == Source::KiroCli && r.origin == Origin::Default)
            .expect("kiro 기본 root가 있어야 함");
        assert!(!kiro_default.exists);
    }

    // ── 직렬화 키/값 계약 (FE `ResolvedCaptureRoot`) ────────────

    #[test]
    fn resolved_root_serializes_with_fe_contract_keys_and_values() {
        let claude_root = ResolvedRoot {
            source: Source::Claude,
            path: PathBuf::from("/tmp/foo/projects"),
            origin: Origin::Custom,
            exists: true,
        };
        let value = serde_json::to_value(&claude_root).unwrap();
        assert_eq!(value["source"], "claude_code");
        assert_eq!(value["path"], "/tmp/foo/projects");
        assert_eq!(value["origin"], "custom");
        assert_eq!(value["exists"], true);

        let kiro_root = ResolvedRoot {
            source: Source::KiroCli,
            path: PathBuf::from("/tmp/bar/cli"),
            origin: Origin::Default,
            exists: false,
        };
        let kiro_value = serde_json::to_value(&kiro_root).unwrap();
        assert_eq!(kiro_value["source"], "kiro_cli");
        assert_eq!(kiro_value["origin"], "default");
        assert_eq!(kiro_value["exists"], false);
    }

    // ── checkUpdates(M5, ADR-0013) ───────────────────────────────

    #[test]
    fn load_check_updates_missing_file_defaults_to_true() {
        let tmp = TempHome::new();
        assert!(load_root_config_from_home(tmp.path()).check_updates);
    }

    #[test]
    fn load_check_updates_parses_false() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "checkUpdates": false }"#);
        assert!(!load_root_config_from_home(tmp.path()).check_updates);
    }

    #[test]
    fn save_check_updates_then_load_round_trips() {
        let tmp = TempHome::new();
        let home = tmp.path();
        save_check_updates_to_home(home, false).unwrap();
        assert!(!load_root_config_from_home(home).check_updates);

        let raw = fs::read_to_string(config_path(home)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["checkUpdates"], false);
    }

    #[test]
    fn save_check_updates_preserves_existing_capture_config() {
        let tmp = TempHome::new();
        let home = tmp.path();
        let cfg = CaptureConfig {
            body_policy: BodyPolicy::Full,
            ..CaptureConfig::default()
        };
        save_config_to_home(home, &cfg).unwrap();

        save_check_updates_to_home(home, false).unwrap();

        // checkUpdates 저장이 앞서 저장한 capture 설정(bodyPolicy)을 되돌리지 않아야 함
        // (save_config_to_home과 반대 방향의 유실 버그 방지 — #7과 동일한 클래스).
        assert_eq!(load_config_from_home(home).body_policy, BodyPolicy::Full);
        assert!(!load_root_config_from_home(home).check_updates);
    }

    #[test]
    fn save_config_preserves_existing_check_updates() {
        let tmp = TempHome::new();
        let home = tmp.path();
        save_check_updates_to_home(home, false).unwrap();

        let cfg = CaptureConfig {
            body_policy: BodyPolicy::Full,
            ..CaptureConfig::default()
        };
        save_config_to_home(home, &cfg).unwrap();

        // capture 설정 저장이 앞서 저장한 checkUpdates를 기본값(true)으로 되돌리지 않아야 함(#7 교훈).
        assert!(!load_root_config_from_home(home).check_updates);
        assert_eq!(load_config_from_home(home).body_policy, BodyPolicy::Full);
    }

    // ── locale(앱 i18n, 트레이 메뉴) ──────────────────────────────

    #[test]
    fn load_locale_missing_file_defaults_to_none() {
        let tmp = TempHome::new();
        assert_eq!(load_root_config_from_home(tmp.path()).locale, None);
    }

    #[test]
    fn load_locale_parses_ko() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "locale": "ko" }"#);
        assert_eq!(load_root_config_from_home(tmp.path()).locale.as_deref(), Some("ko"));
    }

    #[test]
    fn save_locale_then_load_round_trips() {
        let tmp = TempHome::new();
        let home = tmp.path();
        save_locale_to_home(home, Some("en".to_string())).unwrap();
        assert_eq!(load_root_config_from_home(home).locale.as_deref(), Some("en"));

        let raw = fs::read_to_string(config_path(home)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["locale"], "en");
    }

    #[test]
    fn save_locale_preserves_existing_capture_config_and_check_updates() {
        let tmp = TempHome::new();
        let home = tmp.path();
        let cfg = CaptureConfig {
            body_policy: BodyPolicy::Full,
            ..CaptureConfig::default()
        };
        save_config_to_home(home, &cfg).unwrap();
        save_check_updates_to_home(home, false).unwrap();

        save_locale_to_home(home, Some("ko".to_string())).unwrap();

        // locale 저장이 앞서 저장한 capture 설정/checkUpdates를 되돌리지 않아야 함(#7과 동일한 클래스).
        assert_eq!(load_config_from_home(home).body_policy, BodyPolicy::Full);
        assert!(!load_root_config_from_home(home).check_updates);
        assert_eq!(load_root_config_from_home(home).locale.as_deref(), Some("ko"));
    }

    #[test]
    fn save_config_preserves_existing_locale() {
        let tmp = TempHome::new();
        let home = tmp.path();
        save_locale_to_home(home, Some("ko".to_string())).unwrap();

        let cfg = CaptureConfig {
            body_policy: BodyPolicy::Full,
            ..CaptureConfig::default()
        };
        save_config_to_home(home, &cfg).unwrap();

        // capture 설정 저장이 앞서 저장한 locale을 지우지 않아야 함(#7 교훈과 동일한 원칙).
        assert_eq!(load_root_config_from_home(home).locale.as_deref(), Some("ko"));
        assert_eq!(load_config_from_home(home).body_policy, BodyPolicy::Full);
    }

    // ── 일일 AI 요약(M7-①, ADR-0016) ──────────────────────────────

    #[test]
    fn load_summary_config_missing_file_defaults_to_disabled_auto_engine() {
        let tmp = TempHome::new();
        let cfg = load_root_config_from_home(tmp.path()).summary;
        assert_eq!(cfg, SummaryConfig::default());
        assert!(!cfg.enabled);
        assert_eq!(cfg.engine, SummaryEngine::Auto);
        assert_eq!(cfg.cli_provider, CliProvider::Claude);
        assert_eq!(cfg.api_provider, ApiProvider::Anthropic);
        assert_eq!(cfg.anthropic_api_key, None);
        assert_eq!(cfg.openai_api_key, None);
        assert_eq!(cfg.gemini_api_key, None);
        assert_eq!(cfg.cli_model, "sonnet");
        assert_eq!(cfg.api_model, "claude-haiku-4-5");
    }

    #[test]
    fn load_summary_config_parses_fields() {
        let tmp = TempHome::new();
        write_config(
            tmp.path(),
            r#"{ "summary": {
                "enabled": true,
                "engine": "api",
                "cliProvider": "gemini",
                "apiProvider": "openai",
                "anthropicApiKey": "sk-ant-abc",
                "openaiApiKey": "sk-proj-abc",
                "geminiApiKey": "AIzaSyAbc",
                "cliModel": "opus",
                "apiModel": "claude-sonnet-4-5"
            } }"#,
        );
        let cfg = load_root_config_from_home(tmp.path()).summary;
        assert!(cfg.enabled);
        assert_eq!(cfg.engine, SummaryEngine::Api);
        assert_eq!(cfg.cli_provider, CliProvider::Gemini);
        assert_eq!(cfg.api_provider, ApiProvider::Openai);
        assert_eq!(cfg.anthropic_api_key.as_deref(), Some("sk-ant-abc"));
        assert_eq!(cfg.openai_api_key.as_deref(), Some("sk-proj-abc"));
        assert_eq!(cfg.gemini_api_key.as_deref(), Some("AIzaSyAbc"));
        assert_eq!(cfg.cli_model, "opus");
        assert_eq!(cfg.api_model, "claude-sonnet-4-5");
    }

    /// 구버전 config.json(단일 프로바이더 시절, `apiKey` 필드) 호환성 — 마이그레이션 스크립트 없이도
    /// `anthropicApiKey`의 serde alias로 무손실 로드돼야 한다.
    #[test]
    fn load_summary_config_legacy_api_key_field_maps_to_anthropic_api_key() {
        let tmp = TempHome::new();
        write_config(
            tmp.path(),
            r#"{ "summary": { "engine": "api", "apiKey": "sk-ant-x" } }"#,
        );
        let cfg = load_root_config_from_home(tmp.path()).summary;
        assert_eq!(cfg.engine, SummaryEngine::Api);
        assert_eq!(cfg.anthropic_api_key.as_deref(), Some("sk-ant-x"));
        // 구버전에는 없던 필드라 기본값 유지돼야 함.
        assert_eq!(cfg.api_provider, ApiProvider::Anthropic);
        assert_eq!(cfg.openai_api_key, None);
        assert_eq!(cfg.gemini_api_key, None);
    }

    #[test]
    fn load_summary_config_partial_fields_use_defaults_for_missing() {
        let tmp = TempHome::new();
        write_config(tmp.path(), r#"{ "summary": { "enabled": true } }"#);
        let cfg = load_root_config_from_home(tmp.path()).summary;
        assert!(cfg.enabled);
        assert_eq!(cfg.engine, SummaryEngine::Auto, "engine 누락 시 기본값 auto 유지돼야 함");
        assert_eq!(cfg.cli_provider, CliProvider::Claude, "cliProvider 누락 시 기본값 claude 유지돼야 함");
        assert_eq!(cfg.api_provider, ApiProvider::Anthropic, "apiProvider 누락 시 기본값 anthropic 유지돼야 함");
        assert_eq!(cfg.cli_model, "sonnet", "cliModel 누락 시 기본값 유지돼야 함");
        assert_eq!(cfg.api_model, "claude-haiku-4-5", "apiModel 누락 시 기본값 유지돼야 함");
    }

    #[test]
    fn save_summary_config_then_load_round_trips() {
        let tmp = TempHome::new();
        let home = tmp.path();
        let cfg = SummaryConfig {
            enabled: true,
            engine: SummaryEngine::Cli,
            cli_provider: CliProvider::Codex,
            api_provider: ApiProvider::Gemini,
            anthropic_api_key: Some("sk-ant-test-key".to_string()),
            openai_api_key: Some("sk-proj-test-key".to_string()),
            gemini_api_key: Some("AIzaSyTestKey".to_string()),
            cli_model: "opus".to_string(),
            api_model: "claude-sonnet-4-5".to_string(),
            cli_config_dir: Some("~/.claude-b".to_string()),
            auto_generate: true,
        };

        save_summary_config_to_home(home, &cfg).unwrap();
        assert_eq!(load_root_config_from_home(home).summary, cfg);

        let raw = fs::read_to_string(config_path(home)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["summary"]["enabled"], true);
        assert_eq!(value["summary"]["engine"], "cli");
        assert_eq!(value["summary"]["cliProvider"], "codex");
        assert_eq!(value["summary"]["apiProvider"], "gemini");
        assert_eq!(value["summary"]["anthropicApiKey"], "sk-ant-test-key");
        assert_eq!(value["summary"]["openaiApiKey"], "sk-proj-test-key");
        assert_eq!(value["summary"]["geminiApiKey"], "AIzaSyTestKey");
        assert_eq!(value["summary"]["cliModel"], "opus");
        assert_eq!(value["summary"]["apiModel"], "claude-sonnet-4-5");
    }

    #[test]
    fn save_summary_config_preserves_existing_capture_config_and_locale() {
        let tmp = TempHome::new();
        let home = tmp.path();
        let capture_cfg = CaptureConfig {
            body_policy: BodyPolicy::Full,
            ..CaptureConfig::default()
        };
        save_config_to_home(home, &capture_cfg).unwrap();
        save_locale_to_home(home, Some("ko".to_string())).unwrap();

        save_summary_config_to_home(home, &SummaryConfig { enabled: true, ..SummaryConfig::default() })
            .unwrap();

        // summary 저장이 앞서 저장한 capture 설정/locale을 되돌리지 않아야 함(#7과 동일한 클래스).
        assert_eq!(load_config_from_home(home).body_policy, BodyPolicy::Full);
        assert_eq!(load_root_config_from_home(home).locale.as_deref(), Some("ko"));
        assert!(load_root_config_from_home(home).summary.enabled);
    }

    #[test]
    fn save_config_preserves_existing_summary_config() {
        let tmp = TempHome::new();
        let home = tmp.path();
        save_summary_config_to_home(home, &SummaryConfig { enabled: true, ..SummaryConfig::default() })
            .unwrap();

        let capture_cfg = CaptureConfig {
            body_policy: BodyPolicy::Full,
            ..CaptureConfig::default()
        };
        save_config_to_home(home, &capture_cfg).unwrap();

        // capture 설정 저장이 앞서 저장한 summary 설정을 기본값으로 되돌리지 않아야 함(#7 교훈).
        assert!(load_root_config_from_home(home).summary.enabled);
        assert_eq!(load_config_from_home(home).body_policy, BodyPolicy::Full);
    }

    // ── 평가 프로필(ADR-0017) ───────────────────────────────────────

    #[test]
    fn review_profile_normalized_trims_role_and_drops_unknown_level() {
        let long_role = "가".repeat(REVIEW_ROLE_MAX_CHARS + 10);
        let profile = ReviewProfile {
            role: format!("  {long_role}  "),
            level: "principal".to_string(),
            manages_people: true,
        };
        let normalized = profile.normalized();
        assert_eq!(normalized.role.chars().count(), REVIEW_ROLE_MAX_CHARS);
        assert_eq!(normalized.level, "", "모르는 연차 값은 미지정으로 떨어져야 함");
        assert!(normalized.manages_people);

        let known = ReviewProfile {
            role: " 프론트엔드 개발자 ".to_string(),
            level: "senior".to_string(),
            manages_people: false,
        }
        .normalized();
        assert_eq!(known.role, "프론트엔드 개발자");
        assert_eq!(known.level, "senior");
    }

    #[test]
    fn review_profile_is_empty_only_when_all_fields_blank() {
        assert!(ReviewProfile::default().is_empty());
        assert!(!ReviewProfile { manages_people: true, ..ReviewProfile::default() }.is_empty());
        assert!(!ReviewProfile { level: "mid".to_string(), ..ReviewProfile::default() }.is_empty());
    }

    #[test]
    fn save_review_profile_preserves_other_sections_and_round_trips() {
        let tmp = TempHome::new();
        let home = tmp.path();
        save_summary_config_to_home(home, &SummaryConfig { enabled: true, ..SummaryConfig::default() })
            .unwrap();
        save_config_to_home(
            home,
            &CaptureConfig {
                body_policy: BodyPolicy::Full,
                ..CaptureConfig::default()
            },
        )
        .unwrap();

        let profile = ReviewProfile {
            role: "프론트엔드 개발자".to_string(),
            level: "senior".to_string(),
            manages_people: true,
        };
        save_review_profile_to_home(home, &profile).unwrap();

        let root = load_root_config_from_home(home);
        assert_eq!(root.review_profile, profile);
        assert!(root.summary.enabled, "프로필 저장이 요약 설정을 되돌리면 안 됨");
        assert_eq!(root.capture.body_policy, BodyPolicy::Full, "프로필 저장이 캡처 설정을 되돌리면 안 됨");

        // 반대 방향 — 요약 설정 저장이 프로필을 지우면 안 된다(요약 설정은 FE가 통째로 왕복 저장).
        save_summary_config_to_home(home, &SummaryConfig::default()).unwrap();
        assert_eq!(load_root_config_from_home(home).review_profile, profile);
    }

    #[test]
    fn unknown_review_level_in_file_does_not_reset_whole_config() {
        let tmp = TempHome::new();
        let home = tmp.path();
        fs::create_dir_all(home.join(".logroom")).unwrap();
        fs::write(
            config_path(home),
            r#"{"summary":{"enabled":true},"reviewProfile":{"role":"PM","level":"principal"}}"#,
        )
        .unwrap();

        let root = load_root_config_from_home(home);
        assert!(root.summary.enabled, "모르는 연차 값 하나로 설정 전체가 기본값이 되면 안 됨");
        assert_eq!(root.review_profile.normalized().level, "");
        assert_eq!(root.review_profile.role, "PM");
    }

    // ── 요약 API 키 마스킹(FE 왕복 보호, 보안 리뷰) ───────────────────

    #[test]
    fn mask_summary_api_key_keeps_last_four_chars() {
        assert_eq!(mask_summary_api_key("sk-ant-1234567890abcdef"), "••••cdef");
    }

    #[test]
    fn resolve_summary_api_key_update_masked_value_preserves_previous_key() {
        let masked = mask_summary_api_key("sk-ant-original-key");
        let resolved = resolve_summary_api_key_update(Some("sk-ant-original-key"), Some(masked));
        assert_eq!(resolved.as_deref(), Some("sk-ant-original-key"));
    }

    #[test]
    fn resolve_summary_api_key_update_new_value_replaces_and_trims() {
        let resolved = resolve_summary_api_key_update(
            Some("sk-ant-original-key"),
            Some("  sk-ant-new-key  ".to_string()),
        );
        assert_eq!(resolved.as_deref(), Some("sk-ant-new-key"));
    }

    #[test]
    fn resolve_summary_api_key_update_none_clears_key() {
        let resolved = resolve_summary_api_key_update(Some("sk-ant-original-key"), None);
        assert_eq!(resolved, None);
    }

    /// 마스킹/resolve 헬퍼는 값 자체만 다루는 범용 함수라 anthropic/openai/gemini 3개 키 모양
    /// (`sk-ant-...` / `sk-proj-...` / `AIzaSy...`) 어디에도 그대로 재사용된다.
    #[test]
    fn mask_summary_api_key_applies_to_any_provider_key_shape() {
        assert_eq!(mask_summary_api_key("sk-ant-1234567890abcdef"), "••••cdef");
        assert_eq!(mask_summary_api_key("sk-proj-1234567890abcdef"), "••••cdef");
        assert_eq!(mask_summary_api_key("AIzaSyABCDEFGHIJKLMNOP1234"), "••••1234");
    }

    /// 3개 키가 서로 다른 previous 값을 가진 상태에서 각자의 마스킹 값을 되돌려보내도 서로 뒤섞이지
    /// 않고 자신의 원본만 보존돼야 한다(`get_summary_config`/`set_summary_config`가 필드별로 독립
    /// 호출하는 것과 동일한 시나리오).
    #[test]
    fn resolve_summary_api_key_update_round_trips_independently_per_provider() {
        let anthropic_prev = "sk-ant-original";
        let openai_prev = "sk-proj-original";
        let gemini_prev = "AIzaSyOriginal";

        let anthropic_masked = mask_summary_api_key(anthropic_prev);
        let openai_masked = mask_summary_api_key(openai_prev);
        let gemini_masked = mask_summary_api_key(gemini_prev);

        assert_eq!(
            resolve_summary_api_key_update(Some(anthropic_prev), Some(anthropic_masked)).as_deref(),
            Some(anthropic_prev)
        );
        assert_eq!(
            resolve_summary_api_key_update(Some(openai_prev), Some(openai_masked)).as_deref(),
            Some(openai_prev)
        );
        assert_eq!(
            resolve_summary_api_key_update(Some(gemini_prev), Some(gemini_masked)).as_deref(),
            Some(gemini_prev)
        );
    }

    // ── CLI/API 프로바이더 enum(직렬화) ──────────────────────────────

    #[test]
    fn cli_provider_serializes_lowercase_and_defaults_to_claude() {
        assert_eq!(CliProvider::default(), CliProvider::Claude);
        assert_eq!(serde_json::to_value(CliProvider::Gemini).unwrap(), serde_json::json!("gemini"));
        assert_eq!(
            serde_json::from_value::<CliProvider>(serde_json::json!("codex")).unwrap(),
            CliProvider::Codex
        );
    }

    #[test]
    fn api_provider_serializes_lowercase_and_defaults_to_anthropic() {
        assert_eq!(ApiProvider::default(), ApiProvider::Anthropic);
        assert_eq!(serde_json::to_value(ApiProvider::Openai).unwrap(), serde_json::json!("openai"));
        assert_eq!(
            serde_json::from_value::<ApiProvider>(serde_json::json!("gemini")).unwrap(),
            ApiProvider::Gemini
        );
    }

    // ── 파일 권한 강제(config.json/디렉토리, 보안 리뷰) ──────────────

    #[test]
    fn save_config_enforces_directory_and_file_permissions() {
        let tmp = TempHome::new();
        let home = tmp.path();
        save_config_to_home(home, &CaptureConfig::default()).unwrap();

        let dir_mode = fs::metadata(home.join(".logroom")).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "~/.logroom 디렉토리는 0700이어야 함");

        let file_mode = fs::metadata(config_path(home)).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "config.json 파일은 0600이어야 함");
    }

    #[test]
    fn load_config_migrates_existing_file_permissions_to_0600() {
        let tmp = TempHome::new();
        let home = tmp.path();
        write_config(home, "{}");
        // 과거 버전이 기본 생성 권한(0644)으로 만들었다고 가정 — 마이그레이션 대상 시나리오.
        fs::set_permissions(config_path(home), fs::Permissions::from_mode(0o644)).unwrap();

        load_root_config_from_home(home);

        let file_mode = fs::metadata(config_path(home)).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "load 시 기존 config.json 권한이 0600으로 교정돼야 함");
    }

    // ── slack 토큰 마스킹(FE 왕복 보호, 보안 리뷰) ────────────────────

    #[test]
    fn mask_slack_token_keeps_last_four_chars() {
        assert_eq!(mask_slack_token("xoxp-1234567890-abcdef"), "••••cdef");
    }

    #[test]
    fn mask_slack_token_shorter_than_four_chars_shows_whole_value() {
        assert_eq!(mask_slack_token("ab"), "••••ab");
    }

    #[test]
    fn is_masked_slack_token_detects_own_output() {
        let masked = mask_slack_token("xoxp-abcdefgh");
        assert!(is_masked_slack_token(&masked));
        assert!(!is_masked_slack_token("xoxp-abcdefgh"));
    }

    #[test]
    fn resolve_slack_token_update_masked_value_preserves_previous_token() {
        // FE가 변경 없이 마스킹 값을 그대로 되돌려보낸 경우 — 기존 토큰이 유지돼야 함.
        let masked = mask_slack_token("xoxp-original-token");
        let resolved = resolve_slack_token_update(Some("xoxp-original-token"), Some(masked));
        assert_eq!(resolved.as_deref(), Some("xoxp-original-token"));
    }

    #[test]
    fn resolve_slack_token_update_new_value_replaces_and_trims() {
        // 새 토큰을 입력한 경우 — 앞뒤 공백은 trim되어 저장돼야 함(FE trim과 이중 방어).
        let resolved = resolve_slack_token_update(
            Some("xoxp-original-token"),
            Some("  xoxp-new-token  ".to_string()),
        );
        assert_eq!(resolved.as_deref(), Some("xoxp-new-token"));
    }

    #[test]
    fn resolve_slack_token_update_none_clears_token() {
        let resolved = resolve_slack_token_update(Some("xoxp-original-token"), None);
        assert_eq!(resolved, None);
    }

    #[test]
    fn resolve_slack_token_update_blank_new_value_clears_token() {
        // trim 후 빈 문자열이면 마스킹 값이 아니더라도 토큰 삭제로 취급.
        let resolved = resolve_slack_token_update(Some("xoxp-original-token"), Some("   ".to_string()));
        assert_eq!(resolved, None);
    }
}
