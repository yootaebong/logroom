# 08 · Connectors

SaaS 커넥터(Slack·Notion·Linear·Figma·Jira 등)는 `docs/00-product.md` 스코프의 "나중" 항목이자
**Pro 구독의 근거**(06-roadmap "수익화 상세" — 커넥터는 외부 API 변경으로 지속 유지보수가
필요해 반복 매출을 정당화한다)다. 이 문서는 커넥터 공통 원칙과 커넥터별 v1 스펙(Slack·GitHub·Linear·Notion)을 정의한다.
아키텍처 결정 근거는 `docs/07-decisions.md` ADR-0015 참고.

## 커넥터 공통 원칙

1. **사용자 기기에서 직접 API 호출** — 커넥터는 로컬 앱(Rust) 프로세스가 SaaS의 공개 API를 직접
   호출한다. **서버 경유는 0**(ADR-0001 "로컬 온리"의 연장). 인바운드 웹훅/Events API처럼 인터넷에
   노출된 서버가 필요한 방식은 채택하지 않는다(ADR-0015).
2. **토큰은 로컬 저장** — 커넥터 API 토큰은 `~/.logroom/config.json`(기존 캡처 설정과 동일 파일,
   perm 0600)에 저장한다. `04-privacy-security.md` "저장 & 키 관리" 표의 "SaaS API 토큰(Pro) → OS
   키체인만" 원칙은 **장기 목표**이며, v1(Slack)은 기존 config.json 저장 관례를 그대로 따른다(다른
   캡처 설정과 같은 파일·같은 권한 모델이라 구현/감사 경로가 하나로 유지된다). 키체인 이관은 커넥터가
   늘어나는 시점에 일괄 검토한다.
3. **기본 off, 사용자가 명시적으로 연결** — 커넥터는 기본 비활성(`enabled: false`). 사용자가 토큰을
   입력하고 켜야만 해당 서비스로 아웃바운드 요청이 발생한다(`04-privacy-security.md` 네트워크 정책
   "SaaS 커넥터" 행 — 기본값 off).
4. **파이프라인 공유** — 커넥터가 가져온 데이터도 기존 캡처와 동일한 정규화 규칙을 통과한다: 소스별
   정규화 함수 → **시크릿 스크럽**(scrub.rs) → **캡처 일시정지** 체크(paused면 저장 스킵, 진행 상태는
   전진) → `db::ingest()`. 캡처 일시정지·시크릿 스크럽은 소스를 가리지 않는 전역 정책이다.
5. **레이트리밋 존중** — 각 서비스가 명시한 재시도 신호(예: `Retry-After` 헤더)를 반드시 지키고,
   실패는 다음 폴링 주기로 미룬다(조용한 재시도 원칙, 03-capture.md 공통 규칙과 동일 정신).
6. **Pro 게이팅은 M6** — 무료/Pro 경계 구현(라이선스 검증 연동)은 `06-roadmap.md` M6에서 일괄
   처리한다. 그 전까지 커넥터는 기능은 동작하되 설정 화면에 "Pro 예정" 표시만 한다(게이팅 없음).

## Slack v1

### 스코프

- **내가 보낸 메시지만** 캡처한다(채널 전체 히스토리 수집 아님 — "회고 중심 저장" 철학과 일치,
  ADR-0012). Slack Web API `search.messages`를 `query = "from:<@내 user id> after:<날짜>"`(전방
  증분) 또는 `query = "from:<@내 user id> before:<날짜>"`(후방 백필, 아래 "이중 커서" 참고)로 호출한다.
  - `<@USER_ID>` 형태(멘션 문법)가 검색 쿼리에서 특정 사용자로 필터링하는 검증된 방식이다. 자기
    자신의 `user_id`는 부팅 시 1회 `auth.test`로 확인한다(아래 "인증" 참고).
  - `after:`/`before:<날짜>`는 일 단위(day) 해상도라, 실제 컷은 앱이 커서(newest/backfill 각각의
    msg ts)로 한 번 더 필터링한다 — Slack 쪽 날짜 필터는 API 호출량을 줄이는 용도, 정확한 dedup
    경계는 `external_id` UNIQUE 제약과 ts 비교가 담당한다.
- **폴링 주기**: 기본 5분, 설정 가능(`pollMinutes`). 실시간 Events API는 채택하지 않는다(ADR-0015).
- **이중 커서(최신 우선 백필)**: 설정 직후 최근 데이터부터 보이도록, 과거 히스토리 백필은
  asc(과거→현재)가 아니라 **desc(현재→과거)**로 진행한다. 기존 `capture_cursors` 테이블을
  재사용하되(새 스키마/테이블 추가 없음) `source = "slack"` 아래 두 `resource`를 둔다:
  - `resource = "search:newest"`: 전방 증분 — 지금까지 본 **최신** msg ts(epoch ms)를 `offset`에
    저장한다. 매 폴링 사이클 `after:<newest 날짜>`로 새 메시지를 전부 가져와 전진시킨다(순서보다
    누락 없음이 중요해 asc로 페이지네이션).
  - `resource = "search:backfill"`: 후방 백필 — 지금까지 내려간 **가장 오래된** msg ts(epoch ms)를
    `offset`에 저장한다. 사이클마다 `before:<backfill 날짜>` + `sort_dir=desc`로 한 배치(페이지
    상한만큼)를 가져와 배치의 최솟값으로 전진시킨다. **배치 결과가 0건이면 바닥(더 이상 과거
    메시지 없음)에 도달했다는 뜻**이라 완료 마킹한다 — `mtime` 컬럼을 "백필 완료" 플래그로
    재사용한다(`NULL`=진행 중, 값이 있으면 완료. 재시작해도 다시 돌지 않는다).
  - **첫 실행**(두 커서 모두 없음): 상한 없이 desc 최신 배치 1회를 가져와 배치의 최댓값을
    `search:newest`로, 최솟값을 `search:backfill`로 동시에 초기화한다.
  - **따라잡기 가속**(아래 "Rate limit 전략")은 backfill이 미완료인 동안 계속 적용되고, 완료된
    뒤에는 `pollMinutes` 간격의 전방 증분만 남는다.
  - **기존 사용자 마이그레이션**: v1 출시 초기의 구 단일 커서(`resource = "search"`, asc로 진행하던
    위치)는 이중 커서 도입 후에도 남아있을 수 있다. 이 값은 과거 어느 중간 지점이지 "최신"이
    아니므로 승계하지 않는다 — 폴러가 이 커서를 발견하면 즉시 삭제하고 **첫 실행과 동일하게** desc
    최신 배치부터 다시 시작한다. 이미 저장돼 있던 과거 메시지들과 backfill이 내려가며 만나는 구간은
    `(source, external_id)` UNIQUE 제약으로 자연히 중복 스킵되어 메꿔지므로 데이터 유실은 없다.
  - Slack `before:`/`after:`는 day(일) 단위 해상도라 자정 경계에서 값이 애매해질 수 있다 — 경계
    중복은 idempotent라 무해하지만 **누락**은 없어야 하므로 `after`는 커서 날짜의 하루 **전**,
    `before`는 커서 날짜의 하루 **뒤**로 여유를 둔다.

