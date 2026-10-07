-- LogRoom V5: git worktree cwd에서 캡처된 스트림의 `project`가 worktree 경로
-- (`<본레포>/.claude/worktrees/<name>`)로 저장된 기존 오염 데이터를 본 레포 루트로 소급 정리.
-- 신규 캡처는 src/capture/normalize.rs::normalize_project가 `.git` 파일(worktree 마커)의
-- `gitdir: <path>` 내용을 파싱해 이미 본 레포 루트로 정규화한다(이 마이그레이션은 그 도입 전
-- 데이터만 정리). 실측상 오염 project는 전부 `.claude/worktrees/` 세그먼트를 포함하는 단일
-- 패턴이라 SQL 문자열 연산(instr/substr)만으로 충분하다. events 테이블엔 project 컬럼이 없어
-- streams만 대상이다. V1~V4는 refinery 규칙상 수정 금지 — 신규 마이그레이션만 추가한다.

UPDATE streams
SET project = substr(project, 1, instr(project, '/.claude/worktrees/') - 1)
WHERE project LIKE '%/.claude/worktrees/%';
