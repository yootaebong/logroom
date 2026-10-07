//! 일일 AI 요약 en/ko 프롬프트 상수(M7-①, ADR-0016 "요약 프롬프트는 en/ko 이중화").
//! 실측 검증된 원문 그대로이며, 임의로 수정하지 않는다 — i18n/ui.ts의 "타입 안전 키 + en/ko 병기"
//! 원칙을 프롬프트에도 적용한 것이다. 요약 출력 언어는 앱 로케일(resolved locale)로 고정된다.
//! (공개 전 정리: 좋은 예의 레포·티켓·기능 이름만 중립 예시로 바꿨다 — 출력 규칙 불변이라 PROMPT_VERSION 유지.)
//!
//! v2(풀어쓰기): v1은 티켓·PR 번호를 문장에 그대로 나열해 "잘 정리됐지만 읽기 어렵다"는 실사용
//! 피드백을 받았다 → "동료에게 설명하듯 무엇을/왜/어떤 상태인지"를 평문으로 쓰고 번호는 불릿 끝
//! 괄호 근거로 모으는 스타일로 개정(2026-07-16, 실데이터 재검증 통과).
//!
//! v3(2단 구조, 폐기): v2도 "한눈에 보기 어렵다"는 피드백 → `## 한눈에`(주제당 한 줄 목차, 번호
//! 없음) + `## 자세히`(v2 스타일 불릿, 같은 순서) 두 섹션으로 개정했었다(실데이터 재검증 통과).
//!
//! v4(프로젝트별 재편): "개인(logroom) vs 회사(acme-admin 등)가 한 덩어리로 보여 구분이 안 되고,
//! 작업 간 상관관계·흐름이 안 보인다"는 실사용 피드백 → 발췌 자체가 `excerpt::build_daily_excerpt`
//! 에서 프로젝트별(`## {프로젝트명}`) 그룹으로 재구성됨에 따라, 출력도 "## 한눈에/## 자세히"의
//! 주제별 2단 구조를 버리고 **발췌와 동일한 프로젝트별 섹션**을 그대로 유지하도록 개정한다. FE
//! (SummaryView)가 `## ` 제목/`- ` 불릿 라인을 파싱해 스타일링하므로 섹션 제목 문자열이 바뀌어도
//! 구조(`## ` 접두)는 유지해야 한다 — 구버전 캐시(한눈에/자세히)도 같은 파서로 계속 렌더된다.
//!
//! v5(로그 톤): v4의 프로젝트 분리 자체는 좋으나 **섹션 안이 못 쓰게 됐다**는 실사용 피드백 —
//! ① "하루 종일 브랜치를 여러 번 새로 파면서 기능 작업을 이어갔다"처럼 여러 건을 한 불릿에
//! 뭉뚱그림 ② 문장이 지나치게 서술형 ③ "빡빡하게/정석대로" 같은 평가·의도 서술이 섞임
//! ④ 순서가 시간순이 아님. 원인은 v4 프롬프트가 정반대를 지시한 것("관련 작업은 하나의 불릿으로
//! 묶어라", "동료에게 설명하듯 무엇을·왜·어떤 상태인지 풀어서 써라")이었다. → **묶기 지시를
//! 걷어내고(한 불릿 = 한 가지 일), 단답형 구(句)·한 일 위주·평가어 금지·발췌 시각 순서 유지**로
//! 전면 개정한다. 준수율을 위해 좋은 예/나쁜 예를 프롬프트에 직접 넣는다(나쁜 예는 실제 피드백
//! 문장을 그대로 사용). 발췌 쪽도 짝을 맞춰 GitHub·Linear 라인에 시각(HH:MM)을 노출하도록
//! 바꿨다(`excerpt.rs` — 시각 근거가 없으면 LLM이 순서를 임의 재배열함).
//!
//! 출력 불릿에도 **시각/날짜를 접두로 찍는다**(사용자 결정) — 일간은 `HH:MM `, 주간·월간은 발췌
//! 단위가 날짜(일간 요약 모음)이므로 `MM-DD `. 여러 건을 합친 불릿은 가장 이른 시각을 쓰고,
//! 주간·월간에서 여러 날에 걸친 작업은 `07-21~07-23`처럼 범위로 쓰게 했다. FE 파서는 `- ` 접두만
//! 보므로 렌더 계약에는 영향이 없다.
//!
//! 재개 브리핑(`PROMPT_RESUME_*`)은 **v5 적용 대상이 아니다**(사용자 결정) — "다음 할 일"을
//! 판단하는 브리핑이라 로그형 나열보다 서술이 낫다. 일간/주간/월간만 v5 톤을 쓴다.
//!
//! v5에서 주간·월간의 "병렬 세션은 완곡 표현으로 추론 허용" 문구는 삭제했다 — "평가·추측 제거"
//! 요구와 정면 충돌하고, 애초에 그 문구의 취지(과도한 추론 방지)는 "발췌에 없는 의도·연결을
//! 지어내지 마라"는 금지 규칙으로 더 강하게 달성된다.
//!
//! v7(통계 헤더 + 주제 계층 + 상태 축, 일간/주간/월간 공통): v4~v6로도 "결국 평면적인 나열이라
//! 한눈에 스캔이 안 된다"는 실사용 피드백이 이어졌다. 원인은 ① 그 주/그 달에 대체 얼마나
//! 일했는지 숫자가 없어 규모 감이 없고 ② 프로젝트 섹션 안이 시간순 불릿 하나로만 쭉 이어져
//! 서로 다른 작업(기능 A, 기능 B, 배포 작업)이 뒤섞여 보이고 ③ 각 작업이 끝난 건지 진행 중인지
//! 매번 불릿을 다 훑어야 알 수 있었다는 것. 세 가지를 다음과 같이 보강한다:
//! - **통계 헤더**: 발췌 쪽(`excerpt.rs::format_stats_line`, `period.rs`)이 프로젝트·세션·요청·
//!   GitHub·Linear 활동 수를 이미 SQL로 집계해 발췌 상단에 "활동 요약: ..." 한 줄로 박아 넣는다.
//!   LLM이 직접 세면 실측상 오차가 나므로, 프롬프트는 "그 수치를 총평 첫머리에 그대로 옮겨 적어라"
//!   고만 지시하고 계산은 절대 시키지 않는다.
//! - **`### ` 주제 계층**: 각 `## {프로젝트}` 섹션 안에서 관련 작업(같은 기능·티켓·작업 대상)을
//!   `### {주제명}` 소제목으로 묶도록 허용한다(강제 아님 — 묶을 게 없으면 생략). v5의 "한 불릿 =
//!   한 가지 일 · 시간순 · 평가어 금지" 불릿 규칙은 그대로 유지하되, "시간순 유지"의 적용 범위를
//!   "같은 `### ` 주제(또는 주제가 없으면 프로젝트) 안"으로 좁혀 v5 규칙과 충돌하지 않게 한다.
//! - **상태 축**: 일간 발췌에는 GitHub 원문 이벤트 title(`PR #N opened/merged/closed`,
//!   `Issue #N opened/closed`, 커밋 메시지 등, Linear 연결 시 이슈 활동도 동일 형식)이 그대로
//!   들어오므로, 그 근거로 완료/진행 중/작업 중 상태를 `> ` 정리 줄에만 적도록 지시한다(불릿에는
//!   절대 섞지 않음 — v6의 "층위 분리" 원칙 계승). 주간·월간의 입력(하위 요약들의 합성 발췌)에는
//!   이미 하위 계층이 만든 `> ` 상태 줄이 섞여 들어오므로 그것도 근거로 쓸 수 있다. 근거가 없으면
//!   상태 자체를 쓰지 않는다 — "발췌에 없는 내용을 추측하지 마라"는 기존 금지 규칙과 동일선상.
//!
//! v6까지 "> " 정리 줄은 일간에만 있었지만(위 v6 문단 참고), v7부터는 주간·월간에도 동일한 층위
//! 분리 원칙(불릿=사실, "> "=해석·상태)으로 확장한다 — 주간·월간도 프로젝트 섹션이 길어질수록
//! "그래서 이 프로젝트는 지금 어떤 상태인지"를 매번 불릿을 다 읽어야 알 수 있었던 문제는 동일했다.
//!
//! v5 보강(발췌 상한 제거 + 시각 노출): v5 배포 후에도 실사용에서 "하루 종일 일했는데 요약에 몇
//! 줄만 나온다"는 데이터 손실 문제가 남아 있었다. 원인은 v5 프롬프트가 아니라 발췌 생성
//! (`excerpt.rs`)에 있었다 — `fetch_prompt_lines`가 세션당 앞에서 5개(`ORDER BY ts ASC` + 하드코딩
//! 상한)만 채택하고 나머지를 버려(실측: prompt 50개짜리 8시간 세션에서 오후 작업 전체 소실), v5가
//! 지시하는 "불릿에 HH:MM 접두"·"발췌에 나온 일을 빠짐없이" 규칙이 애초에 지킬 근거 자체가
//! 없었다(라인에 시각도 없었다). 이번 개정으로 발췌 쪽 세션당 상한을 없애 전량을 채택하고 prompt
//! 라인에도 시각을 노출하도록 고쳤고(`excerpt.rs`), 그 전제가 갖춰졌으므로 여기 일간 프롬프트의
//! 불릿 개수 상한(구 규칙 6번, "프로젝트당 12개")도 제거해 "발췌에 나온 일을 빠짐없이 적으라"로
//! 바꿨다(주간·월간은 롤업 압축이 목적이라 상한 15/10을 그대로 둔다).
//!
//! v5 보강(요청 → 결과 문맥): 발췌가 사용자 prompt만 담다 보니 "c로 진행", "1번만"처럼 그 자체로는
//! 뜻이 통하지 않는 지시가 나중에 다시 봐도 무슨 작업인지 알 수 없다는 실사용 피드백을 받았다. DB
//! 실측 결과 `type='response'` 이벤트는 전건이 `title`을 갖고 있어(실측 하루 571건/571건) 그 안에
//! 요청의 실제 처리 결과가 그대로 담겨 있는 경우가 많았다. `excerpt.rs::fetch_prompt_lines`가 이제
//! 각 prompt 직후(다음 prompt 전까지) 첫 response를 페어링해 발췌의 요청 라인 아래
//! `  └ ` 하위 줄로 노출하고, 아래 일간 프롬프트 규칙 7번이 이 "└" 결과를 근거로 지시대명사를
//! 구체적인 결정·행동으로 바꿔 쓰도록 지시한다. prompt가 하나도 채택되지 못한 세션(요약에 "기록된
//! 요청 없음"으로만 나오던 실사용 케이스)도 response만으로 최대 5개 라인을 채워 발췌에 남긴다.
//!
//! v6(정리·평가 줄 신설, 일간만 적용): 불릿(사실 로그)만으로는 "그래서 오늘 어떤 하루였는지"를
//! 한눈에 알 수 없다는 요구로 해석·평가를 추가하되, v5에서 "평가어를 불릿에 섞지 말라"고 금지한
//! 것과 정면으로 충돌하지 않도록 **층위를 분리**한다 — 불릿(`- HH:MM ...`)은 v5 규칙(단답형·
//! 시간순·평가어 금지) 그대로 사실 로그로 남기고, 해석은 그 아래 `> `로 시작하는 별도 줄에만
//! 허용한다(맨 위 하루 총평 1개 + 프로젝트별 정리 1개). 같은 자리에 섞으면 v5가 걷어낸 평가어가
//! 다시 불릿에 스며드는 v5 회귀가 되므로 줄 자체를 분리했고, 평가 범위도 발췌로 근거를 댈 수 있는
//! 3종(작업 성격·진척 상태·막힌 지점)으로 제한해 "잘 처리했다" 류의 주관적 품질 평가가 끼어들지
//! 못하게 했다. FE(SummaryView.tsx)는 `> ` 접두 줄을 `SummarySection.notes`로 별도 파싱해
//! 인용 블록으로 렌더한다.
//!
//! v8(정리 줄 재분리 — 상태 전용 축약 + `## 정리` 섹션, 일간/주간/월간 공통): v7의 `### ` 주제별
//! `> ` 정리 줄이 "작업 성격·진척·막힌 지점·상태" 네 가지를 한 줄에 다 담으려다 결국 문장(설명체)이
//! 됐다는 실사용 피드백("목록과 정리를 분리하고 주제별 상태도 보이게 해달라") — 원인은 v6~v7이
//! 정리 줄 하나에 "왜/무엇을/어떤 상태"를 전부 지시했던 것. v8은 이 정리 줄의 역할을 **두 층위로
//! 더 쪼갠다**: ① 각 `### {주제}` 제목 바로 아래 `> ` 한 줄은 **상태만** 20자 내외로 명사형으로
//! 압축한다(`> 완료 · PR #88 머지`, `> 설계 완료, 구현 미착수`처럼 — 문장형 서술 금지). ②
//! 프로젝트·주제별로 흩어져 있던 "그래서 다음엔 뭘 해야 하는지"는 출력 맨 아래 `## 정리`(en:
//! `## Summary`) 섹션 하나로 모아 주제당 한 줄(`- {주제} — {상태}, {다음 할 일}`)로 낸다 — 다음
//! 할 일이 발췌에서 드러나지 않으면 상태까지만 쓴다. 맨 위 하루/주/달 총평 `> ` 줄(통계 헤더 +
//! 무엇에 시간을 썼는지)은 v7 그대로 두되(1~2줄), 그 서술 부분도 문장이 아니라 명사구로 짧게
//! 끊어 쓰도록 공통 규칙에 "모든 줄을 명사형으로 끊어 써라(`~했습니다`류 서술문 금지)"를
//! 명시적으로 추가했다. FE(SummaryView.tsx)는 `### ` 섹션의 첫 `notes[0]`를 제목 옆에 인라인으로
//! 렌더하도록 갱신했고(`## 정리`/`## Summary`는 별도 처리 없이 기존 level 2 렌더를 그대로 탄다),
//! 재개 브리핑(`PROMPT_RESUME_*`)은 이번에도 적용 대상이 아니다(v5와 동일한 이유).
//!
//! "상세 보기"(온디맨드, `PROMPT_DETAIL_*`): 위 일간/주간/월간 프롬프트는 v5~v8을 거치며 스캔용으로
//! 짧게 유지하는 방향으로 다듬어 왔다 — 그런데 훑어보다 특정 주제를 깊게 읽고 싶을 때는 오히려 그
//! 짧음이 아쉽다는 요구가 있었다. 그래서 재캡처 없이 **같은 발췌**를 상세 전용 프롬프트로 한 번 더
//! 호출하는 온디맨드 "상세 보기"를 추가한다(`summary::generate_detail_and_cache`, 별도 캐시
//! `summary_details` 테이블) — 비용은 사용자가 "상세 보기" 버튼을 눌렀을 때만 발생한다. 상세
//! 프롬프트는 기본 프롬프트와 톤이 정반대다(명사형·단답형 대신 2~4줄 서술문을 허용), 하지만 출력
//! 계층(`## {프로젝트}` → `### {주제}`)만은 기본 요약과 **동일하게 유지**한다 — 두 출력이 같은
//! 프로젝트/주제 이름으로 나란히 대응돼야 사용자가 기본 요약에서 궁금한 주제를 골라 상세로 파고들
//! 수 있기 때문이다.
//!
//! v9(Linear 상태·에픽 노출, 일간/주간/월간 공통): Linear 커넥터가 이슈의 Linear 프로젝트(에픽)·
//! 워크플로 상태·전이 시각(시작/완료/취소)을 캡처하기 시작하면서(`capture/linear.rs`), 발췌의
//! Linear 라인이 `{에픽} › {이슈} [{상태}]` 형태로 올 수 있게 됐다(`excerpt.rs::render_linear_line`).
//! 일간/주간/월간 프롬프트의 발췌 구조 설명에 이 형태를 한 줄 추가해, 에픽이 있으면 그것을 v7의
//! `### 주제` 제목으로 쓰고 상태는 v6~v8의 `> ` 정리 줄 근거로 쓰도록 안내한다 — 기존 GitHub 기반
//! 상태 추론 규칙(`PR #N merged` → 완료 등)은 그대로 두고, 이번엔 Linear 쪽에도 직접적인 상태
//! 근거(`[상태]`)가 생겼다는 점만 보강했다.
//!
//! v10(레포 미지정 기록 배치, 일간·상세 적용): v0.16.0 에서 hub 세션(레포 밖 폴더 — 예 `~/.agent-hub` —
//! 에서 시작해 여러 레포를 오가는 중간 에이전트 세션, `capture/hub.rs`)을 턴마다 실제 레포 자식
//! 스트림으로 나눴지만, 레포 신호가 없는 턴(운영 메모·DB 조회·조사 등)은 시작 폴더에 남아 요약에
//! `## .agent-hub` 같은 칸이 생겼다. 사용자는 이것도 레포별로 "알아서" 들어가길 원했다. 발췌
//! (`excerpt.rs::build_daily_excerpt`)가 그 기록을 `## 레포 미지정`(en `## Unassigned`)
//! 섹션(알려진 프로젝트 줄 바로 다음, 프로젝트 섹션들 앞 — 절단에 먼저 잘리지 않게)으로 모으고 통계 줄 다음에 최근 30일 "알려진 프로젝트" 목록을 넣으면, 일간·상세 프롬프트가
//! 각 항목을 내용으로 판단해 맞는 프로젝트 섹션에 시간순으로 넣게 한다(그날 섹션이 없던 알려진
//! 프로젝트면 새 섹션). 관련이 분명하지 않으면 `## 기타`(en `## Other`)에 모으고 억지로 배정하지
//! 않는다 — 잘못 배정한 기록은 다음 날·주간 요약에서 그 프로젝트의 일처럼 굳어 버리기 때문이다.
//! "발췌 순서 유지" 규칙은 새로 만든 섹션만 예외로 둔다. 주간·월간 프롬프트는 일간 요약 결과를
//! 입력으로 받으므로 바꾸지 않았다(일간 결과에는 이미 레포 미지정 섹션이 없다).