### 인증 — 수동 user token(xoxp)

- v1은 **OAuth 설치 플로우 없이 수동 토큰 입력**이다(ADR-0015 "OAuth v2 보류").
- 사용자가 직접 발급: `api.slack.com/apps` → **Create App**(From scratch) → **OAuth & Permissions** →
  **User Token Scopes**에 `search:read`, `users:read` 추가 → **Install to Workspace** → `xoxp-`로
  시작하는 **User OAuth Token** 복사 → LogRoom 설정 다이얼로그 "커넥터" 섹션에 붙여넣기.
  - `users:read`는 DM 스트림 title에 상대방 이름을 표시하기 위해 필요하다(아래 "매핑" 참고). **이미
    `search:read`만으로 앱을 설치해 사용 중이었다면** OAuth & Permissions에서 `users:read`를 추가한
    뒤 **Reinstall to Workspace**로 재설치하고, 새로 발급된 토큰을 LogRoom 설정에 다시 붙여넣어야
    한다(스코프 추가는 재설치 없이는 반영되지 않음).
- 봇 토큰(`xoxb-`)은 `search.messages`를 호출할 수 없다(Slack API 제약 — 검색은 user token 전용).
- 앱 시작 시 토큰으로 `POST auth.test`를 호출해 유효성 확인 + `user_id`/`team`(워크스페이스명) 획득.
  실패(무효 토큰, 스코프 누락 등)하면 폴러는 시작하지 않고 에러를 로그로 남긴다 — 캡처 헬스에도
  반영된다(아래 "헬스" 참고). `users:read`가 없어도 폴러 자체는 정상 동작한다 — DM 상대방 이름
  해석(`users.info`)만 실패해 title이 "DM: `<user id>`"로 표시되는 정도로 저하된다(아래 "매핑" 참고).

### 매핑

| LogRoom 필드 | 값 |
|---|---|
| `stream.source` | `"slack"` |
| `stream.id` | `"slack:" + <channel_id>` |
| `stream.kind` | `"session"` |
| `stream.title` | `"#" + <channel.name>`(채널) 또는 `"DM"`(1:1 다이렉트 메시지, 채널명이 없는 경우) |
| `stream.project` | 워크스페이스명(`auth.test` 응답의 `team`) |
| `event.type` | `"message"` |
| `event.externalId` | `<channel_id>:<msg ts>`(Slack ts 원본 문자열, 예: `"1730000000.123456"`) |
| `event.ts` | Slack ts(초.마이크로초 문자열) → epoch ms |
| `event.body` | 메시지 텍스트(`text` 필드) |
| `event.url` | `permalink`(있으면) |

`search.messages` 한 번의 호출 결과는 여러 채널의 메시지를 섞어서 반환할 수 있다 — 앱은 응답을
`channel.id` 기준으로 그룹핑해 **채널(스트림)당 1건의 인제스트 요청**으로 나눠 처리한다(기존
`db::ingest()`가 요청 1건당 하나의 `stream` upsert를 전제하는 것과 동일한 계약을 지키기 위함,
`capture/normalize.rs`/`capture/kiro.rs`와 같은 패턴).

### Rate limit 전략

- `search.messages`는 Slack Tier 2 메서드(분당 호출 수 제한)다. 페이지네이션(`page`/`paging.pages`)
  으로 여러 페이지를 순회할 때도, 전방 증분과 후방 백필이 각각 별도 호출인 것도 이 제한을 공유한다.
- 한 번의 호출은 페이지 상한(10페이지 = 최대 1,000건)까지만 가져온다. 백필 대상 이력이 많으면 한
  사이클에 다 못 내려가고 여러 사이클에 나눠 진행된다.
- **429 응답**: `Retry-After` 헤더(초 단위)만큼 대기 후 같은 페이지를 재시도한다.
- **연속 429**(백오프): 재시도 대기시간을 지수적으로 늘리되 **최대 5분**을 상한으로 둔다. 상한에
  도달한 뒤에도 실패하면 이번 폴링 주기는 포기하고 다음 주기에 커서 위치부터 재시도한다(데이터
  유실 없음 — cursor가 전진하지 않았으므로).
- **그 외 에러**(네트워크 오류, 인증 오류, JSON 파싱 실패 등): 로그만 남기고 다음 주기 재시도.
- **백필 가속**: `search:backfill`이 미완료인 동안은 평소 간격(`pollMinutes`) 대신 짧은 간격(10초)
  으로 다음 사이클을 즉시 돌려 밀린 과거 이력을 빠르게 따라잡는다. 백필이 완료되면(바닥 도달) 이
  가속은 멈추고 평소 간격의 전방 증분만 남는다. 가속 중에도 위 페이지 상한·429 백오프는 동일하게
  적용돼 레이트리밋을 존중한다.

### 헬스 판정 (파일감시와 다른 기준)

Slack은 감시할 로컬 파일/디렉토리("root")가 없는 **폴러형** 소스라 `capture/health.rs`의 기존
"root의 최신 파일 mtime vs 마지막 캡처 시각" 비교(03-capture.md "캡처 헬스")를 그대로 쓸 수 없다.
대신:

- **inactive**: `enabled=false` 이거나 토큰 미설정.
- **stale**: 활성화+토큰 있음에도 (1) 한 번도 폴링에 성공한 적 없거나(예: `auth.test` 실패로 폴러가
  아예 시작 못 함), (2) 마지막 성공 시각이 `pollMinutes`의 3배(최소 10분) 이상 지남 — 폴러가 죽었거나
  네트워크/인증 오류가 지속되는 상태로 간주.
- **ok**: 그 외(최근 폴링 주기 내 성공).

"마지막 성공 시각"은 새 메시지가 없어도 폴링 사이클이 정상 완료될 때마다 `search:newest` 커서의
`updated_at`을 갱신해 얻는다(전방 증분은 매 사이클 수행되므로 — 메시지가 없다고 실패는 아니므로
offset 값은 그대로 두되 갱신 시각만 찍는다) — 별도 공유 상태 없이 기존 cursor 테이블 하나로
"마지막 성공 폴링 시각"까지 표현한다.

## GitHub v1

### 스코프

- **다중 계정** 지원(Slack v1과의 핵심 차이) — 계정마다 독립된 PAT·커서·폴러 태스크를 갖는다. 한
  계정의 토큰이 무효화돼도 다른 계정은 영향받지 않는다(아래 "인증" 참고).
- **두 API를 함께 쓰는 이중 파이프라인**:
  - **Events(전방 증분)**: `GET /users/{username}/events` — GitHub이 보관하는 최근 활동(최대 약
    90일/300건, GitHub 자체 제약)을 `pollMinutes` 간격으로 가져온다. PushEvent(커밋별로 분리),
    PullRequestEvent(opened/closed/merged/reopened), IssuesEvent(opened/closed),
    PullRequestReviewEvent·PullRequestReviewCommentEvent·IssueCommentEvent(코멘트/리뷰),
    CreateEvent(branch/tag 생성)만 매핑하고 그 외 타입(ForkEvent, WatchEvent 등)은 skip한다(아래
    "매핑" 참고).
  - **Search 백필(후방)**: `GET /search/issues` + `GET /search/commits` — Events API가 보관하지 않는
    과거 이력 중 **커밋·PR·이슈 생성만** 메꾼다. **코멘트·리뷰는 Search API로 알 수 없어 백필 불가**
    (Events가 보관하는 최근 약 90일치만 확보 가능 — 알려진 한계).
- **로컬 프로젝트 매칭은 후속** — `stream.project`는 일단 repo full name(`owner/repo`) 그대로 쓰고,
  로컬 git 체크아웃 경로와의 자동 매칭은 이 문서 갱신 없이 별도 착수한다.

### 인증 — 다중 계정 PAT

- v1은 Slack과 동일하게 **수동 발급 PAT**다(ADR-0015 "OAuth v2 보류"의 근거를 그대로 따름). 사용자가
  `github.com/settings` → **Developer settings** → **Fine-grained personal access tokens** → 새
  토큰 생성 시 대상 저장소에 `Contents`/`Issues`/`Pull requests` **Read-only** 스코프를 부여한다.
- 계정을 추가하면 폴러가 부팅 시(그리고 실패 후 재시도 시) `GET /user`로 토큰을 검증하고 로그인명
  (`login`)을 확인한다. 실패하면(무효 토큰, 만료 등) **그 계정만** 폴러를 시작하지 않고 로그를
  남긴다 — 다른 계정은 정상 동작한다. 검증에 성공하면 확인된 `username`을 설정 파일에 저장해 FE
  계정 목록에 표시한다.
- 봇 토큰 개념이 없는 개인 PAT라 Slack의 "user token 전용" 제약은 해당 없음.

### 매핑

| LogRoom 필드 | 값 |
|---|---|
| `stream.source` | `"github"` |
| `stream.id` | `"github:" + <owner/repo>` |
| `stream.kind` | `"session"` |
| `stream.title` / `stream.project` | `<owner/repo>`(repo full name 그대로 — 로컬 프로젝트 매칭은 후속) |
| `event.type` | `"message"` |

| 이벤트 | 조건 | title | externalId |
|---|---|---|---|
| PushEvent | 커밋마다 1건 | 커밋 메시지 1줄(`body`는 전문) | `gh:<sha>` |
| PullRequestEvent | action ∈ {opened, closed, reopened} | `PR #N <opened\|closed\|merged\|reopened>: <제목>`(closed+merged면 `merged`) | `gh:<event id>` |
| IssuesEvent | action ∈ {opened, closed} | `Issue #N <opened\|closed>: <제목>` | `gh:<event id>` |
| PullRequestReviewEvent / PullRequestReviewCommentEvent / IssueCommentEvent | — | `코멘트/리뷰: <PR·이슈 제목>`(`body`는 코멘트/리뷰 본문) | `gh:<event id>` |
| CreateEvent | ref_type ∈ {branch, tag} | `<branch\|tag> 생성: <ref>` | `gh:<event id>` |
| search/issues(백필) | PR 또는 이슈 **생성**만 | `<PR\|Issue> #N 생성: <제목>` | `gh:<repo>#<번호>:created` |
| search/commits(백필) | — | 커밋 메시지 1줄(`body`는 전문) | `gh:<sha>`(Events의 PushEvent 커밋과 동일 이름공간 — 자연 dedup) |

`event.url`은 각 항목의 `html_url`(커밋은 `https://github.com/<repo>/commit/<sha>`로 조립)을 쓴다.
Events API 응답 1건에 여러 repo가 섞여 있을 수 있어 Slack과 동일하게 **repo(스트림)당 1건**으로
그룹핑해 인제스트한다.

### Rate limit 전략

- **Events API**는 일반 코어 rate limit(시간당 5,000회, 인증 시)을 쓴다 — `pollMinutes`(기본 5분)
  간격이면 여유가 충분하다.
- **Search API는 별도로 훨씬 낮은 rate limit(분당 30회)** 을 적용한다. 요청 사이 최소 2초 간격을
  두고, `429`(Too Many Requests) 또는 `Retry-After` 헤더를 동반한 `403`(secondary rate limit)은
  Slack과 동일한 지수 백오프(최대 5분 상한)로 재시도한다. `Retry-After` 없는 `403`은 권한/스코프
  문제로 보아 즉시 실패시키고 다음 폴링 주기에 재시도한다.
