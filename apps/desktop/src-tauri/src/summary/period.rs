//! 주간/월간 AI 요약 롤업(M7-③) — 원본 이벤트를 재발췌하지 않고 하위 계층 요약을 합성한다.
//! 주간 발췌 = 그 주(월~일) `daily_summaries` 7개 합성, 월간 발췌 = 그 달에 속한
//! `period_summaries('week')` 합성(설계 원칙 "계층 롤업"). 숫자(활동일·세션·PR 등)는
//! [`get_period_stats`]가 SQL로 직접 집계해 LLM 프롬프트와 분리한다(설계 원칙 "숫자는 DB, 서사는
//! LLM" — 합성 발췌는 서사만 담당).
//!
//! v7(통계 헤더): 위 원칙은 FE 팩트 스트립(`PeriodStatsStrip`, SummaryView.tsx)에는 여전히
//! 유효하지만, "평면 나열이라 스캔이 안 된다"는 실사용 피드백으로 LLM에 넘기는 발췌 자체에도
//! 통계 한 줄(활동일·세션·요청·GitHub·Linear)을 미리 박아 넣는다 — [`get_period_stats`]가 쓰는
//! 집계 함수를 그대로 재사용하고(새 SQL 없음), LLM은 그 수치를 직접 세지 않고 총평 첫머리에
//! 그대로 옮겨 적기만 한다(`prompts.rs` 참고).
//!
//! `period_summaries` 캐시(V9, `daily_summaries`와 동일한 upsert 원칙)의 read/write도 이 모듈이
//! 담당한다. `lib.rs`의 `get_period_summary`/`generate_period_summary`/`preview_period_summary_input`/
//! `get_period_stats` 커맨드가 이 모듈을 사용한다.

use super::excerpt::{self, MAX_EXCERPT_CHARS, NO_DATA_ERROR_CODE};
use super::{engine, prompts};
use crate::capture::config::SummaryConfig;
use crate::capture::policy::truncate_with_ellipsis;
use crate::capture::scrub;
use crate::query::day_range_ms;
use chrono::{Datelike, Duration, NaiveDate, TimeZone};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("시스템 시간")
        .as_millis() as i64
}

// ── 기간 계산(순수함수) ───────────────────────────────────────────

fn parse_local_date(s: &str) -> anyhow::Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| anyhow::anyhow!("invalid date: {s}"))
}

fn to_date_string(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// `date`가 속한 주의 월요일(주 시작 고정 — ADR-0016과 동일하게 로컬 날짜 기준으로만 계산한다).
fn monday_of_week(date: NaiveDate) -> NaiveDate {
    let days_from_monday = i64::from(date.weekday().num_days_from_monday());
    date - Duration::days(days_from_monday)
}

/// `first_day`가 속한 달의 마지막 날(다음 달 1일 - 1일로 계산 — 윤년 2월도 자동 처리).
fn last_day_of_month(first_day: NaiveDate) -> NaiveDate {
    let (year, month) = (first_day.year(), first_day.month());
    let next_month_first = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .expect("valid next-month first day");
    next_month_first - Duration::days(1)
}

/// 주간 기간 계산: `period_key`(그 주 월요일 "YYYY-MM-DD")로부터 `[start_ms, end_ms)`(로컬 자정
/// 기준, `query::day_range_ms`와 동일 규칙) + 월요일~일요일 7개 로컬 날짜 문자열을 반환한다.
/// `period_key`는 호출부(FE)가 이미 "그 주의 월요일"로 계산해 넘긴다는 계약이며, 여기서는 그
/// 값을 그대로 시작점으로 삼는다(릴리스 빌드에선 요일 검증 없음 — FE 계산이 틀렸다면 결과 날짜
/// 목록이 관례와 다른 요일에서 시작할 뿐, 크래시하지는 않는다. 개발 빌드에선 `debug_assert`로
/// 계약 위반을 조기에 잡는다 — 리뷰 Nit).
pub fn week_range(period_key: &str, tz: &str) -> anyhow::Result<(i64, i64, Vec<String>)> {
    let monday = parse_local_date(period_key)?;
    debug_assert!(
        monday.weekday() == chrono::Weekday::Mon,
        "week periodKey는 월요일이어야 함: {period_key}"
    );
    let dates: Vec<String> = (0..7).map(|i| to_date_string(monday + Duration::days(i))).collect();
    let start_ms = day_range_ms(&dates[0], tz)?.0;
    let end_ms = day_range_ms(&dates[6], tz)?.1;
    Ok((start_ms, end_ms, dates))
}

/// 월간 기간 계산: `period_key`("YYYY-MM")로부터 `[start_ms, end_ms)`(그 달 1일 00:00 ~ 다음 달
/// 1일 00:00, 로컬) + 그 달에 걸린 주들의 월요일 key 목록을 반환한다.
///
/// 주 포함 규칙: 1일이 속한 주의 월요일부터 시작해 7일씩 전진하며 그 달 마지막 날까지의 월요일을
/// 모두 모은다. 1일이 월요일이면 그 자체가 시작점이라 "그 달 안의 월요일만" 모이고, 1일이
/// 월요일이 아니면 시작점(1일이 속한 주의 월요일)이 전달 날짜일 수 있어 "1일이 속한 주(월요일은
/// 전달)도 포함"이 자동으로 만족된다 — 두 규칙을 하나의 루프로 중복 없이 처리한다.
pub fn month_range(period_key: &str, tz: &str) -> anyhow::Result<(i64, i64, Vec<String>)> {
    let first_day = parse_local_date(&format!("{period_key}-01"))?;
    let last_day = last_day_of_month(first_day);

    let mut week_keys = Vec::new();
    let mut monday = monday_of_week(first_day);
    while monday <= last_day {
        week_keys.push(to_date_string(monday));
        monday += Duration::days(7);
    }

    let start_ms = day_range_ms(&to_date_string(first_day), tz)?.0;
    let end_ms = day_range_ms(&to_date_string(last_day), tz)?.1;
    Ok((start_ms, end_ms, week_keys))
}

const WEEKDAY_LABELS_KO: [&str; 7] = ["월", "화", "수", "목", "금", "토", "일"];
const WEEKDAY_LABELS_EN: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// 요일 라벨(locale 분기) — 주간 발췌 헤더(`## {날짜} ({요일})`)에 쓰인다.
fn weekday_label(date: NaiveDate, locale: &str) -> &'static str {
    let idx = date.weekday().num_days_from_monday() as usize;
    if locale == "ko" {
        WEEKDAY_LABELS_KO[idx]
    } else {
        WEEKDAY_LABELS_EN[idx]
    }
}