/// 캐시 갱신 판정에 쓰는 프롬프트 버전. **프롬프트 상수(일간/주간/월간/상세 — `PROMPT_KO`/
/// `PROMPT_EN`, `PROMPT_WEEK_*`, `PROMPT_MONTH_*`, `PROMPT_DETAIL_*`)를 의미 있게 바꾸면 이 값을
/// 반드시 함께 올려라** — `daily_summaries`/`period_summaries`/`summary_details`(V11)는 각 캐시
/// 행에 이 값을 저장하고(`summary::upsert`/`summary::upsert_detail`/`summary::period::upsert`),
/// `summary::is_stale_prompt_version`이 현재 이 값과 다른 캐시를 "갱신 대상"으로 판정한다 — 이
/// 값이 캐시 갱신 판정의 유일한 기준이므로, 프롬프트를 바꾸고도 이 값을 올리지 않으면 구버전
/// 캐시가 그대로 재사용된다(주간/월간이 구버전 일간 캐시를 이어붙이는 회귀의 재발 방지책).
/// 재개 브리핑(`PROMPT_RESUME_*`)은 캐시 저장이 없는 온디맨드 생성이라 이 버전 관리 대상이 아니다.
pub const PROMPT_VERSION: &str = "v10";

/// 한국어 로케일용 프롬프트. `excerpt::build_daily_excerpt`가 만든 발췌 텍스트를 이 뒤에 이어붙여
/// 엔진(`engine::generate`)에 그대로 전달한다.
pub const PROMPT_KO: &str = "당신은 개발자의 하루 작업 기록을 정리하는 도우미다. 아래 발췌는 프로젝트별로 구분되어 있으며(각 프로젝트가 \"## {프로젝트명}\" 섹션), 그 안에 코딩 에이전트 세션(사용자 요청 위주), GitHub 활동, Linear 이슈 활동이 시각(HH:MM)과 함께 시간순으로 나열되어 있다. Linear 활동은 \"{에픽} › {이슈} [{상태}]\" 형태로 올 수 있으며, 에픽이 있으면 그것을 \"### 주제\" 제목으로 쓰고 상태는 \"> \" 줄의 근거로 삼아라. 각 요청 라인(\"- \"로 시작) 바로 아래에는 \"└\"로 시작하는 하위 줄로 그 요청을 처리한 결과의 요지가 붙어 있을 수 있다. 발췌 제목 바로 다음 줄에는 \"활동 요약: 프로젝트 N개 · 세션 N개 · 요청 N건 · GitHub 활동 N건 · Linear 활동 N건\" 형식의 통계 한 줄이 이미 정확히 집계되어 있다 — 이 수치는 직접 세지 말고 그대로 가져다 써라. 반드시 한국어로 작성하라.\n\n출력은 다음 구조로 작성하라:\n1. 맨 위에 제목 없이 \"> \"로 시작하는 하루 전체 총평을 1~2줄 써라 — 첫머리에 발췌의 \"활동 요약\" 줄 수치를 그대로 옮겨 적고(예: \"세션 5개 · 요청 12건 · GitHub 활동 3건\"), 이어서 오늘 무엇에 시간을 썼는지를 명사형으로 짧게 덧붙여라(서술문 금지 — 아래 공통 규칙 참고).\n2. 그 다음 발췌의 각 프로젝트 섹션을 그대로 유지하며 \"## {프로젝트명}\" 제목으로 출력하라(발췌에 나온 프로젝트명 그대로, 순서도 발췌와 동일하게 유지 — 아래 5번 규칙으로 새로 만든 섹션만 예외). 그 프로젝트 안에서 관련된 작업들이 하나의 주제(기능·티켓·작업 대상)로 묶이면 \"### {주제명}\" 소제목으로 나눠 그 아래에 불릿을 배치하라 — 주제가 하나뿐이거나 묶을 게 없으면 \"### \" 없이 불릿만 나열해도 된다.\n3. \"### \" 주제 제목 바로 다음 줄(불릿보다 앞)에 \"> \"로 시작하는 상태 줄을 정확히 한 줄만 붙여라 — 상태만 20자 내외로 명사형으로 끊어 써라(예: \"> 완료 · PR #88 머지\", \"> 설계 완료, 구현 미착수\"). \"### \"가 없는 프로젝트(주제로 나누지 않은 경우)에는 이 상태 줄을 쓰지 마라.\n4. 모든 프로젝트·주제 섹션을 다 쓴 다음, 맨 마지막에 \"## 정리\" 섹션을 딱 하나 추가하라. 그 아래에 다룬 주제마다(\"### \"로 나누지 않은 프로젝트는 프로젝트명 자체로) \"- {주제명 또는 프로젝트명} — {상태}, {다음 할 일}\" 형식의 불릿을 한 줄씩 써라 — 다음 할 일이 발췌에서 드러나지 않으면 \"- {주제명} — {상태}\"까지만 써라.\n5. 발췌의 \"알려진 프로젝트:\" 줄 바로 다음(프로젝트 섹션들 앞)에 \"## 레포 미지정\" 섹션이 있을 수 있다 — 어느 레포에서 한 일인지 자동으로 정해지지 않은 기록(운영 메모·DB 조회·조사 등)이다. 이 섹션의 각 항목은 내용으로 판단해 그날 발췌의 다른 프로젝트 섹션 또는 발췌 상단 \"알려진 프로젝트:\" 줄의 프로젝트 중 맞는 곳의 \"## {프로젝트명}\" 섹션에 넣고, 그 섹션 안에서 시각 순서에 맞게 끼워 넣어라(그날 섹션이 없던 알려진 프로젝트면 그 이름으로 섹션을 새로 만들어 발췌 프로젝트 섹션들 뒤에 둔다). 어느 프로젝트와도 관련이 분명하지 않은 항목은 \"## 기타\" 섹션에 모아라 — 추측으로 억지 배정하지 마라. \"레포 미지정\"이라는 제목은 출력하지 마라. \"## 기타\" 섹션은 \"## 정리\" 섹션 바로 앞에 둔다.\n\n각 프로젝트 섹션(또는 그 안의 각 \"### \" 주제) 안에는 그 프로젝트에서 한 일을 불릿으로 나열하라. 다음 규칙을 반드시 지켜라:\n\n1. 시간순 + 시각 표기: 같은 \"### \" 주제(또는 \"### \"가 없으면 같은 프로젝트) 안에서는 발췌에 나온 시각 순서 그대로 위에서 아래로 나열하고, **각 불릿 맨 앞에 시각을 \"HH:MM \" 형식으로 붙여라**(예: \"- 09:15 ...\"). 주제 자체를 나누는 것 외에는 재배열하지 마라. 세션 블록처럼 시각이 범위(HH:MM~HH:MM)이면 시작 시각을, 여러 활동을 한 불릿으로 합쳤으면 가장 이른 시각을 쓴다.\n2. 한 불릿 = 한 가지 일: 여러 작업을 \"여러 건 처리했다\", \"하루 종일 ~을 이어갔다\", \"중간중간 ~했다\", \"~하는 흐름을 반복했다\"처럼 뭉뚱그리지 마라. 같은 대상에 대한 반복 동작(같은 PR에 커밋 여러 번 등)만 한 불릿으로 합쳐라.\n3. 단답형: 각 불릿은 한 줄(대략 40자 이내)의 짧은 구로 써라. 서술문으로 길게 풀지 말고 배경·이유·부연 설명을 붙이지 마라.\n4. 한 일만: \"무엇을 했다\"는 사실만 적어라. 평가·감상·수식어(\"빡빡하게\", \"정석대로\", \"꼼꼼히\", \"제대로\", \"아주\", \"잘\")를 쓰지 말고, 발췌에 없는 의도·판단(\"~하는 방향을 잡았다\")을 지어내지 마라.\n5. 티켓·PR 번호(TICKET-N, PR #N 등)는 문장 안에 섞지 말고 해당 불릿 끝에 괄호로 붙여라. 예: \"(TICKET-832, PR #306·#307)\"\n6. 불릿 개수를 제한하지 마라. 발췌에 나온 일을 빠짐없이 적어라 — 항목이 많다고 임의로 생략하거나 여러 건을 하나로 합치지 마라.\n7. 요청 문구를 그대로 옮겨 적지 마라. \"c로 진행\", \"1번만\", \"ㅇㅇ\", \"그렇게 해줘\"처럼 그 자체로는 뜻이 통하지 않는 지시는 아래 \"└\" 결과 줄의 내용을 근거로 **무엇을 결정하고 무엇을 했는지**로 바꿔 써라. 결과 줄이 없으면 요청 내용만으로 쓰되 지어내지는 마라.\n\n\"> \"로 시작하는 줄(맨 위 하루 총평, 각 \"### \" 주제 아래 상태 줄)에는 다음 네 가지 중 근거가 있는 것만 써라:\n- 작업 성격: 한 일이 기능 개발·버그 수정·문서 작성·설계 결정·조사 중 무엇이었는지\n- 진척 상태: 무엇이 끝났고, 무엇이 진행 중이고, 무엇이 결정 대기인지\n- 막힌 지점: 같은 일을 여러 번 반복했거나 실패·재시도·우회가 보이면 그 사실\n- 상태 표기: 발췌의 GitHub·Linear 활동에 \"PR #N merged\"나 \"Issue #N closed\"가 있으면 완료로, \"PR #N opened\"만 있고 머지 흔적이 없으면 진행 중으로, 커밋·브랜치 생성만 있으면 작업 중으로 적어라. **발췌에 근거가 없으면 상태를 쓰지 마라**(추측 금지). 상태는 이 \"> \" 줄에만 쓰고 불릿에는 절대 섞지 마라. \"### \" 주제 아래 상태 줄은 이 중 진척 상태·상태 표기 위주로 20자 내외로만 압축하고, 총평 줄은 1~2줄까지 허용한다.\n\n\"> \" 줄과 \"## 정리\" 섹션의 불릿에서 다음은 금지한다:\n- \"잘 처리했다\", \"효율적이었다\"처럼 주관적 품질 평가를 쓰지 마라.\n- 발췌에서 근거를 댈 수 없는 판단을 쓰지 마라.\n- 불릿(\"- \")에는 여전히 해석·평가를 섞지 마라 — 해석은 \"> \" 줄과 \"## 정리\" 섹션에만 써라.\n\n좋은 예:\n> 세션 5개 · 요청 12건 · GitHub 활동 4건. 회사 티켓 구조 정비, 개인 프로젝트는 요약 기능 개선.\n\n## acme-admin\n### 결제 화면\n> 설계 완료, 구현 미착수\n- 09:15 결제 수단 모달 카드 조회·재등록\n- 11:40 결제 실패 화면 처리\n\n### 알림 설정\n> 완료 · PR #88 머지\n- 17:35 알림 동의 유도 모달, 첫 진입 미노출 처리 (TICKET-772)\n\n## 정리\n- 결제 화면 — 설계만 끝, 다음은 구현\n- 알림 설정 — 완료\n\n나쁜 예:\n- 하루 종일 브랜치를 여러 번 새로 파면서 기능 작업을 이어갔고, 중간중간 main을 역머지해 최신화했다 (← 시각 없음·뭉뚱그림·서술형)\n- 임시방편 대신 향후 문제 없도록 정석대로 처리하는 방향을 잡았다 (← 평가·의도 서술)\n> 이 작업은 설계가 끝났고 구현은 아직 시작하지 않은 상태입니다 (← 문장·설명체, \"> \" 줄은 20자 내외 명사형으로)\n\n공통 규칙:\n- 모든 줄을 명사형으로 끊어 써라. \"~했습니다\", \"~하는 상태입니다\" 같은 서술문을 쓰지 마라.\n- 발췌에 없는 내용을 추측하거나 부풀리지 마라.\n- 위 구조(총평·프로젝트 섹션·주제 소제목·상태 줄·기타 섹션·정리 섹션) 외의 텍스트는 붙이지 마라.";