- **기간 슬라이싱**: Search API는 쿼리당 최대 1,000건만 반환하므로, 3개월 단위로 최신→과거 기간을
  슬라이스하며 내려간다(경계 하루는 의도적으로 겹치게 해 누락을 막는다 — 중복은 idempotent).
  연속 3회 빈 슬라이스(이슈+커밋 검색 모두 0건)를 만나면 바닥(계정 생성 즈음)에 도달했다고 보아
  백필을 완료 처리한다.
- **백필 가속**: 백필이 미완료인 동안은 평소 간격 대신 10초 간격으로 다음 사이클을 즉시 돌려
  밀린 과거 이력을 빠르게 따라잡는다(Slack과 동일 정책).

### 헬스 판정

Slack과 동일한 폴러형 판정 기준(`capture/health.rs::classify_slack` 재사용)을 쓰되, **다중 계정**
특성을 반영해 `roots` 필드는 설정된 계정 수(0..N)를 담는다. `last_captured_at`은 모든 계정 커서의
`updated_at` 중 최댓값이라, 계정 일부만 토큰이 무효화돼도 다른 계정이 살아있으면 전체 상태는
stale로 잡히지 않는다 — 개별 계정 실패는 로그로만 확인 가능하다(향후 계정별 헬스 노출은 검토 대상).

### 한계

- **백필은 커밋·PR·이슈 생성만** — 코멘트·리뷰는 Events API가 보관하는 최근 약 90일치만 확보되고,
  그 이전 이력은 백필되지 않는다(Search API로는 코멘트/리뷰를 조회할 방법이 없음).
- **PR/이슈 생성의 드문 중복 가능성** — Events의 `externalId`(`gh:<event id>`)와 Search 백필의
  `externalId`(`gh:<repo>#<번호>:created`)가 서로 다른 이름공간이라, 커넥터를 켠 직후의 아주 좁은
  시간창에 막 생성된 PR/이슈는 두 경로 모두에서 발견돼 1건 중복될 수 있다(커밋은 두 경로 모두
  `gh:<sha>`를 써서 자연히 dedup된다).
- **다중 계정**, **rate limit 분리(Events vs Search)** 는 위 절 참고.

## Linear v1

### 스코프

- **내가 생성한 이슈** + **내가 작성한 코멘트**만 캡처한다(Slack "내가 보낸 메시지만"과 동일한
  "회고 중심 저장" 철학, ADR-0012). **이슈 상태 변경(status/assignee 등) 히스토리는 v1 스코프
  밖**이다(후속 — 아래 "한계" 참고).
- **다중 워크스페이스** 지원(GitHub v1과 동일한 다중 계정 패턴) — 워크스페이스마다 독립된 Personal
  API Key·커서·폴러 태스크를 갖는다. 한 워크스페이스의 키가 무효화돼도 다른 워크스페이스는 영향받지
  않는다.
- Linear API는 **GraphQL 단일 엔드포인트**(`https://api.linear.app/graphql`)만 제공한다 — REST
  페이지네이션(`page`/`per_page`) 대신 GraphQL 자체의 커서 페이지네이션(`after`/
  `pageInfo.hasNextPage`/`pageInfo.endCursor`)을 쓴다. 쿼리당 결과 상한이 없어(GitHub Search API의
  1,000건 제한과 달리) 기간 슬라이싱 없이 `after` 커서로 임의 깊이까지 페이지네이션된다.
- **이중 커서(전방 증분 + 후방 백필)** — `issues`/`comments` 쿼리 각각에 독립적으로 적용한다(Slack
  이중 커서와 동일 원리). 커서 값 자체는 GitHub Events 커서와 동일하게 `updatedAt`(epoch ms)으로
  둔다 — `capture_cursors.offset`이 `i64`라 GraphQL의 불투명 `endCursor` 문자열을 그대로 영속 저장할
  수 없어, 한 폴링 사이클 **안에서** 여러 페이지를 순회할 때만 GraphQL 네이티브 `after` 커서를 쓰고
  사이클 **사이**의 진행 상태는 `updatedAt` 경계값으로 표현한다.
  - **전방 증분**(`capture_cursors` resource `issues:<viewerId>:newest` / `comments:<viewerId>:newest`):
    `updatedAt: { gt: <cursor ISO> }` 필터로 매 사이클 새 항목을 전부 가져와 배치 최댓값으로
    전진시킨다. 이슈가 "생성"이 아니라 단순 업데이트(상태 변경 등)로 다시 걸려도 `externalId`가
    같아 재수집은 idempotent하다((source, external_id) UNIQUE 제약 자연 dedup) — v1이 상태변경을
    수집하지 않는 근거이기도 하다.
  - **후방 백필**(resource `issues:<viewerId>:backfill` / `comments:<viewerId>:backfill`, `mtime`을
    "완료" 플래그로 재사용 — Slack과 동일 패턴): `updatedAt: { lt: <floor ISO> }` 필터 + 사이클당
    페이지 상한(10페이지 = 최대 500건)으로 한 배치씩 내려가고, 배치가 비면 바닥에 도달했다고 보아
    완료 마킹한다.
  - **첫 실행**(두 커서 모두 없음): `updatedAt` 필터 없이 배치 1회로 newest=배치 최댓값·backfill=배치
    최솟값을 동시에 초기화한다(Slack 부트스트랩과 동일).
  - **따라잡기 가속**: issues/comments 중 하나라도 백필 미완료면 평소 간격 대신 10초 간격으로 다음
    사이클을 즉시 돈다(GitHub/Slack과 동일 정책).

### 인증 — 워크스페이스 배열 + Personal API Key

- v1은 Slack/GitHub과 동일하게 **수동 발급 Personal API Key**다(ADR-0015 "OAuth v2 보류"). 사용자가
  `linear.app` → **Settings** → **Security & access** → **Personal API keys** → **Create key**로
  발급한 키를 LogRoom 설정 다이얼로그 "커넥터" 섹션에 붙여넣는다.