// ── period_summaries 캐시 read/write(daily_summaries의 summary::get_cached/upsert와 동일 패턴) ──

/// `(period_type, period_key, locale)` 캐시 1건 조회. FE 계약(camelCase)과 1:1 대응.
pub fn get_cached(
    conn: &Connection,
    period_type: &str,
    period_key: &str,
    locale: &str,
) -> anyhow::Result<Option<Value>> {
    conn.query_row(
        "SELECT tz, engine, model, content, created_at, prompt_version FROM period_summaries
         WHERE period_type = ?1 AND period_key = ?2 AND locale = ?3",
        params![period_type, period_key, locale],
        |row| {
            let prompt_version: String = row.get("prompt_version")?;
            Ok(json!({
                "periodType": period_type,
                "periodKey": period_key,
                "locale": locale,
                "tz": row.get::<_, String>("tz")?,
                "engine": row.get::<_, String>("engine")?,
                "model": row.get::<_, String>("model")?,
                "content": row.get::<_, String>("content")?,
                "createdAt": row.get::<_, i64>("created_at")?,
                "stale": super::is_stale_version(&prompt_version),
            }))
        },
    )
    .optional()
    .map_err(Into::into)
}

/// `(period_type, period_key, locale)` 캐시를 upsert한다(재생성 시 기존 행을 덮어씀).
/// `prompt_version`은 [`super::upsert`](daily)와 동일한 원칙으로 호출부가 넘기지 않고 이 함수
/// 내부에서 [`prompts::PROMPT_VERSION`]을 직접 기록한다 — 월간 롤업이 "낡은 주간"을 판정할 때
/// 이 값을 근거로 쓴다([`is_stale_weekly_prompt_version`]). 운영 경로([`generate_and_cache`])는
/// [`upsert_with_version`] 을 직접 부르므로 이 래퍼는 테스트에서만 쓴다.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn upsert(
    conn: &Connection,
    period_type: &str,
    period_key: &str,
    locale: &str,
    tz: &str,
    engine: &str,
    model: &str,
    content: &str,
) -> anyhow::Result<()> {
    upsert_with_version(conn, period_type, period_key, locale, tz, engine, model, content, prompts::PROMPT_VERSION)
}

/// [`upsert`] 와 같되 기록할 `prompt_version` 을 받는다 — 낡은 일간을 이어 붙여 만든 주간이 현재 버전으로
/// "세탁"되지 않게 옛 버전을 남길 때만 쓴다([`rollup_week_prompt_version`]).
#[allow(clippy::too_many_arguments)]
fn upsert_with_version(
    conn: &Connection,
    period_type: &str,
    period_key: &str,
    locale: &str,
    tz: &str,
    engine: &str,
    model: &str,
    content: &str,
    prompt_version: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO period_summaries (period_type, period_key, locale, tz, engine, model, content, created_at, prompt_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(period_type, period_key, locale) DO UPDATE SET
           tz = excluded.tz,
           engine = excluded.engine,
           model = excluded.model,
           content = excluded.content,
           created_at = excluded.created_at,
           prompt_version = excluded.prompt_version",
        params![period_type, period_key, locale, tz, engine, model, content, now_ms(), prompt_version],
    )?;
    Ok(())
}

/// 롤업 온리([`Cascade::CachedOnly`])로 다시 조립한 주간에 기록할 `prompt_version` — 그 주 일간 캐시 중
/// 낡은 것(stale — 캐시 없음은 제외)이 있으면 그 옛 버전을, 없으면 현재 [`prompts::PROMPT_VERSION`].
/// 월간 롤업이 stale 주간을 낡은 일간 그대로 다시 묶으면서 현재 버전을 찍으면, 그 주가 다시는
/// stale 로 잡히지 않아 낡은 일간 내용이 굳는다(세탁) — 옛 버전을 남겨 다음 Refresh 대상으로 둔다.
fn rollup_week_prompt_version(conn: &Connection, week_key: &str, tz: &str, locale: &str) -> anyhow::Result<String> {
    let (_start_ms, _end_ms, dates) = week_range(week_key, tz)?;
    for date in &dates {
        let stored: Option<String> = conn
            .query_row(
                "SELECT prompt_version FROM daily_summaries WHERE local_date = ?1 AND locale = ?2",
                params![date, locale],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(version) = stored.filter(|v| super::is_stale_version(v)) {
            return Ok(version);
        }
    }
    Ok(prompts::PROMPT_VERSION.to_string())
}

/// 주간 캐시(`period_summaries` WHERE `period_type = 'week'`)의 `prompt_version`이 현재
/// [`prompts::PROMPT_VERSION`]과 다르면 `true`(갱신 대상) — 캐시 행이 없으면 `false`
/// ([`super::is_stale_prompt_version`](일간)과 동일한 계약, 대상 테이블만 다르다). 월간 롤업의
/// [`Cascade::Refresh`]가 낡은 주간을 재생성할지 판단하는 데만 쓰인다([`build_monthly_excerpt`]).
fn is_stale_weekly_prompt_version(conn: &Connection, period_key: &str, locale: &str) -> anyhow::Result<bool> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT prompt_version FROM period_summaries WHERE period_type = 'week' AND period_key = ?1 AND locale = ?2",
            params![period_key, locale],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match stored {
        Some(v) => v != prompts::PROMPT_VERSION,
        None => false,
    })
}

// ── 발췌 합성(캐스케이드 생성 포함) ──────────────────────────────────

/// 캐스케이드 정책 — 상위 롤업이 하위 캐시 부재 시 얼마나 내려가며 생성할지(리뷰 Critical:
/// 콜드 스타트 팬아웃 상한. 무제한이면 월간 1건이 최악 40~50회 순차 엔진 호출로 번진다).
/// - `OneLevel`: **한 단계만** 생성 — 주간은 빠진 일간을 생성(≤7회), 월간은 빠진 주간을 생성하되
///   그 주간은 `CachedOnly`로 내려가 일간을 새로 만들지 않는다(≤6회). 사용자 수동 생성 경로.
/// - `CachedOnly`: 캐시된 하위 요약만 합성(엔진 호출 0) — 자동 캐치업의 월간 경로. 일간·주간이
///   데일리 루틴으로 자연히 쌓이면 월간도 결국 완성된다(롤업 온리).
/// - `Refresh`: 캐시가 있어도 프롬프트 버전이 낡았으면 다시 생성한다(사용자가 명시적으로 "다시
///   생성"을 눌렀을 때만, `lib.rs::generate_period_summary`의 `force=true` 경로). 캐시가 있으면
///   프롬프트 버전과 무관하게 무조건 재사용하던 버그(주간이 구버전 일간 캐시를 그대로 이어붙이는
///   문제)의 재발 방지책 — `summary::is_stale_prompt_version` 참고. 빠진 항목 생성은 `OneLevel`과
///   동일하게 동작한다(≤7회/≤6회 상한도 동일).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cascade {
    CachedOnly,
    OneLevel,
    Refresh,
}