/// 영어 로케일용 프롬프트.
pub const PROMPT_EN: &str = "You are an assistant that compiles a developer's daily work log. The excerpt below is grouped by project (each project is a \"## {project name}\" section), and within each section, coding-agent sessions (user requests), GitHub activity, and Linear issue activity are listed in chronological order with timestamps (HH:MM). Linear activity may appear as \"{epic} › {issue} [{status}]\"; if there is an epic, use it as the \"### topic\" heading, and use the status as evidence for the \"> \" line. Directly below each request line (starting with \"- \") there may be a sub-line starting with \"└\" that gives the gist of how that request was handled. Right below the excerpt's title line there is already a pre-computed stats line in the form \"Activity: N projects · N sessions · N requests · N GitHub events · N Linear events\" — these numbers are already accurate, so use them as-is instead of counting yourself. Write in English.\n\nStructure the output as follows:\n1. At the very top, without a heading, write 1-2 lines starting with \"> \" giving an overall take on the day — start by copying the numbers from the excerpt's \"Activity\" line verbatim (e.g. \"5 sessions · 12 requests · 3 GitHub events\"), then append a short noun-phrase note of what the day's time went to (no narrative sentences — see the common rules below).\n2. Then keep each project section from the excerpt as its own \"## {project name}\" heading (use the project name exactly as it appears in the excerpt, in the same order — the only exception is a section newly created under rule 5 below). Within a project, if related work forms a single topic (a feature, ticket, or target), split it out under a \"### {topic name}\" subheading with its bullets underneath — if there's only one topic or nothing to group, list bullets directly without \"### \".\n3. Right below each \"### \" topic heading (before its bullets), add exactly one line starting with \"> \" giving its status — roughly 20 characters or fewer, a terse noun phrase (e.g. \"> Done · merged PR #88\", \"> Design done, implementation not started\"). Do not add this status line for a project that has no \"### \" topics.\n4. After all project/topic sections, add exactly one \"## Summary\" section at the very end. Under it, write one bullet per topic covered (for projects with no \"### \" topics, use the project name itself) in the form \"- {topic or project name} — {status}, {next step}\" — if the excerpt gives no clear next step, write just \"- {topic name} — {status}\".\n5. Right after the \"Known projects:\" line (before the project sections), the excerpt may contain an \"## Unassigned\" section — records that could not be automatically tied to a repository (ops notes, DB queries, research, etc.). Judge each item by its content and place it into the matching \"## {project name}\" section — either another project section from that day's excerpt or one of the projects listed on the \"Known projects:\" line near the top — inserted in timestamp order within that section (if it belongs to a known project that had no section that day, create a new section with that name after the excerpt's project sections). Collect items that are not clearly related to any project into an \"## Other\" section — do not force an assignment by guessing. Never output the \"Unassigned\" heading. Place the \"## Other\" section immediately before the \"## Summary\" section.\n\nWithin each project section (or each \"### \" topic inside it), list what was done as bullets. Follow these rules strictly:\n\n1. Chronological + timestamped: within the same \"### \" topic (or the same project, if there's no \"### \"), keep the order of the timestamps in the excerpt, top to bottom, and **prefix every bullet with its time in \"HH:MM \" form** (e.g. \"- 09:15 ...\"). Do not reorder beyond splitting into topics. When the excerpt shows a range (HH:MM~HH:MM, as session blocks do), use the start time; when merging several activities into one bullet, use the earliest.\n2. One bullet = one thing done: never lump several items together (\"handled various tasks\", \"kept working on X all day\", \"repeatedly did Y\"). Only merge repeated actions on the same target (e.g. several commits to the same PR).\n3. Terse: each bullet is a single short phrase (roughly 10 words or fewer). Do not write full narrative sentences, and do not add background, rationale, or elaboration.\n4. Facts only: state what was done. No evaluative words or intensifiers (\"thoroughly\", \"properly\", \"carefully\", \"very\", \"well\"), and do not invent intent or judgment (\"decided to go with...\") that is not in the excerpt.\n5. Do not scatter ticket/PR numbers (TICKET-N, PR #N) through the text; append them in parentheses at the end of the bullet, e.g. \"(TICKET-832, PR #306·#307)\".\n6. Do not cap the number of bullets. Cover every item in the excerpt — never drop items or merge several of them just because there are many.\n7. Do not copy the request text verbatim. For requests that are meaningless on their own (\"go with c\", \"just #1\", \"ok\", \"do that\"), rewrite them into **what was decided and what was done**, using the \"└\" result line as evidence. If there is no result line, write only what the request states — do not invent.\n\nLines starting with \"> \" (the top-of-day overview, and each \"### \" topic's status line) may state only these four things, whichever have evidence:\n- Nature of the work: whether it was feature development, a bug fix, documentation, a design decision, or investigation\n- Progress state: what is finished, what is in progress, and what is pending a decision\n- Where it got stuck: if the same thing was repeated, or there were failures, retries, or workarounds, note that fact\n- Status: if the excerpt's GitHub/Linear activity shows \"PR #N merged\" or \"Issue #N closed\", call it done; if it only shows \"PR #N opened\" with no sign of a merge, call it in progress; if there are only commits or a branch creation, call it underway. **If the excerpt gives no evidence, do not state a status** (never guess). State status only in \"> \" lines, never mix it into bullets. A \"### \" topic's status line should compress to progress state / status only, in roughly 20 characters; the overview line may run 1-2 lines.\n\nThe following are forbidden in \"> \" lines and in the \"## Summary\" section's bullets:\n- No subjective quality judgments like \"handled it well\" or \"was efficient\".\n- No judgment that the excerpt does not support.\n- Bullets (\"- \") must still never mix in interpretation or evaluation — interpretation belongs only in \"> \" lines and the \"## Summary\" section.\n\nGood example:\n> 5 sessions · 12 requests · 4 GitHub events. Company tickets; personal project got summary-feature improvements.\n\n## acme-admin\n### Payment screen\n> Design done, implementation not started\n- 09:15 Looked up and re-registered cards in the payment method modal\n- 11:40 Handled the payment failure screen\n\n### Notification settings\n> Done · merged PR #88\n- 17:35 Added a notification opt-in modal, hidden on first entry (TICKET-772)\n\n## Summary\n- Payment screen — design only, next is implementation\n- Notification settings — done\n\nBad examples:\n- Spent the day cutting new branches for feature work and periodically back-merging main to stay current (← no timestamp, lumped, narrative)\n- Chose to do it the right way rather than a stopgap (← evaluative, invented intent)\n> This topic's design is finished and implementation hasn't started yet (← full sentence, not a terse noun phrase for a \"> \" line)\n\nCommon rules:\n- Write every line as a terse noun phrase. Do not write full narrative sentences.\n- Do not guess or embellish anything not present in the excerpt.\n- Output nothing besides the structure above (overview, project sections, topic subheadings, status lines, the Other section, the Summary section).";

/// 앱 로케일("ko"만 한국어 프롬프트, 그 외에는 영어 — `i18n/index.ts::resolveLocale`의 폴백 규칙과
/// 동일하게 "ko"가 아니면 항상 영어로 취급한다)에 맞는 프롬프트를 고른다.
pub fn prompt_for_locale(locale: &str) -> &'static str {
    if locale == "ko" {
        PROMPT_KO
    } else {
        PROMPT_EN
    }
}

