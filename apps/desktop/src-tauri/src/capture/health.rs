//! 소스별 캡처 헬스(감시 상태) 판정 (docs/03-capture.md "캡처 헬스").
//! `get_capture_health` 커맨드(lib.rs)가 이 모듈의 [`compute_health`]를 그대로 사용한다.
//!
//! 판정은 단순 경과시간이 아니라 "감시 root의 최신 `.jsonl` mtime"과
//! "`capture_cursors`의 마지막 갱신 시각"을 비교한다 — 도구를 오래 안 써서 파일 mtime도
//! 오래됐다면 그건 정상(ok)이고, 파일은 계속 갱신되는데 cursor가 안 따라가면(감시 실패 의심)
//! stale로 잡아야 하기 때문이다.

use crate::capture::config::{self, Source};
use crate::capture::github;
use crate::capture::linear;
use crate::capture::notion;
use crate::capture::slack;
use crate::capture::watch::walk_jsonl_files;
use crate::db;
use rusqlite::Connection;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `latestFileMtime`이 `lastCapturedAt`보다 이 값(ms) 이상 앞서면 stale로 간주(2분 여유).
const STALE_THRESHOLD_MS: i64 = 120_000;

/// Slack(폴러형 소스) stale 판정 배수 — `pollMinutes`의 이 배수만큼 폴 성공이 없으면 폴러가
/// 죽었거나 네트워크/인증 오류가 지속되는 상태로 간주한다(docs/08-connectors.md "헬스 판정").
const SLACK_STALE_POLL_MULTIPLIER: i64 = 3;
/// Slack stale 판정 최소 임계값(ms) — `pollMinutes`를 아주 작게 설정해도 순간적 지연을 stale로
/// 오탐하지 않도록 하는 하한.
const SLACK_STALE_MIN_MS: i64 = 10 * 60 * 1000;

/// FE 계약(`get_capture_health` 응답 `sources[].status`) 값과 1:1 대응.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Stale,
    Inactive,
}

/// `get_capture_health` 응답 `sources[].source` 값. 파일감시(Claude/Kiro, [`config::Source`])와
/// 폴러(Slack, root 개념이 없어 `config::Source`에는 없음)를 하나의 목록으로 묶어 반환하기 위한
/// 통합 표현 — 헬스 API 전용이며 `config::Source`(root 해석용)와는 별개다(docs/08-connectors.md,
/// ADR-0015 "결과").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthSource {
    ClaudeCode,
    KiroCli,
    Slack,
    Github,
    Linear,
    Notion,
}

impl From<Source> for HealthSource {
    fn from(source: Source) -> Self {
        match source {
            Source::Claude => HealthSource::ClaudeCode,
            Source::KiroCli => HealthSource::KiroCli,
        }
    }
}

/// 소스 1개의 캡처 헬스. FE 계약(camelCase)과 1:1 대응. `roots`는 파일감시 소스(Claude/Kiro)에서는
/// 감시 중인 root 개수, Slack(폴러형)에서는 "설정됨"(1)/"미설정"(0)을 의미한다(root 개념이 없어
/// 같은 필드를 재사용 — FE는 소스에 따라 표시 문구를 다르게 쓴다).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceHealth {
    pub source: HealthSource,
    pub roots: usize,
    pub last_captured_at: Option<i64>,
    pub latest_file_mtime: Option<i64>,
    pub status: Status,
}

/// 판정 순수함수: `roots == 0` → inactive. 파일은 있는데 cursor가 아예 없거나,
/// 파일 mtime이 마지막 캡처 시각보다 2분 넘게 앞서 있으면 → stale(감시 실패 의심). 그 외 → ok.
fn classify(roots: usize, last_captured_at: Option<i64>, latest_file_mtime: Option<i64>) -> Status {
    if roots == 0 {
        return Status::Inactive;
    }
    match (latest_file_mtime, last_captured_at) {
        (Some(_), None) => Status::Stale,
        (Some(latest), Some(last)) if latest > last + STALE_THRESHOLD_MS => Status::Stale,
        _ => Status::Ok,
    }
}