/// 캐스케이드 하위 생성 실패를 처리한다 — **그 날/주에 데이터가 없는 정상 케이스(no_data)만
/// 스킵**하고, 그 외(엔진 미설정·CLI 실패·네트워크 등)는 즉시 전파한다(리뷰 Warning: 전부
/// 스킵하면 설정 오류가 "데이터 없음"으로 위장돼 사용자가 원인을 알 수 없다).
fn skip_or_propagate(e: anyhow::Error, context: &str) -> anyhow::Result<Option<String>> {
    if e.to_string() == NO_DATA_ERROR_CODE {
        eprintln!("[logroom] {context}: 데이터 없음 — 스킵");
        Ok(None)
    } else {
        Err(e)
    }
}

/// 주간 발췌 합성: 그 주(월~일) `daily_summaries` 7개를 `## {날짜} ({요일})` 헤더 + content로
/// 이어붙인다. 캐시에 없는 날짜는 [`super::generate_daily_and_cache`]를 호출해 그 자리에서 채운다
/// (원본 이벤트 재발췌 없음 — 하루치 발췌 생성/엔진 호출은 daily 파이프라인이 그대로 담당).
///
/// 캐시에 없는 날짜는 `cascade`가 [`Cascade::OneLevel`] 또는 [`Cascade::Refresh`]일 때만
/// 생성한다(≤7회 상한). `cascade`가 [`Cascade::Refresh`]이면 캐시가 있는 날짜라도
/// [`super::is_stale_prompt_version`]이 `true`(구버전 프롬프트로 생성된 캐시)면 그 자리에서
/// 재생성한다 — "캐시가 있으면 프롬프트 버전과 무관하게 무조건 재사용"하던 버그의 재발 방지책.
/// 그 날 데이터가 없는 경우(no_data)만 스킵하고 그 외 실패(엔진 미설정 등)는 즉시 전파한다
/// ([`skip_or_propagate`]). 7일 모두 스킵되면 [`NO_DATA_ERROR_CODE`]로 에러를 반환한다.
///
/// 발췌 첫 줄에 "# 대상 주: ..." 제목(월간의 "# 대상 월"과 동일한 패턴)을 붙이고 그 다음 줄에
/// [`excerpt::format_stats_line`]으로 만든 통계 요약(활동일·세션·요청·GitHub·Linear)을 넣는다
/// (v7) — 숫자는 [`get_period_stats`](이미 있는 SQL 집계, 새 쿼리 아님)로 계산해 LLM이 직접 세지
/// 않도록 미리 박아 넣는다.
pub async fn build_weekly_excerpt(
    db: &crate::Db,
    period_key: &str,
    tz: &str,
    locale: &str,
    cfg: &SummaryConfig,
    cascade: Cascade,
) -> anyhow::Result<String> {
    let (_start_ms, _end_ms, dates) = week_range(period_key, tz)?;

    let mut sections = Vec::new();
    // 발췌 첫 줄에 대상 주 명시 + 그 다음 줄에 통계 요약 — 월간("# 대상 월")과 동일한 패턴.
    let (week_start, week_end) = (&dates[0], &dates[6]);
    sections.push(if locale == "ko" {
        format!("# 대상 주: {week_start} ~ {week_end}")
    } else {
        format!("# Target week: {week_start}\u{2013}{week_end}")
    });
    let stats = {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
        get_period_stats(&conn, "week", period_key, tz)?
    };
    let active_days = stats["activeDays"].as_i64().unwrap_or(0);
    let lead = if locale == "ko" {
        format!("활동일 {active_days}/7")
    } else {
        format!("{active_days}/7 active days")
    };
    sections.push(excerpt::format_stats_line(
        locale,
        lead,
        stats["sessions"].as_i64().unwrap_or(0),
        stats["prompts"].as_i64().unwrap_or(0),
        stats["githubEvents"].as_i64().unwrap_or(0),
        stats["linearIssues"].as_i64().unwrap_or(0),
    ));

    for date_str in &dates {
        let cached = {
            let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            super::get_cached(&conn, date_str, locale)?
        };
        // Refresh에서만 낡음 여부를 검사한다 — CachedOnly/OneLevel은 항상 false로 취급해 기존
        // 동작(캐시가 있으면 그대로 재사용)을 그대로 지킨다.
        let is_stale = cascade == Cascade::Refresh
            && cached.is_some()
            && {
                let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
                super::is_stale_prompt_version(&conn, date_str, locale)?
            };

        let content = if let (Some(cached), false) = (&cached, is_stale) {
            cached["content"].as_str().map(str::to_string)
        } else if cached.is_none() && cascade == Cascade::CachedOnly {
            None // 롤업 온리 — 하위 생성 없음.
        } else {
            // 캐시가 없는 날짜(OneLevel/Refresh 공통) 또는 캐시는 있지만 낡은 날짜(Refresh 전용) —
            // 둘 다 generate_daily_and_cache로 그 자리에서 (재)생성한다.
            match super::generate_daily_and_cache(db, date_str, tz, locale, None, cfg).await {
                Ok(generated) => generated["content"].as_str().map(str::to_string),
                Err(e) => skip_or_propagate(e, &format!("주간 롤업 {date_str}"))?,
            }
        };

        if let Some(content) = content {
            let date = parse_local_date(date_str)?;
            let label = weekday_label(date, locale);
            sections.push(format!("## {date_str} ({label})\n\n{content}"));
        }
    }

    // sections[0..2]는 항상 "대상 주" 헤더 + 통계 줄 — 실제 날짜 섹션이 하나도 없으면 no_data.
    if sections.len() <= 2 {
        anyhow::bail!("{NO_DATA_ERROR_CODE}");
    }

    let joined = sections.join("\n\n");
    let scrubbed = scrub::scrub_text(joined.trim_end());
    Ok(truncate_with_ellipsis(&scrubbed, MAX_EXCERPT_CHARS))
}