- 워크스페이스(계정)를 추가하면 폴러가 부팅 시(그리고 실패 후 재시도 시) `query { viewer { id name
  email } }`로 키를 검증하고 `id`(이후 필터링 키)/`name`(표시명)을 확인한다. 실패하면(무효 키 등)
  **그 워크스페이스만** 폴러를 시작하지 않고 로그를 남긴다 — 다른 워크스페이스는 정상 동작한다.
  검증에 성공하면 확인된 `viewerId`/`viewerName`을 설정 파일에 저장해 FE 워크스페이스 목록에
  표시한다.
- **인증 헤더는 `Authorization: <key>`**(Bearer 아님) — OAuth access token과 달리 Personal API Key는
  접두어 없이 그대로 헤더 값에 넣는다.

### 매핑

| LogRoom 필드 | 값 |
|---|---|
| `stream.source` | `"linear"` |
| `stream.id` | `"linear:" + <issue identifier>`(**이슈=스트림**, 예 `linear:TICKET-838`) |
| `stream.kind` | `"session"` |
| `stream.title` | `<identifier> <이슈 제목>`(제목이 비면 identifier만) |
| `stream.project` | 팀명(`team.name`) — 사이드바 프로젝트 필터의 단위 |
| `event.type` | `"message"` |

> 처음엔 Slack 채널/GitHub repo와 같은 결로 **팀=스트림**이었으나, 팀은 이슈 수백 개를 담는
> 컨테이너라 하루치 활동이 한 덩어리로 뭉쳐 "오늘 어떤 이슈를 작업했나"가 안 보였다(실사용 피드백).
> 이슈=스트림/팀=project로 재매핑 — Claude(project=레포, stream=세션) 구조와 동일한 결. 이 때문에
> 코멘트 GraphQL 쿼리도 이슈 `title`을 함께 가져온다(남의 이슈에 코멘트만 단 경우 스트림 제목을
> 코멘트 쪽에서 채워야 함).

| 이벤트 | title | body | externalId | ts |
|---|---|---|---|---|
| 이슈 생성 | `<identifier> 생성: <제목>` | description 앞부분(2,000자 절단 — bodyPolicy와 무관한
    고정 정책) | `ln:<이슈 uuid>` | `createdAt` |
| 코멘트 작성 | `<identifier> 코멘트` | 코멘트 원문 | `ln:<코멘트 uuid>` | `createdAt` |

`event.url`은 이슈 url(`issue.url`) 또는 코멘트 url(코멘트 자체 url이 있으면 우선, 없으면 이슈
url로 폴백)을 쓴다.

### Rate limit 전략

- Linear Personal API Key는 **시간당 1,500회** 제한이다. `429` 또는 `Retry-After` 헤더는 GitHub/
  Slack과 동일한 지수 백오프(최대 5분 상한)로 재시도한다.
- 페이지 요청 사이 최소 1초 간격을 둬 레이트리밋을 존중한다.

### 헬스 판정

GitHub와 동일한 폴러형+다중 계정 판정 기준(`capture/health.rs::classify_slack` 재사용)을 쓰되,
`roots` 필드는 설정된 워크스페이스 수(0..N)를 담는다. `last_captured_at`은 모든 워크스페이스 커서의
`updated_at` 중 최댓값이라, 일부만 키가 무효화돼도 다른 워크스페이스가 살아있으면 전체 상태는
stale로 잡히지 않는다.

### 한계

- **상태 변경 히스토리 미수집** — 이슈 생성/코멘트 작성만 캡처한다. 상태(status)·담당자(assignee)
  변경 등은 v1 스코프 밖이다(후속 검토 대상).
- **GraphQL 스키마 실측 미검증** — 이 커넥터의 쿼리/필터 shape(`IssueFilter`/`CommentFilter`의
  `creator`/`user` 서브필드 등)는 Linear 공개 문서 근거로만 작성됐고 실제 API 응답으로 아직
  검증되지 않았다(수동 확인 필요).
- **다중 워크스페이스**, **rate limit** 은 위 절 참고.

## Notion v1

### 왜 앞의 셋과 아키텍처가 다른가

로드맵은 Notion을 "검색 API 기반 폴링, Slack과 유사한 아키텍처"로 예상했으나 **실제 API 제약이 셋과
다르다**. 아래 세 가지가 이 커넥터의 설계를 전부 결정한다.

1. **`POST /v1/search`에 시간 범위 필터가 없다.** 지원하는 `filter`는 `object`(page/data_source)와
   `in_trash` 둘뿐이고, 정렬만 `last_edited_time` asc/desc로 된다. Slack의 `after:`/`before:`,
   Linear의 `updatedAt: { gt/lt }`에 해당하는 게 없다 → **이중 커서(전방 증분 + 후방 백필)를 쓸 수
   없고, 쓸 필요도 없다**(아래 "커서" 참고).
2. **편집 이력 API가 없다.** Notion은 페이지 버전 히스토리를 공개 API로 주지 않는다. 주는 것은
   `last_edited_time` **스냅샷 한 개**뿐이고 재편집하면 덮어쓰인다 → **커넥터를 켜기 전의 과거 편집
   이력은 원천적으로 확보할 수 없다**(Slack·GitHub·Linear와 다른 근본 한계, 아래 "한계"). 뒤집으면
   이것이 이 커넥터의 존재 이유다 — 폴링으로 스냅샷을 쌓으면 **Notion 자신도 API로 제공하지 않는
   편집 타임라인을 LogRoom이 만든다.**
3. **토큰 종류에 따라 "나"를 알 수 있는지가 갈린다.** Internal integration 토큰으로 `GET /v1/users/me`를
   부르면 bot이 온다(`owner: { type: "workspace" }`) — Linear의 `viewer { id }`, GitHub의 `login`에
   해당하는 값이 없어 페이지의 `last_edited_by.id`와 비교할 대상이 없다. 반면 **2026-05-12에 도입된
   Personal Access Token(PAT)** 은 `users/me`가 토큰을 만든 **사람**을 돌려준다. 회사 워크스페이스는
   남의 편집이 대부분이라 "내가 편집한 것만" 거를 수 없으면 쓸 수 없다 — **PAT를 기본 방식으로 둔다**
   (아래 "인증").

### 스코프