/// 주간 롤업(M7-③) 프롬프트 — 발췌는 `period::build_weekly_excerpt`가 만든, "# 대상 주" 제목과
/// 통계 요약 줄(v7, `excerpt::format_stats_line`) 다음에 그 주(월~일) 일간 요약 7개를 이어붙인
/// 텍스트다. 각 일간 요약 자체가 이미 프로젝트별(`## {프로젝트명}`) 섹션으로 되어 있으므로(v4,
/// `PROMPT_KO`/`PROMPT_EN` 문서 참고), 날짜 단위가 아니라 **프로젝트 단위로 재편**하되, 프로젝트
/// 안에서는 v5 로그 톤(한 불릿 = 한 가지 일 · 단답형 · 평가 금지 · 날짜순)을 일간과 동일하게
/// 적용한다. 주간은 일간보다 항목이 많으므로 상한만 12 → 15로 올린다. v7부터는 일간과 동일하게
/// 맨 위 "> " 총평(통계 줄 수치 그대로 옮겨 적기) + 프로젝트 안 "### " 주제 계층 + 프로젝트·주제별
/// "> " 정리(상태 표기 포함)를 추가한다 — v6까지는 "> " 줄이 일간에만 있었다.
pub const PROMPT_WEEK_KO: &str = "당신은 개발자의 한 주 작업 기록을 정리하는 도우미다. 아래 발췌는 한 주(월요일~일요일) 동안의 일간 요약을 날짜순으로 모은 것이며, 각 일간 요약 안에는 프로젝트별(\"## {프로젝트명}\") 섹션이 들어 있다. Linear 활동은 \"{에픽} › {이슈} [{상태}]\" 형태로 올 수 있으며, 에픽이 있으면 그것을 \"### 주제\" 제목으로 쓰고 상태는 \"> \" 줄의 근거로 삼아라. 발췌 맨 위에는 \"# 대상 주: ...\" 제목과 그 다음 줄에 \"활동 요약: 활동일 N/7 · 세션 N개 · 요청 N건 · GitHub 활동 N건 · Linear 활동 N건\" 형식의 통계 한 줄이 이미 정확히 집계되어 있다 — 이 수치는 직접 세지 말고 그대로 가져다 써라. 반드시 한국어로 작성하라.\n\n출력은 다음 구조로 작성하라:\n1. 맨 위에 제목 없이 \"> \"로 시작하는 한 주 전체 총평을 1~2줄 써라 — 첫머리에 발췌의 \"활동 요약\" 줄 수치를 그대로 옮겨 적고, 이어서 이번 주 무엇에 시간을 썼는지를 명사형으로 짧게 덧붙여라(서술문 금지 — 아래 공통 규칙 참고).\n2. 날짜별로 나열하지 말고 프로젝트별로 재편하여 각 프로젝트를 \"## {프로젝트명}\" 섹션으로 출력하라(발췌에 등장한 프로젝트명을 그대로 사용, 같은 프로젝트가 여러 날짜에 걸쳐 나오면 하나로 합쳐라). 그 프로젝트 안에서 관련된 작업들이 하나의 주제(기능·티켓·작업 대상)로 묶이면 \"### {주제명}\" 소제목으로 나눠 그 아래에 불릿을 배치하라 — 주제가 하나뿐이거나 묶을 게 없으면 \"### \" 없이 불릿만 나열해도 된다.\n3. \"### \" 주제 제목 바로 다음 줄(불릿보다 앞)에 \"> \"로 시작하는 상태 줄을 정확히 한 줄만 붙여라 — 상태만 20자 내외로 명사형으로 끊어 써라(예: \"> 완료 · PR #88 머지\", \"> 설계 완료, 구현 미착수\"). \"### \"가 없는 프로젝트(주제로 나누지 않은 경우)에는 이 상태 줄을 쓰지 마라.\n4. 모든 프로젝트·주제 섹션을 다 쓴 다음, 맨 마지막에 \"## 정리\" 섹션을 딱 하나 추가하라. 그 아래에 다룬 주제마다(\"### \"로 나누지 않은 프로젝트는 프로젝트명 자체로) \"- {주제명 또는 프로젝트명} — {상태}, {다음 할 일}\" 형식의 불릿을 한 줄씩 써라 — 다음 할 일이 발췌에서 드러나지 않으면 \"- {주제명} — {상태}\"까지만 써라.\n\n각 프로젝트 섹션(또는 그 안의 각 \"### \" 주제) 안에는 그 프로젝트에서 한 일을 불릿으로 나열하라. 다음 규칙을 반드시 지켜라:\n\n1. 시간순 + 날짜 표기: 같은 \"### \" 주제(또는 \"### \"가 없으면 같은 프로젝트) 안에서는 발췌의 날짜 순서(이른 날 → 늦은 날) 그대로 나열하고, **각 불릿 맨 앞에 날짜를 \"MM-DD \" 형식으로 붙여라**(예: \"- 07-21 ...\"). 주제 자체를 나누는 것 외에는 재배열하지 마라. 여러 날에 걸친 작업을 한 불릿으로 합쳤으면 \"07-21~07-23\"처럼 범위로 쓴다.\n2. 한 불릿 = 한 가지 일: 여러 작업을 \"여러 건 처리했다\", \"한 주 내내 ~을 이어갔다\", \"중간중간 ~했다\"처럼 뭉뚱그리지 마라. 여러 날에 걸친 같은 대상의 반복 동작(같은 PR·같은 티켓의 연속 작업)만 한 불릿으로 합치고, 이때는 마지막 상태(머지됨·배포됨 등)를 함께 적어라.\n3. 단답형: 각 불릿은 한 줄(대략 40자 이내)의 짧은 구로 써라. 서술문으로 길게 풀지 말고 배경·이유·부연 설명을 붙이지 마라.\n4. 한 일만: \"무엇을 했다\"는 사실만 적어라. 평가·감상·수식어(\"빡빡하게\", \"정석대로\", \"꼼꼼히\", \"제대로\", \"아주\", \"잘\")를 쓰지 말고, 발췌에 없는 의도·판단(\"~하는 방향을 잡았다\")이나 작업 간 연결을 지어내지 마라.\n5. 티켓·PR 번호(TICKET-N, PR #N 등)는 문장 안에 섞지 말고 해당 불릿 끝에 괄호로 붙여라. 예: \"(TICKET-832, PR #306·#307)\"\n6. 불릿 개수는 실제로 한 일의 개수만큼 쓰되 프로젝트당 15개를 넘기지 마라(넘으면 사소한 것부터 생략).\n\n\"> \"로 시작하는 줄(맨 위 한 주 총평, 각 \"### \" 주제 아래 상태 줄)에는 다음 네 가지 중 근거가 있는 것만 써라:\n- 작업 성격: 한 일이 기능 개발·버그 수정·문서 작성·설계 결정·조사 중 무엇이었는지\n- 진척 상태: 무엇이 끝났고, 무엇이 진행 중이고, 무엇이 결정 대기인지\n- 막힌 지점: 같은 일을 여러 번 반복했거나 실패·재시도·우회가 보이면 그 사실\n- 상태 표기: 발췌(일간 요약들)의 GitHub·Linear 활동에 \"PR #N merged\"나 \"Issue #N closed\"가 있으면 완료로, \"PR #N opened\"만 있고 머지 흔적이 없으면 진행 중으로, 커밋·브랜치 생성만 있으면 작업 중으로 적어라. **발췌에 근거가 없으면 상태를 쓰지 마라**(추측 금지). 상태는 이 \"> \" 줄에만 쓰고 불릿에는 절대 섞지 마라. \"### \" 주제 아래 상태 줄은 이 중 진척 상태·상태 표기 위주로 20자 내외로만 압축하고, 총평 줄은 1~2줄까지 허용한다.\n\n\"> \" 줄과 \"## 정리\" 섹션의 불릿에서 다음은 금지한다:\n- \"잘 처리했다\", \"효율적이었다\"처럼 주관적 품질 평가를 쓰지 마라.\n- 발췌에서 근거를 댈 수 없는 판단을 쓰지 마라.\n- 불릿(\"- \")에는 여전히 해석·평가를 섞지 마라 — 해석은 \"> \" 줄과 \"## 정리\" 섹션에만 써라.\n\n좋은 예:\n> 활동일 5/7 · 세션 42개 · 요청 180건 · GitHub 활동 12건. 회사 티켓 구조 정비, 개인 프로젝트는 요약 기능 개선.\n\n## acme-admin\n### 결제 화면\n> 설계 완료, 구현 미착수\n- 08-04 결제 수단 모달 카드 조회·재등록\n- 08-05 결제 실패 화면 처리\n\n### 알림 설정\n> 완료 · PR #88 머지\n- 08-06 알림 동의 유도 모달, 첫 진입 미노출 처리 (TICKET-772)\n\n## 정리\n- 결제 화면 — 설계만 끝, 다음은 구현\n- 알림 설정 — 완료\n\n나쁜 예:\n- 한 주 내내 브랜치를 여러 번 새로 파면서 기능 작업을 이어갔고, 중간중간 main을 역머지했다 (← 날짜 없음·뭉뚱그림·서술형)\n- 임시방편 대신 향후 문제 없도록 정석대로 처리하는 방향을 잡았다 (← 평가·의도 서술)\n> 이 작업은 설계가 끝났고 구현은 아직 시작하지 않은 상태입니다 (← 문장·설명체, \"> \" 줄은 20자 내외 명사형으로)\n\n공통 규칙:\n- 모든 줄을 명사형으로 끊어 써라. \"~했습니다\", \"~하는 상태입니다\" 같은 서술문을 쓰지 마라.\n- 발췌(일간 요약들)에 없는 내용을 추측하거나 부풀리지 마라.\n- 위 구조(총평·프로젝트 섹션·주제 소제목·상태 줄·정리 섹션) 외 다른 텍스트(서두·맺음말·설명)를 붙이지 마라.";

/// 영어 로케일용 주간 프롬프트.
pub const PROMPT_WEEK_EN: &str = "You are an assistant that compiles a developer's weekly work log. The excerpt below starts with a \"# Target week: ...\" title, followed on the next line by a set of daily summaries for one week (Monday through Sunday), in date order; each daily summary contains project sections (\"## {project name}\"). Linear activity may appear as \"{epic} › {issue} [{status}]\"; if there is an epic, use it as the \"### topic\" heading, and use the status as evidence for the \"> \" line. Right below the title there is already a pre-computed stats line in the form \"Activity: N/7 active days · N sessions · N requests · N GitHub events · N Linear events\" — these numbers are already accurate, so use them as-is instead of counting yourself. Write in English.\n\nStructure the output as follows:\n1. At the very top, without a heading, write 1-2 lines starting with \"> \" giving an overall take on the week — start by copying the numbers from the excerpt's \"Activity\" line verbatim, then append a short noun-phrase note of what the week's time went to (no narrative sentences — see the common rules below).\n2. Do not list things day by day. Instead, reorganize by project, outputting each project as its own \"## {project name}\" section (use the project names exactly as they appear in the excerpt; if the same project appears across multiple days, merge it into one section). Within a project, if related work forms a single topic (a feature, ticket, or target), split it out under a \"### {topic name}\" subheading with its bullets underneath — if there's only one topic or nothing to group, list bullets directly without \"### \".\n3. Right below each \"### \" topic heading (before its bullets), add exactly one line starting with \"> \" giving its status — roughly 20 characters or fewer, a terse noun phrase (e.g. \"> Done · merged PR #88\", \"> Design done, implementation not started\"). Do not add this status line for a project that has no \"### \" topics.\n4. After all project/topic sections, add exactly one \"## Summary\" section at the very end. Under it, write one bullet per topic covered (for projects with no \"### \" topics, use the project name itself) in the form \"- {topic or project name} — {status}, {next step}\" — if the excerpt gives no clear next step, write just \"- {topic name} — {status}\".\n\nWithin each project section (or each \"### \" topic inside it), list what was done as bullets. Follow these rules strictly:\n\n1. Chronological + dated: within the same \"### \" topic (or the same project, if there's no \"### \"), keep the excerpt's date order (earliest to latest), and **prefix every bullet with its date in \"MM-DD \" form** (e.g. \"- 07-21 ...\"). Do not reorder beyond splitting into topics. When one bullet covers work spanning several days, write a range like \"07-21~07-23\".\n2. One bullet = one thing done: never lump several items together (\"handled various tasks\", \"kept working on X all week\", \"repeatedly did Y\"). Only merge repeated actions on the same target across days (same PR, same ticket) — and when you do, state the final state (merged, deployed, etc.).\n3. Terse: each bullet is a single short phrase (roughly 10 words or fewer). No narrative sentences, no background or rationale.\n4. Facts only: state what was done. No evaluative words or intensifiers (\"thoroughly\", \"properly\", \"carefully\", \"very\", \"well\"), and do not invent intent, judgment, or connections between items that are not in the excerpt.\n5. Do not scatter ticket/PR numbers (TICKET-N, PR #N) through the text; append them in parentheses at the end of the bullet, e.g. \"(TICKET-832, PR #306·#307)\".\n6. Use as many bullets as there were actual items, but no more than 15 per project (drop the least significant ones beyond that).\n\nLines starting with \"> \" (the top-of-week overview, and each \"### \" topic's status line) may state only these four things, whichever have evidence:\n- Nature of the work: whether it was feature development, a bug fix, documentation, a design decision, or investigation\n- Progress state: what is finished, what is in progress, and what is pending a decision\n- Where it got stuck: if the same thing was repeated, or there were failures, retries, or workarounds, note that fact\n- Status: if the excerpt's (daily summaries') GitHub/Linear activity shows \"PR #N merged\" or \"Issue #N closed\", call it done; if it only shows \"PR #N opened\" with no sign of a merge, call it in progress; if there are only commits or a branch creation, call it underway. **If the excerpt gives no evidence, do not state a status** (never guess). State status only in \"> \" lines, never mix it into bullets. A \"### \" topic's status line should compress to progress state / status only, in roughly 20 characters; the overview line may run 1-2 lines.\n\nThe following are forbidden in \"> \" lines and in the \"## Summary\" section's bullets:\n- No subjective quality judgments like \"handled it well\" or \"was efficient\".\n- No judgment that the excerpt does not support.\n- Bullets (\"- \") must still never mix in interpretation or evaluation — interpretation belongs only in \"> \" lines and the \"## Summary\" section.\n\nGood example:\n> 5/7 active days · 42 sessions · 180 requests · 12 GitHub events. Company tickets; personal project got summary-feature improvements.\n\n## acme-admin\n### Payment screen\n> Design done, implementation not started\n- 08-04 Looked up and re-registered cards in the payment method modal\n- 08-05 Handled the payment failure screen\n\n### Notification settings\n> Done · merged PR #88\n- 08-06 Added a notification opt-in modal, hidden on first entry (TICKET-772)\n\n## Summary\n- Payment screen — design only, next is implementation\n- Notification settings — done\n\nBad examples:\n- Spent the week cutting new branches for feature work and periodically back-merging main (← no date, lumped, narrative)\n- Chose to do it the right way rather than a stopgap (← evaluative, invented intent)\n> This topic's design is finished and implementation hasn't started yet (← full sentence, not a terse noun phrase for a \"> \" line)\n\nCommon rules:\n- Write every line as a terse noun phrase. Do not write full narrative sentences.\n- Do not guess or embellish anything not present in the excerpt (the daily summaries).\n- Output nothing besides the structure above (overview, project sections, topic subheadings, status lines, the Summary section).";