/// 월간 발췌 합성: 그 달에 속한 주간 요약(`period_summaries('week')`)들을 헤더 + content로
/// 이어붙인다. 캐시에 없는 주는 `cascade`가 [`Cascade::OneLevel`]일 때만 생성하며(≤6회 상한),
/// **그 주간 생성은 [`Cascade::CachedOnly`]로 내려간다** — 월간에서 일간까지 파고드는 팬아웃
/// (최악 40~50회 엔진 호출)을 구조적으로 차단(리뷰 Critical). 스킵/에러 규칙은 주간과 동일
/// (no_data만 스킵, 그 외 전파). 모든 주가 스킵되면 [`NO_DATA_ERROR_CODE`].
///
/// **주의(팩트 스트립과의 경계 차이)**: 이 발췌는 "월에 걸친 주" 전체를 포함하므로 전달 말일
/// 활동이 섞일 수 있지만, `get_period_stats`의 month는 캘린더 월로만 집계한다 — 서사가 월 밖
/// 활동을 언급하지 않도록 발췌 첫 줄에 대상 월을 명시하고 월간 프롬프트가 이를 지시한다.
pub async fn build_monthly_excerpt(
    db: &crate::Db,
    period_key: &str,
    tz: &str,
    locale: &str,
    cfg: &SummaryConfig,
    cascade: Cascade,
) -> anyhow::Result<String> {
    let (_start_ms, _end_ms, week_keys) = month_range(period_key, tz)?;

    let mut sections = Vec::new();
    // 발췌 첫 줄에 대상 월 명시 — 월간 프롬프트의 "대상 월 밖 활동 무시" 지침이 참조한다.
    sections.push(if locale == "ko" {
        format!("# 대상 월: {period_key}")
    } else {
        format!("# Target month: {period_key}")
    });
    // 그 다음 줄에 통계 요약(활동일·세션·요청·GitHub·Linear) — 숫자는 get_period_stats(이미 있는
    // SQL 집계, 새 쿼리 아님)로 계산해 LLM이 직접 세지 않도록 미리 박아 넣는다(v7).
    let stats = {
        let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
        get_period_stats(&conn, "month", period_key, tz)?
    };
    let active_days = stats["activeDays"].as_i64().unwrap_or(0);
    let lead = if locale == "ko" {
        format!("활동일 {active_days}일")
    } else {
        format!("{active_days} active days")
    };
    sections.push(excerpt::format_stats_line(
        locale,
        lead,
        stats["sessions"].as_i64().unwrap_or(0),
        stats["prompts"].as_i64().unwrap_or(0),
        stats["githubEvents"].as_i64().unwrap_or(0),
        stats["linearIssues"].as_i64().unwrap_or(0),
    ));

    for week_key in &week_keys {
        let cached = {
            let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            get_cached(&conn, "week", week_key, locale)?
        };
        // Refresh에서만 낡음 여부를 검사한다(CachedOnly/OneLevel은 항상 false로 취급 — 기존 동작
        // 유지). 월간의 Refresh는 **주간 레벨까지만** 갱신한다: 아래 재생성 호출이 하위로
        // `Cascade::CachedOnly`를 내려보내므로 월간→주간→일간 팬아웃 방지 계약이 그대로 지켜진다
        // (한 번의 월간 재생성이 최대 수십 회 LLM 호출로 번지는 것을 막는다).
        let is_stale = cascade == Cascade::Refresh
            && cached.is_some()
            && {
                let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
                is_stale_weekly_prompt_version(&conn, week_key, locale)?
            };

        let content = if let (Some(cached), false) = (&cached, is_stale) {
            cached["content"].as_str().map(str::to_string)
        } else if cached.is_none() && cascade == Cascade::CachedOnly {
            None // 롤업 온리 — 하위 생성 없음.
        } else {
            // 캐시가 없는 주(OneLevel/Refresh 공통) 또는 캐시는 있지만 낡은 주(Refresh 전용).
            // month → generate_and_cache → build_excerpt → build_monthly_excerpt로 이어지는 async fn
            // 재귀는 컴파일러가 크기를 정할 수 없다(E0733) — Box::pin으로 이 간선 하나만 힙 할당해
            // 순환을 끊는다(실제 런타임 재귀 깊이는 항상 month→week 1단계뿐이라 무한 재귀는 아니다).
            match Box::pin(generate_and_cache(
                db,
                "week",
                week_key,
                tz,
                locale,
                None,
                cfg,
                Cascade::CachedOnly,
            ))
            .await
            {
                Ok(generated) => generated["content"].as_str().map(str::to_string),
                Err(e) => skip_or_propagate(e, &format!("월간 롤업 {week_key}"))?,
            }
        };

        if let Some(content) = content {
            let monday = parse_local_date(week_key)?;
            let sunday = monday + Duration::days(6);
            let (week_start, week_end) = (to_date_string(monday), to_date_string(sunday));
            let heading = if locale == "ko" {
                format!("## {week_start} ~ {week_end} 주간")
            } else {
                format!("## Week of {week_start}\u{2013}{week_end}")
            };
            sections.push(format!("{heading}\n\n{content}"));
        }
    }

    // sections[0..2]는 항상 "대상 월" 헤더 + 통계 줄 — 실제 주간 콘텐츠가 하나도 없으면 no_data.
    if sections.len() <= 2 {
        anyhow::bail!("{NO_DATA_ERROR_CODE}");
    }

    let joined = sections.join("\n\n");
    let scrubbed = scrub::scrub_text(joined.trim_end());
    Ok(truncate_with_ellipsis(&scrubbed, MAX_EXCERPT_CHARS))
}

/// `period_type`(`"week"` | `"month"`)에 맞는 합성 발췌를 만든다. `preview_period_summary_input`
/// 커맨드가 그대로 노출하고(캐스케이드 생성 포함 — "미리보기 = 실제 전송본" 계약),
/// [`generate_and_cache`]도 override가 없을 때 이 함수로 발췌를 만든다.
pub async fn build_excerpt(
    db: &crate::Db,
    period_type: &str,
    period_key: &str,
    tz: &str,
    locale: &str,
    cfg: &SummaryConfig,
    cascade: Cascade,
) -> anyhow::Result<String> {
    match period_type {
        "week" => build_weekly_excerpt(db, period_key, tz, locale, cfg, cascade).await,
        "month" => build_monthly_excerpt(db, period_key, tz, locale, cfg, cascade).await,
        other => anyhow::bail!("invalid periodType: {other}"),
    }
}

