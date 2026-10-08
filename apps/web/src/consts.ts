/** 사람이 받는 DMG의 **고정 주소**. 버전 경로(`/0.13.0/…`)를 여기 박으면 릴리스마다 이 파일을
 * 같이 고쳐야 하고, 안 고치면 버튼이 조용히 구버전을 내려준다. `scripts/publish.sh`가 릴리스
 * 마지막 단계에서 이 별칭을 최신 DMG로 갱신한다. */
export const DOWNLOAD_URL = "https://releases.logroom.app/latest/LogRoom.dmg";

/** 소스 저장소. 히어로·오픈소스 섹션·푸터·JSON-LD(sameAs)가 함께 쓴다. */
export const GITHUB_URL = "https://github.com/yootaebong/logroom";
export const GITHUB_ISSUES_URL = `${GITHUB_URL}/issues`;
export const GITHUB_LICENSE_URL = `${GITHUB_URL}/blob/main/LICENSE`;
export const GITHUB_CONTRIBUTING_URL = `${GITHUB_URL}/blob/main/CONTRIBUTING.md`;

/** Homebrew 설치 명령. cask 의 postflight 가 quarantine 속성을 지워 첫 실행 경고가 뜨지 않는다. */
export const BREW_COMMAND = "brew install --cask yootaebong/tap/logroom";

/** 루트 README 의 "Build from source" 와 같은 명령. README 를 바꾸면 여기도 같이 바꾼다. */
export const BUILD_FROM_SOURCE_COMMANDS = [
  "pnpm install",
  "pnpm --filter @logroom/core build",
  "pnpm --filter @logroom/desktop tauri build",
] as const;

/** 이슈를 열기 어려운 사람을 위한 보조 연락처. */
export const FEEDBACK_MAILTO = "mailto:feedback@logroom.app?subject=LogRoom";