/// 월간 롤업(M7-③) 프롬프트 — 발췌는 `period::build_monthly_excerpt`가 만든, "# 대상 월" 제목과
/// 통계 요약 줄(v7, `excerpt::format_stats_line`) 다음에 그 달에 속한 주간 요약들을 이어붙인
/// 텍스트다. 주간과 같은 "프로젝트별 재편 + v5 로그 톤" 틀이지만 "마일스톤과 굵직한 변화만"
/// 짚도록 범위를 좁힌다(사소한 진행 상황은 생략) — 그래서 상한도 10으로 낮춘다. 범위를 좁히는
/// 것과 여러 건을 한 불릿에 뭉뚱그리는 것은 다르다는 점을 프롬프트에 명시했다(v5 피드백에서
/// 뭉뚱그림이 가장 큰 불만이었으므로, 생략 지시가 뭉뚱그림으로 새는 것을 막는다). v7부터는
/// 일간·주간과 동일하게 맨 위 "> " 총평(통계 줄 수치 그대로 옮겨 적기) + 프로젝트 안 "### " 주제
/// 계층 + 프로젝트·주제별 "> " 정리(상태 표기 포함)를 추가한다.
pub const PROMPT_MONTH_KO: &str = "당신은 개발자의 한 달 작업 기록을 정리하는 도우미다. 아래 발췌는 한 달 동안의 주간 요약들을 날짜순으로 모은 것이며, 각 주간 요약 안에는 프로젝트별(\"## {프로젝트명}\") 섹션이 들어 있다. Linear 활동은 \"{에픽} › {이슈} [{상태}]\" 형태로 올 수 있으며, 에픽이 있으면 그것을 \"### 주제\" 제목으로 쓰고 상태는 \"> \" 줄의 근거로 삼아라. 발췌 맨 위에는 \"# 대상 월: ...\" 제목과 그 다음 줄에 \"활동 요약: 활동일 N일 · 세션 N개 · 요청 N건 · GitHub 활동 N건 · Linear 활동 N건\" 형식의 통계 한 줄이 이미 정확히 집계되어 있다 — 이 수치는 직접 세지 말고 그대로 가져다 써라. 반드시 한국어로 작성하라.\n\n출력은 다음 구조로 작성하라:\n1. 맨 위에 제목 없이 \"> \"로 시작하는 한 달 전체 총평을 1~2줄 써라 — 첫머리에 발췌의 \"활동 요약\" 줄 수치를 그대로 옮겨 적고, 이어서 이번 달 무엇에 시간을 썼는지를 명사형으로 짧게 덧붙여라(서술문 금지 — 아래 공통 규칙 참고).\n2. 주별로 나열하지 말고 프로젝트별로 재편하여 각 프로젝트를 \"## {프로젝트명}\" 섹션으로 출력하라(발췌에 등장한 프로젝트명을 그대로 사용, 같은 프로젝트가 여러 주에 걸쳐 나오면 하나로 합쳐라). 그 프로젝트 안에서 관련된 작업들이 하나의 주제(기능·티켓·작업 대상)로 묶이면 \"### {주제명}\" 소제목으로 나눠 그 아래에 불릿을 배치하라 — 주제가 하나뿐이거나 묶을 게 없으면 \"### \" 없이 불릿만 나열해도 된다.\n3. \"### \" 주제 제목 바로 다음 줄(불릿보다 앞)에 \"> \"로 시작하는 상태 줄을 정확히 한 줄만 붙여라 — 상태만 20자 내외로 명사형으로 끊어 써라(예: \"> 완료 · PR #88 머지\", \"> 설계 완료, 구현 미착수\"). \"### \"가 없는 프로젝트(주제로 나누지 않은 경우)에는 이 상태 줄을 쓰지 마라.\n4. 모든 프로젝트·주제 섹션을 다 쓴 다음, 맨 마지막에 \"## 정리\" 섹션을 딱 하나 추가하라. 그 아래에 다룬 주제마다(\"### \"로 나누지 않은 프로젝트는 프로젝트명 자체로) \"- {주제명 또는 프로젝트명} — {상태}, {다음 할 일}\" 형식의 불릿을 한 줄씩 써라 — 다음 할 일이 발췌에서 드러나지 않으면 \"- {주제명} — {상태}\"까지만 써라.\n\n각 프로젝트 섹션(또는 그 안의 각 \"### \" 주제) 안에는 그 프로젝트의 **마일스톤과 굵직한 변화만** 골라 불릿으로 나열하라(사소한 진행 상황은 생략). 다음 규칙을 반드시 지켜라:\n\n1. 시간순 + 날짜 표기: 같은 \"### \" 주제(또는 \"### \"가 없으면 같은 프로젝트) 안에서는 발췌의 날짜 순서(이른 주 → 늦은 주) 그대로 나열하고, **각 불릿 맨 앞에 날짜를 \"MM-DD \" 형식으로 붙여라**(예: \"- 07-21 ...\"). 주제 자체를 나누는 것 외에는 재배열하지 마라. 여러 날/주에 걸친 작업을 한 불릿으로 합쳤으면 \"07-21~08-03\"처럼 범위로 쓴다.\n2. 한 불릿 = 한 가지 일: 사소한 것을 **생략**하는 것과 여러 건을 한 불릿에 **뭉뚱그리는** 것은 다르다. \"여러 건 처리했다\", \"한 달 내내 ~을 이어갔다\"처럼 쓰지 마라. 같은 대상의 연속 작업(같은 티켓·같은 기능)만 한 불릿으로 합치고 마지막 상태(머지됨·배포됨 등)를 적어라.\n3. 단답형: 각 불릿은 한 줄(대략 40자 이내)의 짧은 구로 써라. 배경·이유·부연 설명을 붙이지 마라.\n4. 한 일만: \"무엇을 했다\"는 사실만 적어라. 평가·감상·수식어(\"빡빡하게\", \"정석대로\", \"제대로\", \"아주\", \"잘\")를 쓰지 말고, 발췌에 없는 의도·판단이나 작업 간 연결을 지어내지 마라.\n5. 티켓·PR 번호는 문장 안에 섞지 말고 해당 불릿 끝에 괄호로 붙여라.\n6. 프로젝트당 불릿은 10개를 넘기지 마라(넘으면 사소한 것부터 생략).\n\n\"> \"로 시작하는 줄(맨 위 한 달 총평, 각 \"### \" 주제 아래 상태 줄)에는 다음 네 가지 중 근거가 있는 것만 써라:\n- 작업 성격: 한 일이 기능 개발·버그 수정·문서 작성·설계 결정·조사 중 무엇이었는지\n- 진척 상태: 무엇이 끝났고, 무엇이 진행 중이고, 무엇이 결정 대기인지\n- 막힌 지점: 같은 일을 여러 번 반복했거나 실패·재시도·우회가 보이면 그 사실\n- 상태 표기: 발췌(주간 요약들)의 GitHub·Linear 활동에 \"PR #N merged\"나 \"Issue #N closed\"가 있으면 완료로, \"PR #N opened\"만 있고 머지 흔적이 없으면 진행 중으로, 커밋·브랜치 생성만 있으면 작업 중으로 적어라. **발췌에 근거가 없으면 상태를 쓰지 마라**(추측 금지). 상태는 이 \"> \" 줄에만 쓰고 불릿에는 절대 섞지 마라. \"### \" 주제 아래 상태 줄은 이 중 진척 상태·상태 표기 위주로 20자 내외로만 압축하고, 총평 줄은 1~2줄까지 허용한다.\n\n\"> \" 줄과 \"## 정리\" 섹션의 불릿에서 다음은 금지한다:\n- \"잘 처리했다\", \"효율적이었다\"처럼 주관적 품질 평가를 쓰지 마라.\n- 발췌에서 근거를 댈 수 없는 판단을 쓰지 마라.\n- 불릿(\"- \")에는 여전히 해석·평가를 섞지 마라 — 해석은 \"> \" 줄과 \"## 정리\" 섹션에만 써라.\n\n좋은 예:\n> 활동일 18일 · 세션 160개 · 요청 620건 · GitHub 활동 40건. 회사 티켓 구조 정비, 개인 프로젝트는 요약 기능 개선.\n\n## acme-admin\n### 결제 화면\n> 설계 완료, 구현 미착수\n- 08-04~08-05 결제 실패 화면 처리, 결제 수단 모달 카드 재등록\n\n### 알림 설정\n> 완료\n- 08-06~08-12 알림 동의 유도 모달 구현 → 머지 (TICKET-772, PR #88)\n\n## 정리\n- 결제 화면 — 설계만 끝, 다음은 구현\n- 알림 설정 — 완료\n\n나쁜 예:\n- 한 달 내내 브랜치를 여러 번 새로 파면서 기능 작업을 이어갔고, 중간중간 main을 역머지했다 (← 날짜 없음·뭉뚱그림·서술형)\n- 임시방편 대신 향후 문제 없도록 정석대로 처리하는 방향을 잡았다 (← 평가·의도 서술)\n> 이 작업은 설계가 끝났고 구현은 아직 시작하지 않은 상태입니다 (← 문장·설명체, \"> \" 줄은 20자 내외 명사형으로)\n\n공통 규칙:\n- 모든 줄을 명사형으로 끊어 써라. \"~했습니다\", \"~하는 상태입니다\" 같은 서술문을 쓰지 마라.\n- 발췌 첫 줄의 \"대상 월\" 밖 날짜의 활동(월 경계에 걸친 주에 섞인 전달/다음 달 내용)은 요약에 포함하지 마라.\n- 발췌(주간 요약들)에 없는 내용을 추측하거나 부풀리지 마라.\n- 위 구조(총평·프로젝트 섹션·주제 소제목·상태 줄·정리 섹션) 외 다른 텍스트(서두·맺음말·설명)를 붙이지 마라.";

