/** 사람이 받는 DMG의 **고정 주소**. 버전 경로(`/0.13.0/…`)를 여기 박으면 릴리스마다 이 파일을
 * 같이 고쳐야 하고, 안 고치면 버튼이 조용히 구버전을 내려준다. `scripts/publish.sh`가 릴리스
 * 마지막 단계에서 이 별칭을 최신 DMG로 갱신한다. */
export const DOWNLOAD_URL = "https://releases.logroom.app/latest/LogRoom.dmg";