/// 발췌 생성(override 우선) → 프롬프트 조립 → 엔진 호출 → `period_summaries` upsert까지 수행한다.
/// [`super::generate_daily_and_cache`]와 동일한 구조 — 주간/월간 커맨드와 상위 캐스케이드(월간이
/// 빠진 주를 채울 때)가 이 함수를 공유한다. enabled/캡처 일시정지 체크는 호출부(커맨드) 책임이며
/// 이 함수는 하지 않는다.
#[allow(clippy::too_many_arguments)]
pub async fn generate_and_cache(
    db: &crate::Db,
    period_type: &str,
    period_key: &str,
    tz: &str,
    locale: &str,
    excerpt_override: Option<String>,
    cfg: &SummaryConfig,
    cascade: Cascade,
) -> anyhow::Result<Value> {
    // 낡은 일간을 그대로 이어 붙이는 주간 롤업만 버전 세탁 방지 대상이다(사용자 수정 발췌는 제외).
    let rollup_week = period_type == "week" && cascade == Cascade::CachedOnly && excerpt_override.is_none();
    let excerpt_text = match excerpt_override {
        Some(text) => excerpt::sanitize_excerpt_override(&text)?,
        None => build_excerpt(db, period_type, period_key, tz, locale, cfg, cascade).await?,
    };
    let prompt = prompts::prompt_for_period(period_type, locale)?;
    let combined = format!("{prompt}\n\n{excerpt_text}");
    let generated = engine::generate(&combined, cfg).await?;

    let conn = db.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
    let prompt_version = if rollup_week {
        rollup_week_prompt_version(&conn, period_key, tz, locale)?
    } else {
        prompts::PROMPT_VERSION.to_string()
    };
    upsert_with_version(
        &conn,
        period_type,
        period_key,
        locale,
        tz,
        &generated.engine,
        &generated.model,
        &generated.content,
        &prompt_version,
    )?;
    get_cached(&conn, period_type, period_key, locale)?
        .ok_or_else(|| anyhow::anyhow!("요약 저장 후 조회에 실패했습니다"))
}

// ── 팩트 집계(설계 원칙 "숫자는 DB, 서사는 LLM") ───────────────────────

/// 로컬 날짜 기준 활동일 수 — SQLite `strftime`은 IANA 타임존을 모르므로, 기간 내 이벤트 ts를
/// 모두 가져와 Rust(`chrono-tz`)로 로컬 날짜 문자열로 변환한 뒤 중복 제거한다
/// (`query::get_digest`와 동일 사상, `summary::excerpt::format_local_time`과 같은 도구 사용).
fn count_active_days(conn: &Connection, start_ms: i64, end_ms: i64, zone: &Tz) -> anyhow::Result<i64> {
    let mut stmt = conn.prepare("SELECT ts FROM events WHERE ts >= ?1 AND ts < ?2")?;
    let rows = stmt.query_map(params![start_ms, end_ms], |row| row.get::<_, i64>(0))?;

    let mut days = std::collections::HashSet::new();
    for ts in rows {
        let ts = ts?;
        if let Some(dt) = zone.timestamp_millis_opt(ts).single() {
            days.insert(dt.format("%Y-%m-%d").to_string());
        }
    }
    Ok(days.len() as i64)
}