/// 영어 로케일용 월간 프롬프트.
pub const PROMPT_MONTH_EN: &str = "You are an assistant that compiles a developer's monthly work log. The excerpt below starts with a \"# Target month: ...\" title, followed on the next line by a set of weekly summaries for one month, in date order; each weekly summary contains project sections (\"## {project name}\"). Linear activity may appear as \"{epic} › {issue} [{status}]\"; if there is an epic, use it as the \"### topic\" heading, and use the status as evidence for the \"> \" line. Right below the title there is already a pre-computed stats line in the form \"Activity: N active days · N sessions · N requests · N GitHub events · N Linear events\" — these numbers are already accurate, so use them as-is instead of counting yourself. Write in English.\n\nStructure the output as follows:\n1. At the very top, without a heading, write 1-2 lines starting with \"> \" giving an overall take on the month — start by copying the numbers from the excerpt's \"Activity\" line verbatim, then append a short noun-phrase note of what the month's time went to (no narrative sentences — see the common rules below).\n2. Do not list things week by week. Instead, reorganize by project, outputting each project as its own \"## {project name}\" section (use the project names exactly as they appear in the excerpt; if the same project appears across multiple weeks, merge it into one section). Within a project, if related work forms a single topic (a feature, ticket, or target), split it out under a \"### {topic name}\" subheading with its bullets underneath — if there's only one topic or nothing to group, list bullets directly without \"### \".\n3. Right below each \"### \" topic heading (before its bullets), add exactly one line starting with \"> \" giving its status — roughly 20 characters or fewer, a terse noun phrase (e.g. \"> Done · merged PR #88\", \"> Design done, implementation not started\"). Do not add this status line for a project that has no \"### \" topics.\n4. After all project/topic sections, add exactly one \"## Summary\" section at the very end. Under it, write one bullet per topic covered (for projects with no \"### \" topics, use the project name itself) in the form \"- {topic or project name} — {status}, {next step}\" — if the excerpt gives no clear next step, write just \"- {topic name} — {status}\".\n\nWithin each project section (or each \"### \" topic inside it), list only that project's **milestones and major changes** as bullets (omit minor progress). Follow these rules strictly:\n\n1. Chronological + dated: within the same \"### \" topic (or the same project, if there's no \"### \"), keep the excerpt's date order (earliest week to latest), and **prefix every bullet with its date in \"MM-DD \" form** (e.g. \"- 07-21 ...\"). Do not reorder beyond splitting into topics. When one bullet covers work spanning several days or weeks, write a range like \"07-21~08-03\".\n2. One bullet = one thing done: **omitting** minor items is not the same as **lumping** several items into one bullet. Never write \"handled various tasks\" or \"kept working on X all month\". Only merge continuous work on the same target (same ticket, same feature), and state the final state (merged, deployed, etc.).\n3. Terse: each bullet is a single short phrase (roughly 10 words or fewer). No background or rationale.\n4. Facts only: state what was done. No evaluative words or intensifiers (\"thoroughly\", \"properly\", \"very\", \"well\"), and do not invent intent, judgment, or connections between items that are not in the excerpt.\n5. Do not scatter ticket/PR numbers through the text; append them in parentheses at the end of the bullet.\n6. No more than 10 bullets per project (drop the least significant ones beyond that).\n\nLines starting with \"> \" (the top-of-month overview, and each \"### \" topic's status line) may state only these four things, whichever have evidence:\n- Nature of the work: whether it was feature development, a bug fix, documentation, a design decision, or investigation\n- Progress state: what is finished, what is in progress, and what is pending a decision\n- Where it got stuck: if the same thing was repeated, or there were failures, retries, or workarounds, note that fact\n- Status: if the excerpt's (weekly summaries') GitHub/Linear activity shows \"PR #N merged\" or \"Issue #N closed\", call it done; if it only shows \"PR #N opened\" with no sign of a merge, call it in progress; if there are only commits or a branch creation, call it underway. **If the excerpt gives no evidence, do not state a status** (never guess). State status only in \"> \" lines, never mix it into bullets. A \"### \" topic's status line should compress to progress state / status only, in roughly 20 characters; the overview line may run 1-2 lines.\n\nThe following are forbidden in \"> \" lines and in the \"## Summary\" section's bullets:\n- No subjective quality judgments like \"handled it well\" or \"was efficient\".\n- No judgment that the excerpt does not support.\n- Bullets (\"- \") must still never mix in interpretation or evaluation — interpretation belongs only in \"> \" lines and the \"## Summary\" section.\n\nGood example:\n> 18 active days · 160 sessions · 620 requests · 40 GitHub events. Company tickets; personal project got summary-feature improvements.\n\n## acme-admin\n### Payment screen\n> Design done, implementation not started\n- 08-04~08-05 Handled the payment failure screen, re-registered cards in payment modal\n\n### Notification settings\n> Done\n- 08-06~08-12 Implemented notification opt-in modal, merged (TICKET-772, PR #88)\n\n## Summary\n- Payment screen — design only, next is implementation\n- Notification settings — done\n\nBad examples:\n- Spent the month cutting new branches for feature work and periodically back-merging main (← no date, lumped, narrative)\n- Chose to do it the right way rather than a stopgap (← evaluative, invented intent)\n> This topic's design is finished and implementation hasn't started yet (← full sentence, not a terse noun phrase for a \"> \" line)\n\nCommon rules:\n- Write every line as a terse noun phrase. Do not write full narrative sentences.\n- Ignore activity outside the \"Target month\" stated on the first line of the excerpt (weeks can straddle month boundaries).\n- Do not guess or embellish anything not present in the excerpt (the weekly summaries).\n- Output nothing besides the structure above (overview, project sections, topic subheadings, status lines, the Summary section).";

/// 재개 브리핑(M7-②) 프롬프트 — 발췌는 `resume::build_resume_excerpt`가 만든, 한 프로젝트의
/// (있으면) 지금 상태(git)/최근 작업/커밋·PR/마지막 세션 끝부분 블록들이다. 일간/주간/월간(v3,
/// `## 한눈에`/`## 자세히`)과는 다른 구조 — "재개"에 필요한 것은 목차형 요약이 아니라 미완결·다음
/// 할 일이라 전용 3섹션(`## 요약`/`## 진행 중·미완결`/`## 다음 할 일`)으로 별도 정의한다.
///
/// "지금 상태"(git) 블록을 최우선 근거로 쓰도록 지시를 추가했다(배경: GitHub 폴러가 5분 간격이라
/// PR을 열자마자 머지하면 이벤트 로그에는 "opened"만 남아, 그 아래 이벤트 로그만 보고 "미완결"로
/// 오판할 수 있다 — 로컬 git 현재 상태가 더 신뢰할 수 있는 근거이므로 이벤트 로그보다 우선하게 한다).
///
/// "## 요약" 지시는 발췌의 "## 최근 작업" 블록(prompt 라인 단순 나열)을 그대로 옮겨 적지 말고
/// **작업 흐름**(무엇을 위해 뭘 했는지)으로 재구성하도록 격상했다(명세 — 재개 브리핑도 흐름 중심).
/// 발췌 구조 자체는 그대로 두고(단일 프로젝트 브리핑이라 프로젝트별 그룹핑은 불필요) 프롬프트
/// 지시만 바꾼다.
pub const PROMPT_RESUME_KO: &str = "당신은 개발자가 이 프로젝트 작업을 이어서 재개할 수 있도록 브리핑을 작성하는 도우미다. 아래는 한 프로젝트의 지금 상태(git), 최근 작업, 최근 커밋·PR, 마지막 세션 끝부분이다. 반드시 한국어로 작성하라.\n\n발췌의 \"지금 상태\"(git)가 현재 사실이다. 그 아래 이벤트 로그(최근 작업·커밋/PR)는 과거 행위일 뿐 — 이미 머지·완료됐을 수 있으니 \"미완결\"로 단정하지 마라. 미완결·다음 할 일은 지금 상태(언커밋 변경·현재 브랜치·최근 커밋)를 우선 근거로 판단하라. 특히 \"지금 상태\"에 \"열린 PR\" 항목이 있으면 그것이 현재 열려 있는 PR의 전부다 — 이벤트 로그에 \"PR opened\"가 있어도 이 목록에 없으면 이미 머지된 것이니 미완결로 넣지 마라.\n\n출력은 정확히 다음 세 섹션으로 구성하라:\n\n## 요약\n\"최근 작업\" 블록의 prompt들을 단순 나열하지 말고, 이 프로젝트에서 최근 **무엇을 위해 어떤 순서로** 작업했는지 흐름으로 2~3줄에 재구성하라.\n\n## 진행 중·미완결\n열린 PR, 끝나지 않은 작업, 마지막 세션에서 중단된 지점을 발췌에 나온 근거로만 짚어라.\n\n## 다음 할 일\n발췌에서 드러나는 다음 단계나 열린 결정을 나열하라. 발췌에 명확한 다음 단계 신호가 없으면 \"명확한 다음 단계 신호 없음\"이라고 써라.\n\n공통 규칙:\n- 발췌에 없는 내용을 추측하거나 부풀리지 마라.\n- 티켓·PR 번호는 문장 끝에 괄호로 근거를 표기하라.\n- 위 세 섹션 외 다른 텍스트(서두·맺음말·설명)를 붙이지 마라.";

/// 영어 로케일용 재개 브리핑 프롬프트.
pub const PROMPT_RESUME_EN: &str = "You are an assistant that writes a briefing so a developer can resume work on this project. Below is a project's current state (git), recent work, recent commits/PRs, and the tail end of its last session. Write in English.\n\nThe excerpt's \"Current state\" (git) reflects present-day fact. The event log below it (recent work, commits/PRs) only records past actions — it may already have been merged or completed, so do not assume it's unfinished. Base your judgment of what's unfinished or what to do next primarily on the current state (uncommitted changes, current branch, recent commits). In particular, if the \"Current state\" includes an \"Open PRs\" item, that is the complete list of currently open PRs — even if the event log shows \"PR opened\", if it's not in this list it has already been merged, so do not list it as unfinished.\n\nStructure the output as exactly three sections:\n\n## Summary\nDo not simply list the prompts from the \"Recent work\" block one by one. Instead, reconstruct in 2-3 lines the **flow** of what this project was recently working toward and in what order.\n\n## In progress / unfinished\nCall out open PRs, unfinished work, and where the last session left off — using only evidence present in the excerpt.\n\n## Next steps\nList the next steps or open decisions that the excerpt reveals. If there's no clear signal of next steps, write \"No clear next-step signal.\"\n\nCommon rules:\n- Do not guess or embellish anything not present in the excerpt.\n- Note ticket/PR numbers in parentheses at the end of the sentence as supporting references.\n- Output nothing besides the three sections above. No preamble or closing remarks.";

/// 앱 로케일에 맞는 재개 브리핑 프롬프트를 고른다(`prompt_for_locale`와 동일한 "ko"만 한국어, 그 외
/// 영어 폴백 규칙).
pub fn resume_prompt(locale: &str) -> &'static str {
    if locale == "ko" {
        PROMPT_RESUME_KO
    } else {
        PROMPT_RESUME_EN
    }
}

/// 기간(`"week"` | `"month"`) + 로케일에 맞는 롤업 프롬프트를 고른다. `period_type`이 그 외 값이면
/// `Err` — 커맨드 계층(`lib.rs`)에서 FE 계약 위반을 조기에 잡아내기 위함이다.
pub fn prompt_for_period(period_type: &str, locale: &str) -> anyhow::Result<&'static str> {
    let is_ko = locale == "ko";
    match (period_type, is_ko) {
        ("week", true) => Ok(PROMPT_WEEK_KO),
        ("week", false) => Ok(PROMPT_WEEK_EN),
        ("month", true) => Ok(PROMPT_MONTH_KO),
        ("month", false) => Ok(PROMPT_MONTH_EN),
        (other, _) => anyhow::bail!("invalid periodType: {other}"),
    }
}

/// "상세 보기"(온디맨드) 프롬프트 — 일간/주간/월간 어느 scope든 이 하나의 프롬프트를 공유한다(파일
/// 상단 doc "상세 보기" 문단 참고). 발췌 구조 설명은 일간 프롬프트(`PROMPT_KO`/`PROMPT_EN`)와
/// 동일하게(프로젝트별 `## ` 섹션, 시각, `└` 결과 줄) 주지만, 출력 지시는 정반대다 — 기본 요약의
/// 명사형·단답형 규칙 대신 **서술문**을 허용해 무엇을·왜·어떻게·지금 상태를 2~4줄로 풀어 쓰게
/// 한다. 출력 계층(`## {프로젝트}` → `### {주제}`)만은 기본 요약과 동일하게 유지해 두 출력이 같은
/// 이름으로 나란히 대응되게 한다. 기본 요약에 이미 있는 `## 정리`(v8) 섹션은 상세에는 넣지 않는다.
pub const PROMPT_DETAIL_KO: &str = "당신은 개발자의 작업 기록을 자세히 풀어 설명하는 도우미다. 아래 발췌는 프로젝트별로 구분되어 있으며(각 프로젝트가 \"## {프로젝트명}\" 섹션), 그 안에 코딩 에이전트 세션(사용자 요청 위주), GitHub 활동, Linear 이슈 활동이 시각(HH:MM)과 함께 시간순으로 나열되어 있다. 각 요청 라인(\"- \"로 시작) 바로 아래에는 \"└\"로 시작하는 하위 줄로 그 요청을 처리한 결과의 요지가 붙어 있을 수 있다. 반드시 한국어로 작성하라.\n\n출력은 발췌의 각 프로젝트 섹션을 그대로 유지해 \"## {프로젝트명}\" 제목으로 작성하라(발췌에 나온 프로젝트명 그대로, 순서도 발췌와 동일하게 유지 — 아래 레포 미지정 규칙으로 새로 만든 섹션만 예외).\n\n발췌의 \"알려진 프로젝트:\" 줄 바로 다음(프로젝트 섹션들 앞)에 \"## 레포 미지정\" 섹션이 있을 수 있다 — 어느 레포에서 한 일인지 자동으로 정해지지 않은 기록(운영 메모·DB 조회·조사 등)이다. 이 섹션의 각 항목은 내용으로 판단해 그날 발췌의 다른 프로젝트 섹션 또는 발췌 상단 \"알려진 프로젝트:\" 줄의 프로젝트 중 맞는 곳의 \"## {프로젝트명}\" 섹션에 넣고, 그 섹션 안에서 시각 순서에 맞게 끼워 넣어라(그날 섹션이 없던 알려진 프로젝트면 그 이름으로 섹션을 새로 만들어 발췌 프로젝트 섹션들 뒤에 둔다). 어느 프로젝트와도 관련이 분명하지 않은 항목은 \"## 기타\" 섹션에 모아라 — 추측으로 억지 배정하지 마라. \"레포 미지정\"이라는 제목은 출력하지 마라. \"## 기타\" 섹션은 맨 마지막에 둔다.\n\n그 프로젝트 안에서 관련된 작업들이 하나의 주제(기능·티켓·작업 대상)로 묶이면 \"### {주제명}\" 소제목으로 나눠라 — 주제가 하나뿐이거나 묶을 게 없으면 \"### \" 없이 서술만 이어써도 된다.\n\n각 프로젝트(또는 그 안의 각 \"### \" 주제) 아래에는 불릿을 쓰지 말고 2~4줄 분량의 서술문으로, 무엇을 했고 왜 했는지, 어떤 과정을 거쳤는지, 지금 어떤 상태인지를 동료에게 설명하듯 풀어서 써라. 발췌에서 같은 일을 여러 번 시도했거나 막혔던 지점, 되돌린 결정이 보이면 그 경위(무엇을 시도했다가 왜 되돌렸는지 등)를 서술에 포함하라. 티켓·PR 번호(TICKET-N, PR #N 등)는 문장 안에 섞지 말고 문장 끝에 괄호로 붙여라.\n\n다음을 반드시 지켜라:\n- 발췌에 없는 내용을 추측하거나 지어내지 마라.\n- 맨 아래에 정리·요약 섹션을 따로 추가하지 마라(기본 요약에 이미 있다).\n- 위 구조(프로젝트 섹션, 주제 소제목, 서술문, 기타 섹션) 외의 텍스트(서두·맺음말·설명)를 붙이지 마라.";

