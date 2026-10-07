import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";

/** 피드백 수신 주소 — CF Email Routing으로 실 메일함에 포워딩(도메인 이메일, 서버 불필요). */
const FEEDBACK_EMAIL = "feedback@logroom.app";

/**
 * 기본 메일 앱으로 피드백 작성 창을 연다(mailto). **아웃바운드 0 유지가 핵심** — 앱이 직접 서버로
 * 보내지 않고 OS 메일 클라이언트를 여는 것뿐이라, "앱은 어떤 신호도 외부로 보내지 않는다"는
 * 프라이버시 약속(docs/04, 랜딩 카피)을 지킨다. 버전·OS를 본문에 프리필하되 사용자가 **보고**
 * 전송하므로 몰래 수집하는 텔레메트리와 다르다(사용자가 지울 수도 있음).
 *
 * @param subject 로케일별 제목(호출부가 t()로 넘김).
 * @param bodyHint 로케일별 본문 안내(작성 위치 안내 문구).
 */
export async function openFeedbackMail(subject: string, bodyHint: string): Promise<void> {
  let version = "";
  try {
    version = await getVersion();
  } catch {
    // 버전 조회 실패는 무시 — 프리필 정보일 뿐 피드백 자체는 보낼 수 있어야 한다.
  }
  const platform = typeof navigator !== "undefined" ? navigator.platform : "";
  const meta = [version && `v${version}`, platform].filter(Boolean).join(" · ");
  const fullSubject = version ? `${subject} (v${version})` : subject;
  const body = `${bodyHint}\n\n\n---\n${meta}`;
  const url = `mailto:${FEEDBACK_EMAIL}?subject=${encodeURIComponent(fullSubject)}&body=${encodeURIComponent(body)}`;
  await openUrl(url);
}