/// 세션형 스트림(claude_code/kiro_cli, kind != 'agent') distinct 수 — `excerpt::fetch_session_streams`와
/// 동일한 소스/kind 필터.
fn count_sessions(conn: &Connection, start_ms: i64, end_ms: i64) -> anyhow::Result<i64> {
    conn.query_row(
        "SELECT COUNT(DISTINCT s.id) FROM events e
         JOIN streams s ON s.id = e.stream_id
         WHERE e.ts >= ?1 AND e.ts < ?2
           AND s.source IN ('claude_code', 'kiro_cli')
           AND s.kind != 'agent'",
        params![start_ms, end_ms],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn count_prompts(conn: &Connection, start_ms: i64, end_ms: i64) -> anyhow::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE ts >= ?1 AND ts < ?2 AND type = 'prompt'",
        params![start_ms, end_ms],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn count_github_events(conn: &Connection, start_ms: i64, end_ms: i64) -> anyhow::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE ts >= ?1 AND ts < ?2 AND source = 'github'",
        params![start_ms, end_ms],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Linear는 이슈 1개 = 스트림 1개(`excerpt::fetch_linear_issues`와 동일 매핑)라 distinct 스트림 수를 센다.
fn count_linear_issues(conn: &Connection, start_ms: i64, end_ms: i64) -> anyhow::Result<i64> {
    conn.query_row(
        "SELECT COUNT(DISTINCT stream_id) FROM events WHERE ts >= ?1 AND ts < ?2 AND source = 'linear'",
        params![start_ms, end_ms],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn count_slack_messages(conn: &Connection, start_ms: i64, end_ms: i64) -> anyhow::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE ts >= ?1 AND ts < ?2 AND source = 'slack'",
        params![start_ms, end_ms],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// 기간 팩트 집계(FE 계약, camelCase) — LLM 프롬프트에는 넣지 않고 FE가 별도 스탯 스트립으로
/// 보여준다(요약이 없어도 표시 가능, SummaryView.tsx 참고).
pub fn get_period_stats(conn: &Connection, period_type: &str, period_key: &str, tz: &str) -> anyhow::Result<Value> {
    let (start_ms, end_ms) = match period_type {
        "week" => {
            let (s, e, _) = week_range(period_key, tz)?;
            (s, e)
        }
        "month" => {
            let (s, e, _) = month_range(period_key, tz)?;
            (s, e)
        }
        other => anyhow::bail!("invalid periodType: {other}"),
    };
    let zone: Tz = tz.parse().map_err(|_| anyhow::anyhow!("invalid timezone: {tz}"))?;

    Ok(json!({
        "activeDays": count_active_days(conn, start_ms, end_ms, &zone)?,
        "sessions": count_sessions(conn, start_ms, end_ms)?,
        "prompts": count_prompts(conn, start_ms, end_ms)?,
        "githubEvents": count_github_events(conn, start_ms, end_ms)?,
        "linearIssues": count_linear_issues(conn, start_ms, end_ms)?,
        "slackMessages": count_slack_messages(conn, start_ms, end_ms)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!(
                "logroom-period-test-{}-{n}-{nanos}",
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
        external_id: &str,
    ) {
        conn.execute(
            "INSERT INTO events
               (id, stream_id, ts, source, type, title, body, model, tokens_in, tokens_out, url, parent_id, external_id, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, NULL, NULL, NULL, NULL, ?7, '{}', ?3)",
            params![id, stream_id, ts, source, event_type, title, external_id],
        )
        .unwrap();
    }

    // ── week_range/month_range(순수 경계 계산) ─────────────────────

    #[test]
    fn week_range_starts_monday_and_covers_seven_local_dates() {
        // 2026-07-13은 실제 월요일(고정 사실 — date -j -f "%Y-%m-%d" 2026-07-13 +%A 확인).
        let (start_ms, end_ms, dates) = week_range("2026-07-13", "Asia/Seoul").unwrap();
        assert_eq!(
            dates,
            vec![
                "2026-07-13",
                "2026-07-14",
                "2026-07-15",
                "2026-07-16",
                "2026-07-17",
                "2026-07-18",
                "2026-07-19",
            ]
        );
        let (day_start, _) = day_range_ms("2026-07-13", "Asia/Seoul").unwrap();
        let (_, day_end) = day_range_ms("2026-07-19", "Asia/Seoul").unwrap();
        assert_eq!(start_ms, day_start);
        assert_eq!(end_ms, day_end);
    }

    #[test]
    fn month_range_includes_leading_monday_and_all_in_month_mondays() {
        // 2026-07-01은 수요일 → 그 주의 월요일(2026-06-29, 전달)부터 시작해서 7월의 마지막 날
        // (2026-07-31)까지의 월요일들을 모두 포함해야 한다.
        let (start_ms, end_ms, week_keys) = month_range("2026-07", "Asia/Seoul").unwrap();
        assert_eq!(
            week_keys,
            vec!["2026-06-29", "2026-07-06", "2026-07-13", "2026-07-20", "2026-07-27"]
        );
        let (month_start, _) = day_range_ms("2026-07-01", "Asia/Seoul").unwrap();
        let (_, month_end) = day_range_ms("2026-07-31", "Asia/Seoul").unwrap();
        assert_eq!(start_ms, month_start);
        assert_eq!(end_ms, month_end);
    }

    #[test]
    fn month_range_when_first_day_is_monday_starts_exactly_on_first() {
        // 2026-06-01은 월요일 → 전달로 새는 주 없이 그 달 안의 월요일만 모여야 한다.
        let (_, _, week_keys) = month_range("2026-06", "Asia/Seoul").unwrap();
        assert_eq!(week_keys.first().unwrap(), "2026-06-01");
        assert!(week_keys.iter().all(|k| k.starts_with("2026-06")));
    }

    #[test]
    fn weekday_label_locale_branches_ko_and_en() {
        let monday = parse_local_date("2026-07-13").unwrap();
        assert_eq!(weekday_label(monday, "ko"), "월");
        assert_eq!(weekday_label(monday, "en"), "Mon");
        let sunday = parse_local_date("2026-07-19").unwrap();
        assert_eq!(weekday_label(sunday, "ko"), "일");
        assert_eq!(weekday_label(sunday, "en"), "Sun");
    }

    // ── period_summaries 캐시 get/upsert ───────────────────────────

    #[test]
    fn get_cached_returns_none_when_missing() {
        let (_tmp, conn) = seeded_conn();
        assert_eq!(get_cached(&conn, "week", "2026-07-13", "ko").unwrap(), None);
    }

    #[test]
    fn upsert_then_get_cached_round_trips_and_overwrites() {
        let (_tmp, conn) = seeded_conn();
        upsert(&conn, "week", "2026-07-13", "ko", "Asia/Seoul", "cli", "sonnet", "- 첫 주간 요약").unwrap();

        let cached = get_cached(&conn, "week", "2026-07-13", "ko").unwrap().expect("캐시 있어야 함");
        assert_eq!(cached["periodType"], json!("week"));
        assert_eq!(cached["periodKey"], json!("2026-07-13"));
        assert_eq!(cached["content"], json!("- 첫 주간 요약"));

        upsert(&conn, "week", "2026-07-13", "ko", "Asia/Seoul", "api", "claude-haiku-4-5", "- 재생성").unwrap();
        let recached = get_cached(&conn, "week", "2026-07-13", "ko").unwrap().unwrap();
        assert_eq!(recached["content"], json!("- 재생성"));

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM period_summaries", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "같은 (period_type, period_key, locale)은 덮어써야지 별도 행이 되면 안 됨");
    }

    #[test]
    fn get_cached_marks_old_prompt_version_as_stale() {
        let (_tmp, conn) = seeded_conn();
        upsert(&conn, "week", "2026-07-13", "ko", "Asia/Seoul", "cli", "sonnet", "- 주간").unwrap();
        assert_eq!(get_cached(&conn, "week", "2026-07-13", "ko").unwrap().unwrap()["stale"], json!(false));

        conn.execute("UPDATE period_summaries SET prompt_version = 'v9'", []).unwrap();
        assert_eq!(get_cached(&conn, "week", "2026-07-13", "ko").unwrap().unwrap()["stale"], json!(true));
    }

    #[test]
    fn rollup_week_keeps_old_version_when_any_daily_is_stale() {
        // 월간이 CachedOnly 로 다시 묶은 주간이 낡은 일간을 담았으면 현재 버전으로 세탁되면 안 된다.
        let (_tmp, conn) = seeded_conn();
        super::super::upsert(&conn, "2026-07-14", "ko", "Asia/Seoul", "cli", "sonnet", "- 화").unwrap();
        assert_eq!(
            rollup_week_prompt_version(&conn, "2026-07-13", "Asia/Seoul", "ko").unwrap(),
            prompts::PROMPT_VERSION,
            "낡은 일간이 없고 빈 날(캐시 없음)은 stale 이 아니다"
        );

        super::super::upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 목").unwrap();
        conn.execute("UPDATE daily_summaries SET prompt_version = 'v9' WHERE local_date = '2026-07-16'", [])
            .unwrap();
        let version = rollup_week_prompt_version(&conn, "2026-07-13", "Asia/Seoul", "ko").unwrap();
        assert_eq!(version, "v9");
        upsert_with_version(&conn, "week", "2026-07-13", "ko", "Asia/Seoul", "cli", "sonnet", "- 주간", &version)
            .unwrap();
        assert_eq!(get_cached(&conn, "week", "2026-07-13", "ko").unwrap().unwrap()["stale"], json!(true));
        assert!(is_stale_weekly_prompt_version(&conn, "2026-07-13", "ko").unwrap(), "다음 Refresh 대상으로 남음");
    }

    #[test]
    fn upsert_keeps_week_and_month_as_separate_rows_for_same_key() {
        let (_tmp, conn) = seeded_conn();
        upsert(&conn, "week", "2026-07-13", "ko", "Asia/Seoul", "cli", "sonnet", "- 주간").unwrap();
        upsert(&conn, "month", "2026-07-13", "ko", "Asia/Seoul", "cli", "sonnet", "- 월간").unwrap();

        assert_eq!(get_cached(&conn, "week", "2026-07-13", "ko").unwrap().unwrap()["content"], json!("- 주간"));
        assert_eq!(get_cached(&conn, "month", "2026-07-13", "ko").unwrap().unwrap()["content"], json!("- 월간"));
    }

    // ── get_period_stats(SQL 집계, 네트워크/엔진 호출 없음) ─────────

    #[test]
    fn get_period_stats_counts_within_week_window_only() {
        let (_tmp, conn) = seeded_conn();
        let (week_start, _, _) = week_range("2026-07-13", "Asia/Seoul").unwrap();

        // 07-13(월)에 세션 1개(prompt 2건) + github 1건.
        insert_stream(&conn, "claude_code:sess-1", "claude_code", "session", Some("/x/logroom"), Some("t"), week_start + 1_000, week_start + 2_000);
        insert_event(&conn, "ev-p1", "claude_code:sess-1", week_start + 1_000, "claude_code", "prompt", Some("p1"), "ext-p1");
        insert_event(&conn, "ev-p2", "claude_code:sess-1", week_start + 2_000, "claude_code", "prompt", Some("p2"), "ext-p2");
        insert_stream(&conn, "github:o/r", "github", "session", Some("o/r"), Some("o/r"), week_start + 1_000, week_start + 1_000);
        insert_event(&conn, "ev-gh1", "github:o/r", week_start + 1_000, "github", "message", Some("PR #1 opened"), "ext-gh1");

        // 07-16(목)에 linear 이슈 1개(활동 2건) + slack 메시지 1건 — 활동일이 하루 더 늘어야 함.
        let (thu_start, _) = day_range_ms("2026-07-16", "Asia/Seoul").unwrap();
        insert_stream(&conn, "linear:TICKET-1", "linear", "session", Some("Maint"), Some("이슈"), thu_start + 1_000, thu_start + 2_000);
        insert_event(&conn, "ev-l1", "linear:TICKET-1", thu_start + 1_000, "linear", "message", None, "ext-l1");
        insert_event(&conn, "ev-l2", "linear:TICKET-1", thu_start + 2_000, "linear", "message", None, "ext-l2");
        insert_stream(&conn, "slack:C1", "slack", "session", Some("ws"), Some("#general"), thu_start + 1_000, thu_start + 1_000);
        insert_event(&conn, "ev-s1", "slack:C1", thu_start + 1_000, "slack", "message", None, "ext-s1");

        // 다음 주(07-20, 월요일) 이벤트는 이 주 통계에 포함되면 안 됨.
        let (next_monday_start, _) = day_range_ms("2026-07-20", "Asia/Seoul").unwrap();
        insert_stream(&conn, "claude_code:sess-out", "claude_code", "session", Some("/x/logroom"), Some("t"), next_monday_start + 1_000, next_monday_start + 1_000);
        insert_event(&conn, "ev-out", "claude_code:sess-out", next_monday_start + 1_000, "claude_code", "prompt", Some("밖"), "ext-out");

        let stats = get_period_stats(&conn, "week", "2026-07-13", "Asia/Seoul").unwrap();
        assert_eq!(stats["activeDays"], json!(2));
        assert_eq!(stats["sessions"], json!(1));
        assert_eq!(stats["prompts"], json!(2));
        assert_eq!(stats["githubEvents"], json!(1));
        assert_eq!(stats["linearIssues"], json!(1));
        assert_eq!(stats["slackMessages"], json!(1));
    }

    #[test]
    fn get_period_stats_month_scopes_to_calendar_month_only() {
        let (_tmp, conn) = seeded_conn();
        let (july_start, _) = day_range_ms("2026-07-01", "Asia/Seoul").unwrap();
        insert_stream(&conn, "claude_code:sess-jul", "claude_code", "session", Some("/x/logroom"), Some("t"), july_start + 1_000, july_start + 1_000);
        insert_event(&conn, "ev-jul", "claude_code:sess-jul", july_start + 1_000, "claude_code", "prompt", Some("7월"), "ext-jul");

        // 6월 마지막 날(리딩 주에 포함되는 날짜) 이벤트는 "7월" 캘린더 스탯에는 포함되면 안 됨.
        let (june_end_start, _) = day_range_ms("2026-06-30", "Asia/Seoul").unwrap();
        insert_stream(&conn, "claude_code:sess-jun", "claude_code", "session", Some("/x/logroom"), Some("t"), june_end_start + 1_000, june_end_start + 1_000);
        insert_event(&conn, "ev-jun", "claude_code:sess-jun", june_end_start + 1_000, "claude_code", "prompt", Some("6월"), "ext-jun");

        let stats = get_period_stats(&conn, "month", "2026-07", "Asia/Seoul").unwrap();
        assert_eq!(stats["activeDays"], json!(1));
        assert_eq!(stats["prompts"], json!(1));
    }

    #[test]
    fn get_period_stats_rejects_invalid_period_type() {
        let (_tmp, conn) = seeded_conn();
        assert!(get_period_stats(&conn, "day", "2026-07-13", "Asia/Seoul").is_err());
    }

    // ── build_weekly_excerpt/build_monthly_excerpt: no-data 경로만(엔진 호출 없이 조기 반환) ──
    // 실 네트워크/CLI 호출 테스트는 금지 — 데이터가 전혀 없으면 daily 생성 파이프라인이
    // engine::generate에 도달하기 전(build_daily_excerpt 단계)에 실패하므로 네트워크 없이 검증 가능.

    #[tokio::test]
    async fn build_weekly_excerpt_returns_no_data_when_every_day_is_empty() {
        let (_tmp, conn) = seeded_conn();
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let cfg = SummaryConfig::default();

        let err = build_weekly_excerpt(&db, "2026-07-13", "Asia/Seoul", "ko", &cfg, Cascade::OneLevel)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[tokio::test]
    async fn build_monthly_excerpt_returns_no_data_when_every_week_is_empty() {
        let (_tmp, conn) = seeded_conn();
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let cfg = SummaryConfig::default();

        let err = build_monthly_excerpt(&db, "2026-07", "Asia/Seoul", "ko", &cfg, Cascade::OneLevel)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[tokio::test]
    async fn cached_only_cascade_never_generates_lower_layers() {
        // 롤업 온리(CachedOnly) 경로: 하위 캐시가 전혀 없으면 엔진 호출 시도 없이 no_data로 끝나야
        // 한다 — 자동 캐치업 월간의 팬아웃 방지 계약(리뷰 Critical). SummaryConfig::default()는
        // enabled=false지만 build_*는 설정을 보지 않으므로, 여기서 엔진에 도달했다면 CLI 감지
        // 시도(claude --version)로 이어졌을 것 — no_data 즉시 반환이 그 경로가 없음을 보증한다.
        let (_tmp, conn) = seeded_conn();
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let cfg = SummaryConfig::default();

        let err = build_weekly_excerpt(&db, "2026-07-13", "Asia/Seoul", "ko", &cfg, Cascade::CachedOnly)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
        let err = build_monthly_excerpt(&db, "2026-07", "Asia/Seoul", "ko", &cfg, Cascade::CachedOnly)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[tokio::test]
    async fn refresh_cascade_reuses_cache_when_prompt_version_is_current() {
        // Refresh의 핵심 안전장치: 최신 버전 캐시까지 다시 만들면 "다시 생성" 한 번에 7일치 LLM
        // 호출이 불필요하게 발생한다. 현재 버전 캐시는 그대로 재사용돼야 한다(엔진 미도달).
        let (_tmp, conn) = seeded_conn();
        super::super::upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 목요일 작업").unwrap();
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let cfg = SummaryConfig::default();

        let excerpt = build_weekly_excerpt(&db, "2026-07-13", "Asia/Seoul", "ko", &cfg, Cascade::Refresh)
            .await
            .unwrap();
        assert!(excerpt.contains("- 목요일 작업"), "최신 버전 캐시는 재생성 없이 그대로 쓰여야 함");
    }

    #[tokio::test]
    async fn refresh_cascade_regenerates_stale_prompt_version_cache() {
        // 낡은 버전 캐시(개편 전 형식)는 Refresh에서 다시 만들어야 한다 — 그러지 않으면 주간이
        // 구버전 일간을 그대로 이어붙여 개편된 형식이 반영되지 않는다(이 수정의 원인이 된 버그).
        let (_tmp, conn) = seeded_conn();
        super::super::upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 구버전 요약").unwrap();
        conn.execute(
            "UPDATE daily_summaries SET prompt_version = '' WHERE local_date = '2026-07-16'",
            [],
        )
        .unwrap();
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let cfg = SummaryConfig::default();

        // 그날 원본 이벤트가 없어 재생성은 no_data로 스킵되고, 낡은 캐시 내용도 쓰이지 않는다 —
        // 즉 캐시를 그냥 재사용하지 않고 실제로 재생성을 시도했다는 증거다.
        let err = build_weekly_excerpt(&db, "2026-07-13", "Asia/Seoul", "ko", &cfg, Cascade::Refresh)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), NO_DATA_ERROR_CODE);
    }

    #[tokio::test]
    async fn build_weekly_excerpt_skips_empty_days_and_assembles_cached_ones() {
        let (_tmp, conn) = seeded_conn();
        // 07-16(목)만 캐시가 있고 나머지 6일은 데이터가 전혀 없어(daily 생성이 조용히 스킵) 결국
        // "## 2026-07-16 (목)" 섹션 하나만 포함된 발췌가 만들어져야 한다 — 네트워크 호출 없음
        // (캐시 히트 경로만 타므로 engine::generate에 도달하지 않는다).
        super::super::upsert(&conn, "2026-07-16", "ko", "Asia/Seoul", "cli", "sonnet", "- 목요일 작업").unwrap();
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let cfg = SummaryConfig::default();

        let excerpt = build_weekly_excerpt(&db, "2026-07-13", "Asia/Seoul", "ko", &cfg, Cascade::OneLevel)
            .await
            .unwrap();
        assert!(excerpt.contains("## 2026-07-16 (목)"));
        assert!(excerpt.contains("- 목요일 작업"));
        // "# 대상 주" 제목에 그 주 월요일(2026-07-13)이 들어가는 건 정상이다 — 여기서 검증하려는
        // 것은 데이터 없는 날짜의 "## " 섹션 자체가 없다는 점이다.
        assert!(!excerpt.contains("## 2026-07-13"), "데이터 없는 날짜는 섹션 자체가 없어야 함");
    }

    #[tokio::test]
    async fn build_weekly_excerpt_includes_stats_line() {
        // v7: 발췌 첫 줄("# 대상 주") 다음에 "활동 요약" 통계가 들어가야 한다 — 숫자는
        // get_period_stats(기존 집계 함수) 재사용, LLM이 직접 세지 않도록 미리 계산해 넣는다.
        let (_tmp, conn) = seeded_conn();
        let (week_start, _, _) = week_range("2026-07-13", "Asia/Seoul").unwrap();
        insert_stream(&conn, "claude_code:sess-1", "claude_code", "session", Some("/x/logroom"), Some("t"), week_start + 1_000, week_start + 2_000);
        insert_event(&conn, "ev-p1", "claude_code:sess-1", week_start + 1_000, "claude_code", "prompt", Some("p1"), "ext-p1");
        super::super::upsert(&conn, "2026-07-13", "ko", "Asia/Seoul", "cli", "sonnet", "- 월요일 작업").unwrap();
        let db: crate::Db = Arc::new(Mutex::new(conn));
        let cfg = SummaryConfig::default();

        let excerpt = build_weekly_excerpt(&db, "2026-07-13", "Asia/Seoul", "ko", &cfg, Cascade::OneLevel)
            .await
            .unwrap();
        assert!(excerpt.contains("# 대상 주: 2026-07-13 ~ 2026-07-19"));
        assert!(
            excerpt.contains("활동 요약: 활동일 1/7 · 세션 1개 · 요청 1건"),
            "통계 줄이 형식대로 들어가야 함: {excerpt}"
        );
        assert!(!excerpt.contains("GitHub 활동"), "GitHub 이벤트가 없으면 생략돼야 함");
        assert!(!excerpt.contains("Linear 활동"), "Linear 이벤트가 없으면 생략돼야 함");
    }
}
