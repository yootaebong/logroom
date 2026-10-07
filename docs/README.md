# LogRoom — 기획/설계 문서

> 프라이버시 우선, **로컬 온리** 활동 기록기. AI 시대의 병렬 작업을 자동 캡처해
> "지금 각 스트림이 뭘 하는지 / 오늘 내가 뭘 했는지"를 한눈에.

이 폴더는 **실행 전 플래닝의 단일 진실 소스(SSOT)** 다. 코드 작성은 이 문서를 기준으로만 한다.
문서와 코드가 어긋나면 문서를 먼저 고치고(PR) 구현한다.

## 색인

| # | 문서 | 내용 |
|---|------|------|
| 00 | [product](./00-product.md) | 비전 · 문제정의 · 차별점 · 스코프 · 용어 · 성공지표 |
| 01 | [architecture](./01-architecture.md) | 기술스택+근거 · 모노레포 · 프로세스/데이터 흐름 |
| 02 | [data-model](./02-data-model.md) | Event/Stream 스키마 · FTS5 · 마이그레이션 · 스트림/레인 알고리즘 |
| 03 | [capture](./03-capture.md) | 인제스트 API 계약 · 소스별 어댑터(검증된 포맷) |
| 04 | [privacy-security](./04-privacy-security.md) | 프라이버시 원칙 · 네트워크 정책 · 저장/키 · 위협모델 |
| 05 | [ui-ux](./05-ui-ux.md) | 화면 · 컴포넌트 · 상호작용 · 상태 |
| 06 | [roadmap](./06-roadmap.md) | 마일스톤 M0~M7 · 완료기준(AC) · 수익화(deferred) |
| 07 | [decisions](./07-decisions.md) | 핵심 기술결정 ADR |
| 08 | [connectors](./08-connectors.md) | 커넥터 원칙 · Slack·GitHub·Linear·Notion v1 스펙 · 로드맵(Figma·Jira) |

## 상태

- 단계: **M4 완료** (M0→M1→M2(CLI)→M4, PR #1~#18). 실사용 캡처 동작 중(Claude Code 다중 config dir + Kiro CLI), 도그푸드 진행.
- 화면: 다이제스트(기본)·타임라인·요청 중심 상세 + ⌘K 검색·키보드 네비. 프라이버시: 시크릿 스크럽(기본 on)·캡처 일시정지·캡처 헬스.
- 이름/도메인: **LogRoom / logroom.app** (구매 완료)
- 저장소: `github.com/yootaebong/logroom` (MIT 오픈소스, ADR-0018)
- 확정: 스택(Tauri v2) · 라이선스(MIT 오픈소스(ADR-0018), 수익화는 별도 재결정) · macOS 우선 · 회고 중심 저장(ADR-0012)
- 다음/재개: `06-roadmap.md`의 **"현재 위치"** 참고 — **M5 Product Hardening**(온보딩·패키징·공증·랜딩) 또는 도그푸드 피드백

## 읽는 순서

처음이면 `00 → 01 → 02 → 03` 순서로. 구현 착수 시엔 `06-roadmap`의 현재 마일스톤 → 관련 세부 문서.