- **PAT: 내가 마지막으로 편집한 페이지만 캡처한다.** 대상은 내가 볼 수 있는 페이지 전체이고(페이지별
  연결 불필요), `last_edited_by.id`가 `users/me`의 `id`와 같은 것만 남긴다 — Slack("내가 보낸
  메시지만")·Linear("내가 생성한 이슈/코멘트")와 같은 원칙이다. `last_edited_by`가 응답에 없는 항목은
  버린다(남의 편집을 내 것으로 기록하는 쪽보다 하나 놓치는 쪽이 낫다).
- **Internal integration: 연결된 페이지의 모든 편집을 캡처한다.** bot이라 "나"를 알 수 없어 작성자
  필터가 없다(위 ③). Notion은 **페이지마다 integration을 수동으로 연결**해야 하므로 *사용자가 연결한
  범위 = 사용자가 고른 범위*다. 개인 워크스페이스라면 전부 본인 편집이라 차이가 없다. 읽기 전용
  권한(Read content)으로 좁힐 수 있다는 점 때문에 남겨 둔다 — PAT는 읽기 전용 옵션이 없다.
- **page 객체만** 받는다(`filter: { property: "object", value: "page" }`). 데이터베이스 안의 아이템도
  page 객체(`parent.data_source_id`)라 DB 콘텐츠는 그대로 들어온다 — 별도로 `data_source` 객체 자체의
  편집은 v1 스코프 밖이다. 휴지통은 제외한다(`in_trash` 기본 false).
- **본문(블록)은 저장하지 않는다.** `search` 응답에는 페이지 속성만 있고 본문은
  `GET /v1/blocks/{id}/children`로 따로 긁어야 한다 — 페이지당 호출 1회가 추가되고 문서 전문이 로컬
  DB에 들어간다. "무슨 문서를 만졌나"는 제목으로 충분하며 요약 엔진 재료로도 그렇다.
- **댓글은 v1 스코프 밖.** 워크스페이스 전역 "내 댓글" 조회 API가 없다(`GET /v1/comments`는
  `block_id`별 조회뿐)라 페이지 수만큼 호출이 늘어난다.
- **다중 계정** 지원(GitHub·Linear와 동일한 다중 계정 패턴) — 개인·회사 워크스페이스를 함께 쓴다.
  계정마다 독립된 토큰·커서·폴러 태스크를 갖는다. PAT는 워크스페이스 하나에 묶이므로 워크스페이스마다
  토큰을 하나씩 추가한다.
- **API 버전은 `Notion-Version: 2026-03-11` 고정**(2026-09 기준 최신). 직전 버전 `2025-09-03`(`filter.value`가
  `database` → `data_source`로 바뀐 버전)과의 차이는 `archived` → `in_trash` 등 이 커넥터가 읽지 않는
  필드뿐이다. 버전을 고정해야 응답 shape이 흔들리지 않는다.

### 커서 — 단일 커서 + 조기 중단 (셋 중 가장 단순)

시간 필터가 없으니 "건너뛰기"가 불가능하다. 대신 **desc 정렬이 보장하는 순서**를 이용해 위에서부터
읽다가 이미 본 지점을 만나면 멈춘다. `capture_cursors`는 `source = "notion"`, `resource =
"search:<계정 id>"` **하나**만 쓴다(Slack·Linear의 `:newest`/`:backfill` 쌍 없음, `mtime`도 쓰지
않음 — "백필 완료" 개념 자체가 없다). 계정 id는 로컬에서 부여한 값이다(아래 "인증" — PAT는 워크스페이스
id를 주지 않는다).

- `offset` = 지금까지 **훑은** 최대 `last_edited_time`(epoch ms). **PAT 편집자 필터로 버린 항목도
  포함한다** — 남긴 것만으로 계산하면 커서가 "내 마지막 편집"에 묶여, 남이 편집을 많이 하는 회사
  워크스페이스에서는 매 사이클 그 지점까지 수천 건을 다시 훑는다.
- **증분 사이클**: `sort: { timestamp: "last_edited_time", direction: "descending" }`로 페이지를
  순회하며, `last_edited_time`이 **cursor보다 작은** 첫 항목을 만나면 그 항목까지 처리하고 중단한다
  (desc 정렬이라 그 아래는 전부 이미 본 것). 평상시에는 1페이지만 읽는다. 경계의 중복은
  `(source, external_id)` UNIQUE 제약으로 자연히 dedup된다.
  - **`<= cursor`가 아니라 `< cursor`인 것이 중요하다.** Notion의 `last_edited_time`은 해상도가
    거칠어(초/분 단위로 절단돼 내려온다) 서로 다른 페이지가 같은 값을 갖는 일이 흔하다. 커서를 T로
    올린 뒤 같은 T 구간 안에서 다른 페이지가 편집되면, `<=`로 자를 경우 desc 배치의 첫 T 항목에서
    멈춰 **그 뒤에 이어지는 같은 T 항목들을 영영 놓친다**(다음 사이클에도 커서가 T라 똑같이 잘린다).
    `<`로 두면 T 구간 전체를 매번 다시 훑고 이미 저장된 것은 조용히 dedup된다 — "경계 중복은
    무해하지만 누락은 복구 불가"라는 Slack v1의 판단과 같다.
- **첫 실행**(커서 없음): `has_more`가 끝날 때까지 전량 1회 스캔한다. 안전장치로 절대 상한
  (`MAX_BOOTSTRAP_PAGES`)을 두고, 상한에 걸리면 경고 로그를 남기고 그 배치의 최댓값을 커서로 저장한다
  (그보다 오래된 페이지는 확보하지 못한다 — 아래 "한계").
- **폴링 성공 표시**: 새 편집이 없어도 사이클이 정상 완료되면 커서의 `updated_at`만 갱신한다(Slack과
  동일 — 헬스 판정의 "마지막 성공 폴링 시각"을 별도 상태 없이 cursor 하나로 표현). 이 때문에 **연결된
  페이지가 아직 0개인 워크스페이스에는 `offset = 0`인 커서 row가 생긴다** — 구현은 `offset == 0`을
  "커서 없음"과 동일하게 취급해야 한다. 그러지 않으면 뒤늦게 페이지를 연결했을 때 첫 실행 경로를
  타지 못해 안전 상한 없이 전량을 긁는다.
- **따라잡기 가속 없음** — 백필 개념이 없으므로 항상 `pollMinutes` 간격이다.

### 인증 — 계정 배열 + 토큰 두 종류(자동 판별)

Slack/GitHub/Linear와 동일하게 **수동 발급 토큰**이다(ADR-0015 "OAuth v2 보류"). 사용자는 토큰을
붙여넣기만 하고, 종류는 폴러가 `GET /v1/users/me`의 `type`으로 가른다(`person` = PAT, `bot` =
integration). 설정 화면의 계정 목록이 판별 결과("내 편집만" / "연결한 페이지의 모든 편집")를 보여 준다.

| | **Personal Access Token (기본)** | Internal Integration Secret |
|---|---|---|
| 발급 | `notion.so/developers/tokens` → **New token** → 권한 **Notion API** → 만료 선택 | `notion.so/profile/integrations` → **New integration**(Internal) → **Read content** |
| 발급 가능한 사람 | Free: 소유자 · Plus: 전원 · Business: 소유자(관리자가 전원으로 확장 가능) · Enterprise: 소유자+지정 그룹(관리자 설정). 게스트는 불가 | **워크스페이스 소유자만** |
| 보이는 범위 | 내가 볼 수 있는 페이지 전체 — 연결 불필요 | 페이지마다 연결을 붙인 것만 |
| `users/me` | 토큰을 만든 사람 → 편집자 필터 가능 | bot → 편집자 필터 불가 |
| 워크스페이스 정보 | **없다** | `bot.workspace_id`/`workspace_name` |
| 만료 | 7·30·90·180일 또는 1년(기본 1년) | 없음 |
| 권한 | 읽기·쓰기(읽기 전용 옵션 없음) | 읽기 전용으로 좁힐 수 있다 |

- **회사 워크스페이스의 일반 구성원이 쓸 수 있는 방식은 PAT뿐이다.** integration은 소유자만 만들 수
  있다. Business·Enterprise에서 토큰 메뉴가 안 보이면 관리자가 막아 둔 것이다(설정 → 연결에서 허용).
  그마저 막혀 있으면 public connection(OAuth)만 남는데, Notion OAuth는 PKCE가 없어 `client_secret`을
  서버가 쥐어야 한다 — 로컬 퍼스트 원칙과 맞지 않아 보류한다(ADR-0015).
- **계정 식별은 로컬 id다.** PAT는 워크스페이스 정보를 주지 않고, 같은 사람이 개인·회사 워크스페이스에
  PAT를 하나씩 만들면 `users/me`가 **같은 사람**을 돌려준다 — API 응답만으로는 두 계정을 구분할 수
  없다. 그래서 계정을 저장할 때 로컬 id(UUID v7)를 부여해 커서 키와 설정 왕복 매칭 키로 쓴다.
- **PAT가 만료되면 계정을 지우고 새 토큰으로 다시 추가한다.** 설정 화면에 토큰 교체 UI가 없어서 새
  계정(새 id)이 되고 첫 실행 경로(전량 1회 스캔)를 다시 탄다 — 이미 저장된 편집은 UNIQUE 제약으로
  걸러져 중복은 생기지 않고, 만료 기간 동안의 편집도 마지막 편집자가 나인 것은 이때 들어온다. 저장
  로직(`resolve_notion_accounts_update`)은 같은 id에 새 토큰이 오면 id를 유지하도록 돼 있어, 교체 UI를
  붙이면 커서가 이어진다(후속).
- **계정 이름(`label`)을 사용자가 붙인다.** 목록 표시와 타임라인 `project`에 쓰인다. PAT는 워크스페이스
  이름을 알 수 없어서 이름을 안 붙이면 계정들이 모두 `"Notion"`으로 뭉친다 — 설정 화면이 이 이유를
  안내한다.
- **integration은 토큰 입력만으로는 아무것도 보이지 않는다** — 캡처하려는 페이지(또는 그 상위 —
  하위는 상속된다)에서 `⋯` → **Connections** → integration을 추가해야 한다. **이 단계를 빠뜨리면
  `search` 응답이 빈 배열이고 오류도 나지 않는다.** 설정 화면이 이 안내를 항상 노출한다.
- 계정을 추가하면 폴러가 부팅 시 `users/me`로 토큰을 검증하고 종류(`kind`), PAT면 사람 이름
  (`userName`), integration이면 `workspaceId`/`workspaceName`을 설정 파일에 저장해 FE 목록에 표시한다.
  실패하면(무효·만료 토큰 등) **그 계정만** 폴러를 시작하지 않고 로그를 남긴다 — 다른 계정은 정상
  동작한다. PAT인데 `users/me`에 `id`가 없으면 편집자 필터를 걸 수 없으므로 역시 건너뛴다(필터 없이
  돌리면 회사 워크스페이스의 남의 편집이 전부 들어온다).
- 인증 헤더는 `Authorization: Bearer <token>`이다(Linear의 접두어 없는 헤더와 다르다).

### 매핑

| LogRoom 필드 | 값 |
|---|---|
| `stream.source` | `"notion"` |
| `stream.id` | `"notion:" + <page id>`(**페이지=스트림**) |
| `stream.kind` | `"session"` |
| `stream.title` | 페이지 제목(비면 `"제목 없음"`) |
| `stream.project` | 계정 이름(`label`) → integration의 `bot.workspace_name` → `"Notion"` |
| `event.type` | `"message"` |

> Linear에서 팀=스트림으로 뒀다가 하루치 활동이 한 덩어리로 뭉쳐 "오늘 어떤 이슈를 작업했나"가 안 보여
> 이슈=스트림으로 재매핑한 전례가 있다. Notion도 같은 이유로 **페이지=스트림**이다. `project`를 상위
> 부모 체인에서 뽑는 방식은 페이지마다 추가 호출이 붙어 v1에서는 채택하지 않는다(계정 이름 하나로
> 두고, 활동이 한 프로젝트로 뭉쳐 불편하면 DB 제목 기준 세분화를 후속 검토). page id는 전역 UUID라
> 계정이 여럿이어도 `externalId`가 겹치지 않는다.

| 이벤트 | 조건 | title | externalId | ts |
|---|---|---|---|---|
| 페이지 생성 | `created_time == last_edited_time` | `<제목> 생성` | `nt:<page id>:<last_edited_time>` | `last_edited_time` |
| 페이지 편집 | 그 외 | `<제목> 편집` | `nt:<page id>:<last_edited_time>` | `last_edited_time` |

- **`externalId`에 시각을 넣는 것이 이 커넥터의 핵심 결정이다.** `nt:<page id>`로 고정하면 UNIQUE
  제약에 걸려 **최초 1건 이후 같은 페이지의 편집이 영영 기록되지 않는다** — 위 "왜 다른가" ②의 가치가
  통째로 사라진다. 시각을 포함시키면 며칠에 걸쳐 고친 문서가 날짜별로 남고, 같은 스냅샷을 여러 사이클에
  걸쳐 다시 봐도 idempotent하다.
- `event.body`는 페이지 제목(본문 미수집 — 위 "스코프"). `event.url`은 page object의 `url`.
- **페이지 제목 추출**: `properties`에서 `type == "title"`인 속성의 `title[].plain_text`를 이어붙인다.
  속성이 없거나 비면 `"제목 없음"`으로 폴백한다(조용히 skip하지 않는다 — 제목 없는 페이지도 편집
  활동이다).

### Rate limit 전략

- 연결(토큰) 단위 **60초 창 180회**(Business·Enterprise는 600회)이고, 워크스페이스의 모든 연결이
  나눠 쓰는 워크스페이스 단위 한도가 따로 있다(2026-09 기준, `Retry-After`는 60초를 넘지 않는다).
  페이지 요청 사이 최소 [`PAGE_REQUEST_INTERVAL_MS`](400ms, 분당 150회) 간격을 둔다.
- **429·529**: `Retry-After` 헤더(초)만큼 대기 후 같은 요청을 재시도하고, 연속 실패는 지수 백오프로
  늘리되 **최대 5분** 상한(Slack/GitHub/Linear와 동일 정책). 상한에 도달한 뒤에도 실패하면 이번 주기는
  포기하고 다음 주기에 커서 위치부터 재시도한다(커서가 전진하지 않았으므로 유실 없음).
- **401**(무효 토큰 — PAT는 만료일이 있다)은 그 계정 폴러를 중단시키고 로그를 남긴다. **403**은 권한/블록 한도 문제라
  즉시 실패시키고 다음 주기에 재시도한다.
- 그 외 에러(네트워크, JSON 파싱 실패 등)는 로그만 남기고 다음 주기 재시도.

### 헬스 판정

GitHub·Linear와 동일한 폴러형 판정 기준(`capture/health.rs::classify_slack` 재사용)을 쓰되, `roots`
필드는 설정된 계정 수(0..N)를 담는다. `last_captured_at`은 모든 계정 커서의 `updated_at` 중
최댓값이다 — **계정 하나의 PAT가 만료돼도 다른 계정이 살아 있으면 전체 헬스는 `ok`로 남는다**
(GitHub·Linear와 같은 알려진 한계. 만료는 로그로만 보인다).

> **주의**: integration은 헬스가 `ok`라도 **연결된 페이지가 0개면 캡처는 0건**이다(위 "인증"의
> Connections 단계 누락). 폴링 자체는 성공하므로 헬스로는 구분되지 않는다 — 설정 화면 가이드 문구가
> 유일한 방어선이다.

### 한계

- **과거 편집 이력을 확보할 수 없다.** 커넥터를 켠 시점의 "각 페이지가 마지막으로 편집된 시각" 1건씩만
  들어오고, 그 이전 편집은 Notion이 API로 제공하지 않는다(위 "왜 다른가" ②). Slack·GitHub·Linear가
  모두 후방 백필로 과거를 확보하는 것과 다르다.
- **PAT의 편집자 필터는 마지막 편집자만 본다.** 내가 고친 뒤 다음 폴링 전에 동료가 같은 페이지를
  고치면 `last_edited_by`가 동료로 바뀌어 내 편집은 남지 않는다. Notion이 편집자 목록을 주지 않아
  피할 방법이 없다 — 폴링 주기를 줄이면 확률만 낮아진다.
- **integration은 작성자 필터 없음** — 팀 워크스페이스에서는 남의 편집도 들어온다(위 "왜 다른가" ③).
- **PAT 만료** — 만료되면 401로 그 계정 폴러가 멈춘다. 새 토큰을 추가해야 한다(위 "헬스 판정").
- **본문·댓글 미수집** — 위 "스코프".
- **integration은 연결 누락이 조용히 실패한다** — 위 "헬스 판정" 주의.
- **첫 실행 절대 상한 초과 시 일부 페이지 누락** — 위 "커서".
- **"그날 내 메시지" 패널에는 나오지 않는다** — 그 패널의 쿼리(`query.rs`)는
  `source IN ('slack','github','linear')`로 발화형 소스만 모은다. 페이지 편집은 발화가 아니라
  의도적으로 제외했다(타임라인·다이제스트에는 정상 표시된다). FE의 `MESSAGE_SOURCES`에는 포함되는데,
  그쪽은 "발화냐"가 아니라 "세션형 렌더 규칙을 쓰느냐"를 가르는 목록이라 기준이 다르다 —
  Notion 스트림의 `started_at~ended_at`은 지속 작업이 아니라 첫/마지막 편집 시각이다.

## 로드맵 (Slack·GitHub·Linear·Notion 다음)

`docs/00-product.md` 스코프 기준, 우선순위는 도그푸드 피드백에 따라 조정될 수 있다.

| 순서 | 커넥터 | 메모 |
|---|---|---|
| 1 | **Slack** | 이 문서, v1 완료 |
| 2 | **GitHub** | 이 문서, v1 완료(Events+Search 백필, 다중 계정) |
| 3 | **Linear** | 이 문서, v1 완료(GraphQL 이중 커서, 다중 워크스페이스) |
| 4 | **Notion** | 이 문서, v1 완료(단일 커서 + 조기 중단, 다중 계정, PAT 편집자 필터). 예상과 달리 Slack형 이중 커서가 아니다 |
| 5 | Figma | 코멘트/버전 히스토리 |
| 6 | Jira | 티켓 활동(Linear와 유사한 형태) |

각 커넥터는 착수 시 이 문서에 소스별 절(스코프/인증/매핑/rate limit)을 Slack·GitHub·Linear와 같은
형식으로 추가한다.