fn file_mtime_ms(path: &Path) -> Option<i64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// 감시 root들(존재하는 것만) 하위 `.jsonl` 파일 중 가장 최근 mtime(ms). 파일이 하나도 없으면 `None`.
fn latest_jsonl_mtime(roots: &[PathBuf]) -> Option<i64> {
    roots
        .iter()
        .flat_map(|root| walk_jsonl_files(root))
        .filter_map(|path| file_mtime_ms(&path))
        .max()
}

/// Slack(폴러형) 판정 순수함수: 미설정(`!configured`) → inactive. 설정됐지만 한 번도 폴 성공이
/// 없거나(`last_captured_at is None`), 마지막 성공이 `pollMinutes`의 3배(최소 10분) 이상 지났으면
/// → stale(폴러 실패 의심). 그 외 → ok(docs/08-connectors.md "헬스 판정").
fn classify_slack(configured: bool, last_captured_at: Option<i64>, poll_minutes: u32, now_ms: i64) -> Status {
    if !configured {
        return Status::Inactive;
    }
    let Some(last) = last_captured_at else {
        return Status::Stale;
    };
    let threshold = (i64::from(poll_minutes.max(1)) * 60_000 * SLACK_STALE_POLL_MULTIPLIER).max(SLACK_STALE_MIN_MS);
    if now_ms - last > threshold {
        Status::Stale
    } else {
        Status::Ok
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Slack 폴러 헬스(docs/08-connectors.md, ADR-0015): 파일 root 개념이 없어 claude/kiro와 다른
/// 판정 기준([`classify_slack`])을 쓴다 — `capture_cursors`(source="slack")의 `updated_at`을
/// "마지막 성공 폴링 시각"으로 재사용한다(`capture/slack.rs::poll_once`가 메시지 유무와 무관하게
/// 폴 성공 시 매번 갱신).
fn compute_slack_health(conn: &Connection) -> anyhow::Result<SourceHealth> {
    let slack_cfg = config::load_slack_config();
    let configured = slack_cfg.enabled && slack_cfg.token.is_some();
    let last_captured_at = db::max_cursor_updated_at(conn, slack::SOURCE)?;
    let status = classify_slack(configured, last_captured_at, slack_cfg.poll_minutes, now_ms());
    Ok(SourceHealth {
        source: HealthSource::Slack,
        roots: usize::from(configured),
        last_captured_at,
        latest_file_mtime: None,
        status,
    })
}

/// GitHub 커넥터(v1, docs/08-connectors.md) 헬스: Slack과 같은 폴러형 판정([`classify_slack`] 재사용
/// — rate limit/폴링 주기 특성이 같은 부류라 별도 계산식을 두지 않는다)을 쓰되, **다중 계정** 특성을
/// 반영해 `roots`는 설정된 계정 수(0..N)를 담는다(Slack은 0/1 — "설정됨" 여부만 표시). `last_captured_at`
/// 은 계정별 커서(`events:<user>:newest` 등, `capture/github.rs`)의 `updated_at` 중 최댓값이라, 계정
/// 일부만 토큰이 무효화돼도 다른 계정이 살아있으면 전체 상태는 stale로 잡히지 않는다(그 계정 개별
/// 실패는 로그로만 확인 가능 — `capture/github.rs::spawn` 참고, 알려진 한계).
fn compute_github_health(conn: &Connection) -> anyhow::Result<SourceHealth> {
    let github_cfg = config::load_github_config();
    let configured = github_cfg.enabled && !github_cfg.accounts.is_empty();
    let last_captured_at = db::max_cursor_updated_at(conn, github::SOURCE)?;
    let status = classify_slack(configured, last_captured_at, github_cfg.poll_minutes, now_ms());
    Ok(SourceHealth {
        source: HealthSource::Github,
        roots: github_cfg.accounts.len(),
        last_captured_at,
        latest_file_mtime: None,
        status,
    })
}

/// Linear 커넥터(v1, docs/08-connectors.md) 헬스: GitHub와 동일한 폴러형+다중 계정(워크스페이스)
/// 판정([`classify_slack`] 재사용 — rate limit/폴링 주기 특성이 같은 부류)을 쓴다. `roots`는 설정된
/// 계정(워크스페이스) 수(0..N)를 담는다. `last_captured_at`은 계정별 커서
/// (`issues:<viewerId>:newest` 등, `capture/linear.rs`)의 `updated_at` 중 최댓값이라, 계정 일부만
/// API 키가 무효화돼도 다른 계정이 살아있으면 전체 상태는 stale로 잡히지 않는다(GitHub와 동일한
/// 알려진 한계 — 개별 계정 실패는 로그로만 확인 가능).
fn compute_linear_health(conn: &Connection) -> anyhow::Result<SourceHealth> {
    let linear_cfg = config::load_linear_config();
    let configured = linear_cfg.enabled && !linear_cfg.accounts.is_empty();
    let last_captured_at = db::max_cursor_updated_at(conn, linear::SOURCE)?;
    let status = classify_slack(configured, last_captured_at, linear_cfg.poll_minutes, now_ms());
    Ok(SourceHealth {
        source: HealthSource::Linear,
        roots: linear_cfg.accounts.len(),
        last_captured_at,
        latest_file_mtime: None,
        status,
    })
}

/// Notion 커넥터(v1, docs/08-connectors.md "Notion v1" §헬스 판정) 헬스: GitHub/Linear와 동일한
/// 폴러형+다중 계정(워크스페이스) 판정([`classify_slack`] 재사용 — rate limit/폴링 주기 특성이 같은
/// 부류)을 쓴다. `roots`는 설정된 계정(워크스페이스) 수(0..N)를 담는다. `last_captured_at`은
/// 계정별 커서(`search:<workspace key>`, `capture/notion.rs`)의 `updated_at` 중 최댓값이라, 계정
/// 일부만 토큰이 무효화돼도 다른 계정이 살아있으면 전체 상태는 stale로 잡히지 않는다(GitHub/Linear와
/// 동일한 알려진 한계). **주의**: 헬스가 `ok`라도 integration이 연결된 페이지가 0개면 캡처는
/// 0건이다(문서 "헬스 판정" — 폴링 자체는 성공하므로 이 헬스만으로는 구분되지 않는다).
fn compute_notion_health(conn: &Connection) -> anyhow::Result<SourceHealth> {
    let notion_cfg = config::load_notion_config();
    let configured = notion_cfg.enabled && !notion_cfg.accounts.is_empty();
    let last_captured_at = db::max_cursor_updated_at(conn, notion::SOURCE)?;
    let status = classify_slack(
        configured,
        last_captured_at,
        notion_cfg.poll_minutes,
        now_ms(),
    );
    Ok(SourceHealth {
        source: HealthSource::Notion,
        roots: notion_cfg.accounts.len(),
        last_captured_at,
        latest_file_mtime: None,
        status,
    })
}

/// `get_capture_health` 커맨드가 사용하는 전체 계산: Claude Code/Kiro CLI/Slack/GitHub/Linear/Notion
/// 6건을 항상 반환한다(root가 0개거나 미설정이어도 inactive로 포함).
pub fn compute_health(conn: &Connection) -> anyhow::Result<Vec<SourceHealth>> {
    let resolved = config::resolve_roots();

    let mut out = Vec::with_capacity(6);
    for source in [Source::Claude, Source::KiroCli] {
        let roots: Vec<PathBuf> = resolved
            .iter()
            .filter(|(s, _)| *s == source)
            .map(|(_, path)| path.clone())
            .collect();
        let last_captured_at = db::max_cursor_updated_at(conn, source.cursor_source())?;
        let latest_file_mtime = latest_jsonl_mtime(&roots);
        let status = classify(roots.len(), last_captured_at, latest_file_mtime);
        out.push(SourceHealth {
            source: source.into(),
            roots: roots.len(),
            last_captured_at,
            latest_file_mtime,
            status,
        });
    }
    out.push(compute_slack_health(conn)?);
    out.push(compute_github_health(conn)?);
    out.push(compute_linear_health(conn)?);
    out.push(compute_notion_health(conn)?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_inactive_when_no_roots() {
        assert_eq!(classify(0, Some(1_000), Some(1_000)), Status::Inactive);
        // root가 없으면 cursor/mtime 값과 무관하게 inactive.
        assert_eq!(classify(0, None, None), Status::Inactive);
    }

    #[test]
    fn classify_ok_within_stale_threshold_boundary() {
        // latest == last + threshold(경계값) → stale 아님(ok).
        assert_eq!(classify(1, Some(1_000), Some(1_000 + STALE_THRESHOLD_MS)), Status::Ok);
    }

    #[test]
    fn classify_stale_when_latest_file_beyond_threshold_ahead_of_cursor() {
        assert_eq!(
            classify(1, Some(1_000), Some(1_000 + STALE_THRESHOLD_MS + 1)),
            Status::Stale
        );
    }

    #[test]
    fn classify_stale_when_file_exists_but_never_captured() {
        assert_eq!(classify(1, None, Some(1_000)), Status::Stale);
    }

    #[test]
    fn classify_ok_when_roots_exist_but_no_files_and_no_cursor_yet() {
        // 도구 미사용(파일도, cursor도 없음)은 감시 실패가 아니라 정상.
        assert_eq!(classify(1, None, None), Status::Ok);
    }

    // ── classify_slack(Slack 폴러 헬스, docs/08-connectors.md) ────────

    #[test]
    fn classify_slack_inactive_when_not_configured() {
        assert_eq!(classify_slack(false, Some(1_000), 5, 2_000), Status::Inactive);
        // 미설정이면 last_captured_at 값과 무관하게 inactive.
        assert_eq!(classify_slack(false, None, 5, 2_000), Status::Inactive);
    }

    #[test]
    fn classify_slack_stale_when_configured_but_never_succeeded() {
        assert_eq!(classify_slack(true, None, 5, 2_000), Status::Stale);
    }

    #[test]
    fn classify_slack_ok_within_threshold() {
        let poll_minutes = 5;
        let threshold = i64::from(poll_minutes) * 60_000 * SLACK_STALE_POLL_MULTIPLIER;
        assert_eq!(classify_slack(true, Some(1_000), poll_minutes, 1_000 + threshold), Status::Ok);
    }

    #[test]
    fn classify_slack_stale_beyond_threshold() {
        let poll_minutes = 5;
        let threshold = i64::from(poll_minutes) * 60_000 * SLACK_STALE_POLL_MULTIPLIER;
        assert_eq!(
            classify_slack(true, Some(1_000), poll_minutes, 1_000 + threshold + 1),
            Status::Stale
        );
    }

    #[test]
    fn classify_slack_uses_minimum_threshold_for_tiny_poll_minutes() {
        // pollMinutes=1이면 3배=3분이지만 최소 임계값(10분)이 적용돼야 함.
        let just_under_min = SLACK_STALE_MIN_MS - 1;
        assert_eq!(classify_slack(true, Some(0), 1, just_under_min), Status::Ok);
        assert_eq!(classify_slack(true, Some(0), 1, SLACK_STALE_MIN_MS + 1), Status::Stale);
    }

    // ── HealthSource 직렬화 계약(FE `HealthSource`) ─────────────────

    #[test]
    fn health_source_serializes_with_fe_contract_values() {
        assert_eq!(serde_json::to_value(HealthSource::ClaudeCode).unwrap(), "claude_code");
        assert_eq!(serde_json::to_value(HealthSource::KiroCli).unwrap(), "kiro_cli");
        assert_eq!(serde_json::to_value(HealthSource::Slack).unwrap(), "slack");
        assert_eq!(serde_json::to_value(HealthSource::Github).unwrap(), "github");
        assert_eq!(serde_json::to_value(HealthSource::Linear).unwrap(), "linear");
        assert_eq!(
            serde_json::to_value(HealthSource::Notion).unwrap(),
            "notion"
        );
    }

    #[test]
    fn health_source_from_config_source_maps_correctly() {
        assert_eq!(HealthSource::from(Source::Claude), HealthSource::ClaudeCode);
        assert_eq!(HealthSource::from(Source::KiroCli), HealthSource::KiroCli);
    }
}