/// 영어 로케일용 "상세 보기" 프롬프트.
pub const PROMPT_DETAIL_EN: &str = "You are an assistant that writes a detailed, narrative account of a developer's work log. The excerpt below is grouped by project (each project is a \"## {project name}\" section), and within each section, coding-agent sessions (user requests), GitHub activity, and Linear issue activity are listed in chronological order with timestamps (HH:MM). Directly below each request line (starting with \"- \") there may be a sub-line starting with \"└\" that gives the gist of how that request was handled. Write in English.\n\nKeep each project section from the excerpt as its own \"## {project name}\" heading (use the project name exactly as it appears in the excerpt, in the same order — the only exception is a section newly created under the Unassigned rule below).\n\nRight after the \"Known projects:\" line (before the project sections), the excerpt may contain an \"## Unassigned\" section — records that could not be automatically tied to a repository (ops notes, DB queries, research, etc.). Judge each item by its content and place it into the matching \"## {project name}\" section — either another project section from that day's excerpt or one of the projects listed on the \"Known projects:\" line near the top — inserted in timestamp order within that section (if it belongs to a known project that had no section that day, create a new section with that name after the excerpt's project sections). Collect items that are not clearly related to any project into an \"## Other\" section — do not force an assignment by guessing. Never output the \"Unassigned\" heading. Place the \"## Other\" section at the very end.\n\nWithin a project, if related work forms a single topic (a feature, ticket, or target), split it out under a \"### {topic name}\" subheading — if there's only one topic or nothing to group, write directly under the project without \"### \".\n\nUnder each project (or each \"### \" topic inside it), do not use bullets — instead write 2-4 lines of narrative prose explaining what was done, why, what process it went through, and what state it's in now, as if explaining to a colleague. If the excerpt shows the same thing attempted repeatedly, a point where work got stuck, or a decision that was reversed, include that story (what was tried and why it was reverted, etc.). Do not scatter ticket/PR numbers (TICKET-N, PR #N) through the sentence; append them in parentheses at the end of the sentence.\n\nFollow these rules strictly:\n- Do not guess or invent anything not present in the excerpt.\n- Do not add a separate summary/recap section at the end (the basic summary already has one).\n- Output nothing besides the structure above (project sections, topic subheadings, narrative prose, the Other section). No preamble or closing remarks.";

/// 앱 로케일에 맞는 "상세 보기" 프롬프트를 고른다(`prompt_for_locale`와 동일한 "ko"만 한국어, 그 외
/// 영어 폴백 규칙).
pub fn detail_prompt_for_locale(locale: &str) -> &'static str {
    if locale == "ko" {
        PROMPT_DETAIL_KO
    } else {
        PROMPT_DETAIL_EN
    }
}

/// 업무평가서(ADR-0017) 프롬프트 버전 — 요약의 [`PROMPT_VERSION`]과 별개로 관리한다(평가서 캐시
/// `performance_reviews.prompt_version`에 기록). 아래 `PROMPT_REVIEW_*`를 의미 있게 바꾸면 올려라.
pub const REVIEW_PROMPT_VERSION: &str = "r1";

// 업무평가서 프롬프트(ADR-0017) — 요약(v5~v9)과 반대로 **해석·판단이 본업**인 산출물이다. 요약의
// "불릿에 평가어 금지"는 요약이 사실 로그이기 때문이고, 평가서는 그 사실 로그(월간/주간 요약)를 입력으로
// 받아 판단을 내린다. 대신 판단을 근거에 묶어 두는 장치를 둔다:
// - 기준: 국내외 평가 제도가 공통으로 쓰는 다섯 축(성과·완결·전문성·협업·성장). 조사 근거는 ADR-0017.
// - "못한 것"은 상황→행동→영향(SBI) 문형으로만. 성격 단정 금지(국내외 자기평가 가이드 공통 경고).
// - 점수·등급 금지, 활동량으로 판단 금지(SPACE·Goodhart — 활동량은 성과가 아니다).
// - 기간 경계를 넘긴 일은 그 자체로 못한 것이 아니다(실측: Linear 이슈 약 30%가 월말을 넘긴다).
// - 기록되지 않은 일(회의·대면 협업)은 모른다고 적는다 — 총평 줄에 근거 범위를 박는다.
// FE는 총평 `> ` 줄 + `## ` 세 섹션을 기존 `SummaryContent` 파서로 그대로 렌더한다.

/// 분기 평가 — 한국어.
pub const PROMPT_REVIEW_QUARTER_KO: &str = r####"당신은 한 사람의 업무 기록을 근거로 자기 업무평가서 초안을 쓰는 도우미다. 대상은 한 분기다. 반드시 한국어로 작성하라.

발췌 구성:
- "# 대상 분기": 평가 기간.
- "## 평가 대상자": 직무·연차 구간·팀원 관리 여부. "입력 없음"이면 직무와 연차를 가정하지 말고 공통 기준으로 평가하라.
- "## 기간 목표": 사용자가 적은 이번 분기 목표. "입력 없음"이면 기록만으로 평가하라.
- "## 근거 범위": 이 평가가 본 기간·프로젝트·활동 요약.
- "## 사실 신호": DB가 직접 센 수치와 미완료·정체 이슈 목록. 수치는 다시 세지 말고 그대로 인용하라.
- "# YYYY-MM 월간 요약": 그 달의 작업 요약. 프로젝트별 "## " 섹션, 주제별 "### " 소제목, 상태를 적은 "> " 줄로 되어 있다.

평가 기준 — 국내외 기업 평가 제도가 공통으로 쓰는 다섯 축:
1. 성과·임팩트: 무엇을 끝냈고 그 결과가 무엇을 바꿨나. 활동량이 아니라 결과를 본다.
2. 실행·완결: 시작한 일을 끝까지 마무리했나.
3. 전문성·문제 해결: 어려운 문제를 어떤 방식으로 풀었나.
4. 협업·소통: 리뷰·이슈 논의·공유처럼 다른 사람과 함께 한 일.
5. 성장·학습: 새로 다룬 영역이나 기술.

기대 수준:
- 연차 구간이 있으면 그 수준에 맞춰 판단하라. 주니어는 맡은 일을 정확히 끝내는가, 미들은 과제를 스스로 설계하고 끝까지 책임지는가, 시니어는 여러 과제·팀에 걸친 영향과 기술 결정·다른 사람의 성장, 리드·매니저는 팀의 성과와 방향·사람의 성장을 본다.
- 팀원을 관리한다면 리더십(방향 제시·팀원 성장·일 분배)도 함께 보라.
- 기간 목표가 있으면 목표 대비로 판단하라. 목표에 없던 성과에는 "(목표 외)"를 붙여라. 기록에서 진척이 보이지 않는 목표는 "못한 것"의 후보다.

출력은 정확히 아래 구조로 쓴다:

> 근거 범위의 기간과 활동 요약을 그대로 옮긴 한 줄. 회의·대면 협업처럼 기록되지 않은 일은 반영되지 않았다는 말을 짧게 덧붙인다.

모든 불릿은 "- **요지** — 설명 [축] (근거)" 형식으로 쓴다. 요지는 20자 안팎의 명사구, 설명은 1~2문장이다. 축은 [성과]·[완결]·[전문성]·[협업]·[성장] 중 하나, 근거 번호(PR·이슈·티켓)는 맨 끝 괄호에 모은다. 근거 번호가 없으면 괄호를 생략한다.

## 잘한 것
- 3~5개. 요지는 무엇을 해냈는지, 설명은 그 결과·영향이다.

## 못한 것
- 2~4개. 요지는 무엇이 부족했는지, 설명은 "어떤 상황에서 → 무엇을 했거나 하지 못했고 → 그 결과 어떤 영향이 있었는지"를 사실만 쓴다. 축은 그 부족이 속한 축이다. 근거가 "사실 신호"의 미완료·정체·취소 항목이면 그 번호를 붙여라.
- 분기 경계를 넘긴 것 자체는 못한 것이 아니다. 오래 멈춘 일, 취소하거나 되돌린 일, 같은 문제의 반복, 목표 대비 진척 없음처럼 근거가 있는 것만 써라. 근거가 없으면 "- 기록상 뚜렷한 개선점 없음" 한 줄만 써라.

## 채우면 좋은 것
- 2~3개. 요지는 다음 분기에 해 볼 구체적 행동, 설명은 왜·어떻게다. 축은 그 행동이 채우는 축이다. 가능하면 "못한 것"의 항목과 연결하라.
- 연차 기대 수준에 비춰 기록에서 잘 보이지 않는 축이 있으면 짚어라. 이때 "하지 않았다"가 아니라 "기록에서 보이지 않는다"로 표현하라.

공통 규칙:
- 발췌에 없는 성과·수치·사실을 지어내지 마라. 모든 판단은 발췌의 근거에 묶여야 한다.
- 요청 수·커밋 수 같은 활동량만으로 잘했다·못했다를 판단하지 마라.
- 점수나 등급(S/A/B, 상/중/하, 10점 만점 등)을 매기지 마라.
- "책임감이 부족하다", "성실하다"처럼 성격·태도를 단정하지 마라. 행동과 결과만 써라.
- 문장은 "~했다"체로 짧게 끊어 써라. 한 불릿은 요지와 설명을 합쳐 세 문장을 넘기지 마라.
- 위 구조 외의 텍스트(서두·맺음말·설명)를 붙이지 마라."####;

/// 분기 평가 — 영어.
pub const PROMPT_REVIEW_QUARTER_EN: &str = r####"You are an assistant that drafts a self performance review from one person's work records. The period is one quarter. Write in English.

The excerpt contains:
- "# Target quarter": the review period.
- "## Person": role, level, and whether they manage people. If it says "not provided", do not assume a role or level — use common criteria.
- "## Goals for the period": goals the person wrote for this quarter. If "not provided", review from the records only.
- "## Evidence scope": the period, projects, and activity counts this review is based on.
- "## Fact signals": numbers counted directly from the database, plus unfinished and stalled issues. Quote these numbers as-is; do not recount.
- "# YYYY-MM monthly summary": that month's work summary, with a "## " section per project, "### " topic headings, and "> " status lines.

Criteria — the five axes shared by performance review systems at companies large and small:
1. Impact: what was finished and what it changed. Judge results, not activity volume.
2. Execution: were started tasks carried through to completion.
3. Expertise and problem solving: how hard problems were solved.
4. Collaboration: work done with others — reviews, issue discussions, sharing.
5. Growth: new areas or technologies taken on.

Expectations:
- If a level is given, judge against it. Junior: finishes assigned work correctly. Mid-level: designs tasks independently and owns them end to end. Senior: influence across tasks and teams, technical decisions, growing others. Lead / Manager: team results and direction, people growth.
- If they manage people, also look at leadership (setting direction, growing teammates, distributing work).
- If goals are given, judge against them. Mark achievements outside the goals with "(beyond goals)". Goals with no visible progress in the records are candidates for "What fell short".

Output exactly this structure:

> One line restating the evidence scope's period and activity counts, briefly adding that unrecorded work such as meetings and in-person collaboration is not reflected.

Write every bullet as "- **Headline** — explanation [Axis] (evidence)". The headline is a noun phrase of about five words; the explanation is one or two sentences. The axis is one of [Impact], [Execution], [Expertise], [Collaboration], [Growth]; supporting numbers (PR, issue, ticket) go in the final parentheses. Omit the parentheses when there are no numbers.

## What went well
- 3-5 bullets. The headline says what was achieved; the explanation gives its result or impact.

## What fell short
- 2-4 bullets. The headline says what fell short; the explanation states facts only, in the order "in what situation → what was or wasn't done → what impact it had". The axis is the one the shortfall belongs to. If the evidence is an unfinished, stalled, or canceled item from "Fact signals", include its number.
- Work that crossed the quarter boundary is not a shortfall by itself. Only include items with evidence: long stalls, canceled or reverted work, repeated problems, no progress against a goal. If there is no such evidence, write the single line "- No clear shortfalls in the records".

## What to add next
- 2-3 bullets. The headline is a concrete action for next quarter; the explanation says why and how. The axis is the one the action strengthens. Link to "What fell short" where possible.
- If an axis expected at their level is barely visible in the records, point it out — phrase it as "not visible in the records", not "did not do".

Common rules:
- Do not invent achievements, numbers, or facts not in the excerpt. Every judgment must be tied to evidence in the excerpt.
- Do not judge good or bad from activity volume (request counts, commit counts) alone.
- Do not assign scores or ratings (S/A/B, 1-10, etc.).
- Do not label personality or attitude ("lacks ownership", "diligent"). Write only behaviors and results.
- Keep sentences short and plain. A bullet, headline and explanation together, stays within three sentences.
- Output nothing besides the structure above. No preamble or closing remarks."####;

/// 월간 점검 — 한국어. 분기 평가와 같은 틀이지만 기간이 짧아 두 번째 칸을 "밀린 것·막힌 것"으로
/// 바꾸고 개수를 줄인다(한 달은 평가하기엔 짧다 — 미완료를 실패로 단정하지 않는다).
pub const PROMPT_REVIEW_MONTH_KO: &str = r####"당신은 한 사람의 업무 기록을 근거로 월간 자기 점검 초안을 쓰는 도우미다. 대상은 한 달이다. 반드시 한국어로 작성하라.

발췌 구성:
- "# 대상 월": 점검 기간.
- "## 평가 대상자": 직무·연차 구간·팀원 관리 여부. "입력 없음"이면 직무와 연차를 가정하지 말고 공통 기준으로 판단하라.
- "## 기간 목표": 사용자가 적은 이번 달 목표. "입력 없음"이면 기록만으로 판단하라.
- "## 근거 범위": 이 점검이 본 기간·프로젝트·활동 요약.
- "## 사실 신호": DB가 직접 센 수치와 미완료·정체 이슈 목록. 수치는 다시 세지 말고 그대로 인용하라.
- "# ... 주간 요약": 그 주의 작업 요약. 프로젝트별 "## " 섹션, 주제별 "### " 소제목, 상태를 적은 "> " 줄로 되어 있다. 대상 월 밖의 날짜는 무시하라.

판단 기준 — 성과·임팩트, 실행·완결, 전문성·문제 해결, 협업·소통, 성장·학습. 활동량이 아니라 결과를 본다. 연차 구간이 있으면 그 수준(주니어: 맡은 일 완수 / 미들: 스스로 설계하고 책임 / 시니어: 여러 과제·팀에 걸친 영향 / 리드·매니저: 팀의 성과와 사람의 성장)에 맞춰 판단하고, 기간 목표가 있으면 목표 대비로 보라.

출력은 정확히 아래 구조로 쓴다:

> 근거 범위의 기간과 활동 요약을 그대로 옮긴 한 줄. 기록되지 않은 일은 반영되지 않았다는 말을 짧게 덧붙인다.

모든 불릿은 "- **요지** — 설명 [축] (근거)" 형식으로 쓴다. 요지는 20자 안팎의 명사구, 설명은 1~2문장이다. 축은 [성과]·[완결]·[전문성]·[협업]·[성장] 중 하나, 근거 번호는 맨 끝 괄호에 모으고 없으면 생략한다.

## 잘한 것
- 2~4개. 요지는 무엇을 해냈는지, 설명은 그 결과다.

## 밀린 것·막힌 것
- 1~3개. 요지는 무엇이 밀렸거나 막혔는지, 설명은 "어떤 상황에서 → 무엇이 밀렸거나 막혔고 → 그 영향"을 사실만 쓴다. 다음 달로 넘어가는 일은 "이월"로 적되 실패로 쓰지 마라. 근거가 "사실 신호"의 항목이면 번호를 붙여라.
- 근거가 없으면 "- 기록상 특별히 밀리거나 막힌 일 없음" 한 줄만 써라.

## 채우면 좋은 것
- 1~2개. 요지는 다음 달에 해 볼 구체적 행동, 설명은 왜·어떻게다. 가능하면 위 칸의 항목과 연결하라. 기록에서 보이지 않는 것은 "하지 않았다"가 아니라 "기록에서 보이지 않는다"로 표현하라.

공통 규칙:
- 발췌에 없는 성과·수치·사실을 지어내지 마라. 모든 판단은 발췌의 근거에 묶여야 한다.
- 요청 수·커밋 수 같은 활동량만으로 판단하지 마라. 점수나 등급을 매기지 마라.
- 성격·태도를 단정하지 마라. 행동과 결과만 써라.
- 문장은 "~했다"체로 짧게 끊어 써라. 한 불릿은 요지와 설명을 합쳐 세 문장을 넘기지 마라.
- 위 구조 외의 텍스트(서두·맺음말·설명)를 붙이지 마라."####;

/// 월간 점검 — 영어.
pub const PROMPT_REVIEW_MONTH_EN: &str = r####"You are an assistant that drafts a monthly self check-in from one person's work records. The period is one month. Write in English.

The excerpt contains:
- "# Target month": the check-in period.
- "## Person": role, level, and whether they manage people. If it says "not provided", do not assume a role or level — use common criteria.
- "## Goals for the period": goals the person wrote for this month. If "not provided", judge from the records only.
- "## Evidence scope": the period, projects, and activity counts this check-in is based on.
- "## Fact signals": numbers counted directly from the database, plus unfinished and stalled issues. Quote these numbers as-is; do not recount.
- "# Week of ...": that week's work summary, with a "## " section per project, "### " topic headings, and "> " status lines. Ignore dates outside the target month.

Criteria — impact, execution, expertise and problem solving, collaboration, growth. Judge results, not activity volume. If a level is given, judge against it (Junior: finishes assigned work / Mid-level: designs and owns tasks / Senior: influence across tasks and teams / Lead or Manager: team results and people growth), and if goals are given, judge against them.

Output exactly this structure:

> One line restating the evidence scope's period and activity counts, briefly adding that unrecorded work is not reflected.

Write every bullet as "- **Headline** — explanation [Axis] (evidence)". The headline is a noun phrase of about five words; the explanation is one or two sentences. The axis is one of [Impact], [Execution], [Expertise], [Collaboration], [Growth]; supporting numbers go in the final parentheses, omitted when there are none.

## What went well
- 2-4 bullets. The headline says what was achieved; the explanation gives its result.

## Slipped or blocked
- 1-3 bullets. The headline says what slipped or got blocked; the explanation states facts only: "in what situation → what slipped or got blocked → its impact". Work carried into next month is a "carry-over", not a failure. If the evidence is an item from "Fact signals", include its number.
- If there is no such evidence, write the single line "- Nothing notably slipped or blocked in the records".

## What to add next
- 1-2 bullets. The headline is a concrete action for next month; the explanation says why and how. Link to the section above where possible. Phrase gaps as "not visible in the records", not "did not do".

Common rules:
- Do not invent achievements, numbers, or facts not in the excerpt. Every judgment must be tied to evidence in the excerpt.
- Do not judge from activity volume alone. Do not assign scores or ratings.
- Do not label personality or attitude. Write only behaviors and results.
- Keep sentences short and plain. A bullet, headline and explanation together, stays within three sentences.
- Output nothing besides the structure above. No preamble or closing remarks."####;

/// 평가서 종류(`"quarter"` | `"month"`) + 로케일에 맞는 프롬프트를 고른다("ko"만 한국어, 그 외 영어
/// 폴백 — [`prompt_for_locale`]와 같은 규칙). 그 외 종류는 에러.
pub fn review_prompt(period_type: &str, locale: &str) -> anyhow::Result<&'static str> {
    let is_ko = locale == "ko";
    match (period_type, is_ko) {
        ("quarter", true) => Ok(PROMPT_REVIEW_QUARTER_KO),
        ("quarter", false) => Ok(PROMPT_REVIEW_QUARTER_EN),
        ("month", true) => Ok(PROMPT_REVIEW_MONTH_KO),
        ("month", false) => Ok(PROMPT_REVIEW_MONTH_EN),
        (other, _) => anyhow::bail!("invalid review periodType: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_for_locale_ko_returns_korean_prompt() {
        assert_eq!(prompt_for_locale("ko"), PROMPT_KO);
    }

    #[test]
    fn prompt_for_locale_en_returns_english_prompt() {
        assert_eq!(prompt_for_locale("en"), PROMPT_EN);
    }

    #[test]
    fn prompt_for_locale_unknown_falls_back_to_english() {
        assert_eq!(prompt_for_locale("fr"), PROMPT_EN);
    }

    #[test]
    fn prompt_for_period_selects_week_and_month_prompts_by_locale() {
        assert_eq!(prompt_for_period("week", "ko").unwrap(), PROMPT_WEEK_KO);
        assert_eq!(prompt_for_period("week", "en").unwrap(), PROMPT_WEEK_EN);
        assert_eq!(prompt_for_period("month", "ko").unwrap(), PROMPT_MONTH_KO);
        assert_eq!(prompt_for_period("month", "en").unwrap(), PROMPT_MONTH_EN);
    }

    #[test]
    fn prompt_for_period_unknown_locale_falls_back_to_english() {
        assert_eq!(prompt_for_period("week", "fr").unwrap(), PROMPT_WEEK_EN);
        assert_eq!(prompt_for_period("month", "fr").unwrap(), PROMPT_MONTH_EN);
    }

    #[test]
    fn prompt_for_period_rejects_invalid_period_type() {
        assert!(prompt_for_period("day", "ko").is_err());
        assert!(prompt_for_period("", "ko").is_err());
    }

    #[test]
    fn resume_prompt_ko_returns_korean_prompt() {
        assert_eq!(resume_prompt("ko"), PROMPT_RESUME_KO);
    }

    #[test]
    fn resume_prompt_en_returns_english_prompt() {
        assert_eq!(resume_prompt("en"), PROMPT_RESUME_EN);
    }

    #[test]
    fn resume_prompt_unknown_locale_falls_back_to_english() {
        assert_eq!(resume_prompt("fr"), PROMPT_RESUME_EN);
    }

    #[test]
    fn review_prompt_selects_by_period_type_and_locale() {
        assert_eq!(review_prompt("quarter", "ko").unwrap(), PROMPT_REVIEW_QUARTER_KO);
        assert_eq!(review_prompt("quarter", "en").unwrap(), PROMPT_REVIEW_QUARTER_EN);
        assert_eq!(review_prompt("month", "ko").unwrap(), PROMPT_REVIEW_MONTH_KO);
        assert_eq!(review_prompt("month", "fr").unwrap(), PROMPT_REVIEW_MONTH_EN);
        assert!(review_prompt("week", "ko").is_err());
    }

    #[test]
    fn review_prompts_forbid_scores_and_require_evidence_sections() {
        // 설계 결정(ADR-0017)이 프롬프트에서 빠지면 조용히 회귀한다 — 핵심 문구가 남아 있는지 고정한다.
        for prompt in [PROMPT_REVIEW_QUARTER_KO, PROMPT_REVIEW_MONTH_KO] {
            assert!(prompt.contains("## 잘한 것"));
            assert!(prompt.contains("## 채우면 좋은 것"));
            assert!(prompt.contains("등급"));
            assert!(prompt.contains("지어내지 마라"));
        }
        assert!(PROMPT_REVIEW_QUARTER_KO.contains("## 못한 것"));
        // FE 보고서 카드가 "**요지** — 설명 [축] (근거)"를 파싱한다(ReviewReport.tsx) — 형식 지시가 빠지면 카드가 평문으로 떨어진다.
        for prompt in [PROMPT_REVIEW_QUARTER_KO, PROMPT_REVIEW_MONTH_KO] {
            assert!(prompt.contains("\"- **요지** — 설명 [축] (근거)\""));
        }
        for prompt in [PROMPT_REVIEW_QUARTER_EN, PROMPT_REVIEW_MONTH_EN] {
            assert!(prompt.contains("\"- **Headline** — explanation [Axis] (evidence)\""));
        }
        assert!(PROMPT_REVIEW_MONTH_KO.contains("## 밀린 것·막힌 것"));
        for prompt in [PROMPT_REVIEW_QUARTER_EN, PROMPT_REVIEW_MONTH_EN] {
            assert!(prompt.contains("## What went well"));
            assert!(prompt.contains("## What to add next"));
            assert!(prompt.contains("ratings"));
        }
    }

    #[test]
    fn daily_and_detail_prompts_route_unassigned_section() {
        // v10: 발췌의 레포 미지정 섹션을 프로젝트/기타로 옮기라는 규칙이 일간·상세 4종 모두에 있어야 한다.
        for prompt in [PROMPT_KO, PROMPT_DETAIL_KO] {
            assert!(prompt.contains("## 레포 미지정") && prompt.contains("알려진 프로젝트:"));
            assert!(prompt.contains("## 기타"));
        }
        for prompt in [PROMPT_EN, PROMPT_DETAIL_EN] {
            assert!(prompt.contains("## Unassigned") && prompt.contains("Known projects:"));
            assert!(prompt.contains("## Other"));
        }
    }

    #[test]
    fn detail_prompt_for_locale_ko_returns_korean_prompt() {
        assert_eq!(detail_prompt_for_locale("ko"), PROMPT_DETAIL_KO);
    }

    #[test]
    fn detail_prompt_for_locale_en_returns_english_prompt() {
        assert_eq!(detail_prompt_for_locale("en"), PROMPT_DETAIL_EN);
    }

    #[test]
    fn detail_prompt_for_locale_unknown_falls_back_to_english() {
        assert_eq!(detail_prompt_for_locale("fr"), PROMPT_DETAIL_EN);
    }
}
