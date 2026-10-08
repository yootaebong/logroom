// Astro i18n copy table — official "useTranslations" recipe pattern.
// https://docs.astro.build/en/recipes/i18n/
//
// Every UI string lives here so components stay presentation-only. Keys are
// grouped by component/section. Add a key to every locale below — TypeScript
// will flag any locale missing a key that another locale defines.

export const languages = {
  en: "English",
  ko: "한국어",
} as const;

export const defaultLang: keyof typeof languages = "en";

export const ui = {
  en: {
    "site.title": "LogRoom — Your work, recorded on your Mac. Summarized by your own AI.",
    "site.description":
      "Claude Code and Kiro sessions, GitHub, Slack, Linear and Notion on one timeline, with daily, weekly and monthly summaries. Local-only, open source under MIT, no telemetry.",
    "meta.keywords":
      "work log, activity tracker, AI session recorder, local-first, privacy, open source, developer tools, Claude Code, Kiro, Notion, daily summary",
    "meta.ogImageAlt":
      "LogRoom — your work, remembered and organized, 100% locally, with a parallel-stream timeline of four colored lanes.",

    "site.name": "LogRoom",

    "hero.brand": "LogRoom",
    "hero.title.line1": "Your work, recorded on your Mac.",
    "hero.title.highlight": "Summarized by your own AI.",
    "hero.subtitle":
      "Claude Code and Kiro sessions, GitHub, Slack, Linear and Notion on one timeline. Daily, weekly and monthly summaries, resume briefings and quarterly reviews. Open source under MIT, no telemetry.",
    "hero.download": "Download for macOS",
    "hero.github": "View source on GitHub",
    "hero.brew.label": "Or install with Homebrew",
    "hero.brew.copy": "Copy",
    "hero.brew.copied": "Copied",
    "hero.brew.copyAriaLabel": "Copy the Homebrew install command",
    "hero.requirements": "macOS · Apple Silicon",
    "hero.badge": "Open source · MIT · 100% local",

    "sourceStrip.works": "Works with: Claude Code · Kiro · GitHub · Slack · Linear · Notion",

    "mockup.heading": "See LogRoom in action",
    "mockup.tablist.ariaLabel": "Product screenshot",
    "mockup.tab.resume": "Resume briefing",
    "mockup.tab.digest": "Digest",
    "mockup.tab.timeline": "Timeline",
    "mockup.tab.summary": "Summary",
    "mockup.alt.resume":
      "LogRoom screenshot: a per-project resume briefing ready to copy into a coding agent.",
    "mockup.alt.digest": "LogRoom screenshot: the daily digest of captured sessions by project.",
    "mockup.alt.timeline":
      "LogRoom screenshot: the timeline, with agent sessions, pull requests and tickets in parallel lanes.",
    "mockup.alt.summary":
      "LogRoom screenshot: an AI-written summary of a period, grouped by project.",

    "problem.heading": "“What did I even do today?” “Where was I on this?”",
    "problem.item1":
      "Your day is scattered across AI sessions, Slack threads, tickets, and design comments.",
    "problem.item2": "Each tool remembers its own — nobody keeps the full picture, per project.",
    "problem.item3": "So you end every day and start every morning asking the same two questions.",

    "features.heading": "What LogRoom does with your records",
    "features.resume.title": "Pick up where you left off",
    "features.resume.description":
      "A resume briefing per project: recent work, what's unfinished, what's next. Copy it and paste it into your coding agent. You can preview exactly what gets sent to the summary engine first.",
    "features.summary.title": "Know what you did — day, week, month",
    "features.summary.description":
      "Summaries per project for any day, week or month. Records without a repository, like a Slack thread or a Notion page, go to the project they belong to; when that isn't clear, they land in Other.",
    "features.review.title": "Draft your self-review from records, not memory",
    "features.review.description":
      "Quarterly review and monthly check-in drafts built on factual signals from Linear and GitHub. No scores, no grades. Personal projects can be left out.",
    "features.agent.title": "Agent-driven work stays readable",
    "features.agent.description":
      "Agent sessions that run outside a repository are split by the repositories they actually touched, and marked “via agent”.",
    "features.connectors.title": "Every source, one timeline",
    "features.connectors.description":
      "Connectors run on your Mac with your own tokens. Notion supports multiple accounts; with a personal token it records only the titles of pages you edited.",

    "privacy.heading": "Everything stays on your device",
    "privacy.description": "No cloud, no accounts. Your history is never sent to a LogRoom server.",
    "privacy.architecture.capture": "Capture",
    "privacy.network.heading": "The only network requests LogRoom makes",
    "privacy.network.connectors":
      "Connectors you turn on — straight to GitHub, Slack, Linear or Notion with your own token.",
    "privacy.network.summary":
      "The summary engine you pick — your own CLI or API key. You can preview what will be sent.",
    "privacy.network.update":
      "An update check — downloads the release manifest and sends nothing; you can turn it off.",
    "privacy.verify": "Verify it in the code → GitHub",
    "privacy.proof": "This page makes zero external requests — check DevTools.",
    "privacy.tile.telemetry.title": "No telemetry",
    "privacy.tile.telemetry.description": "No usage data, no identifiers, no analytics.",
    "privacy.tile.scrubbing.title": "Secret scrubbing",
    "privacy.tile.scrubbing.description":
      "Known secret patterns like API keys and tokens are redacted before storage.",
    "privacy.tile.pause.title": "Pause anytime",
    "privacy.tile.pause.description": "Turn capture off instantly, no restart required.",
    "privacy.tile.openFormat.title": "Open format",
    "privacy.tile.openFormat.description":
      "Your history lives in a local SQLite database. Export the whole thing to NDJSON anytime.",

    "install.heading": "Installing LogRoom",
    "install.brew.title": "Homebrew",
    "install.brew.description":
      "The cask clears the quarantine flag during install, so there's no first-launch warning.",
    "install.dmg.title": "DMG — first launch will show a warning",
    "install.description":
      "LogRoom isn't notarized by Apple — it's built by one developer. Here's how to get past the warning.",
    "install.step1": "Open the .dmg and drag LogRoom to Applications.",
    "install.step2": "Launch it. macOS will refuse and say it can't verify the developer.",
    "install.step3":
      'Go to System Settings → Privacy & Security, scroll down, and click "Open Anyway".',
    "install.updateNote":
      "That's a one-time step. Every update after it is verified against a signing key compiled into the app — a tampered release can't install itself.",
    "install.archNote": "Apple Silicon only. Intel Macs aren't supported yet.",

    "openSource.heading": "Open source, MIT licensed",
    "openSource.description":
      "Read every line, build it yourself, or send a fix. Contributions are welcome.",
    "openSource.repo": "Repository",
    "openSource.contributing": "Contributing guide",
    "openSource.license": "MIT License",
    "openSource.build.heading": "Build from source",
    "openSource.build.note":
      "Needs Node.js 20+, pnpm 10, a stable Rust toolchain and Xcode Command Line Tools.",
    "openSource.issue.heading": "Bug or idea?",
    "openSource.issue.cta": "Open an issue",
    "openSource.issue.mailto": "or email us",

    "footer.tagline": "macOS first · Windows on the roadmap",
    "footer.github": "GitHub",
    "footer.issues": "Issues",
    "footer.license": "MIT License",

    "lang.nav.ariaLabel": "Language",
    "lang.switchToEnglish": "Switch to English",
    "lang.switchToKorean": "Switch to Korean",
  },
  ko: {
    "site.title": "LogRoom — 일한 기록은 내 Mac에, 요약은 내 AI로.",
    "site.description":
      "Claude Code·Kiro 세션과 GitHub·Slack·Linear·Notion을 한 타임라인에 모으고 일⁠·⁠주⁠·⁠월 요약을 만듭니다. 로컬 전용, MIT 오픈소스, 텔레메트리 없음.",
    "meta.keywords":
      "작업 기록, 업무 로그, AI 세션 기록, 로컬, 프라이버시, 오픈소스, 개발자 도구, Claude Code, Kiro, Notion, 일일 요약",
    "meta.ogImageAlt":
      "LogRoom — 당신의 모든 작업을 기억하고 정리합니다. 100% 로컬에서, 4색 병렬 스트림 타임라인.",

    "site.name": "LogRoom",

    "hero.brand": "LogRoom",
    "hero.title.line1": "일한 기록은 내 Mac에.",
    "hero.title.highlight": "요약은 내 AI로.",
    "hero.subtitle":
      "Claude Code·Kiro 세션, GitHub·Slack·Linear·Notion을 한 타임라인에 모읍니다. 일⁠·⁠주⁠·⁠월 요약, 이어서 하기 브리핑, 분기 평가서 초안까지. MIT 오픈소스, 텔레메트리 없음.",
    "hero.download": "macOS용 다운로드",
    "hero.github": "GitHub에서 소스 보기",
    "hero.brew.label": "Homebrew로 설치",
    "hero.brew.copy": "복사",
    "hero.brew.copied": "복사됨",
    "hero.brew.copyAriaLabel": "Homebrew 설치 명령 복사",
    "hero.requirements": "macOS · Apple Silicon",
    "hero.badge": "오픈소스 · MIT · 100% 로컬",

    "sourceStrip.works": "지원: Claude Code · Kiro · GitHub · Slack · Linear · Notion",

    "mockup.heading": "LogRoom 실제 화면",
    "mockup.tablist.ariaLabel": "제품 스크린샷",
    "mockup.tab.resume": "재개 브리핑",
    "mockup.tab.digest": "다이제스트",
    "mockup.tab.timeline": "타임라인",
    "mockup.tab.summary": "요약",
    "mockup.alt.resume": "LogRoom 화면: 코딩 에이전트에 붙여넣을 프로젝트별 재개 브리핑.",
    "mockup.alt.digest": "LogRoom 화면: 프로젝트별로 묶인 하루 세션 다이제스트.",
    "mockup.alt.timeline": "LogRoom 화면: 에이전트 세션·PR·티켓이 병렬 레인으로 놓인 타임라인.",
    "mockup.alt.summary": "LogRoom 화면: 기간을 프로젝트별로 정리한 AI 요약.",

    "problem.heading": "“내가 오늘 뭐 했더라?” “이거 어디까지 했더라?”",
    "problem.item1": "하루가 AI 세션, 슬랙 스레드, 티켓, 디자인 코멘트로 조각납니다.",
    "problem.item2": "각각의 툴은 자기 것만 기억합니다 — 프로젝트별 전체 그림은 어디에도 없습니다.",
    "problem.item3": "그래서 매일 하루의 끝과 시작마다, 같은 두 질문을 반복합니다.",

    "features.heading": "LogRoom이 기록으로 해 주는 일",
    "features.resume.title": "멈춘 곳에서 바로 이어서",
    "features.resume.description":
      "프로젝트마다 최근 작업·남은 일·다음 할 일을 정리한 재개 브리핑을 만듭니다. 복사해서 코딩 에이전트에 붙여넣으면 됩니다. 요약 엔진에 보낼 내용은 보내기 전에 미리 볼 수 있습니다.",
    "features.summary.title": "하루·한 주·한 달, 무엇을 했는지",
    "features.summary.description":
      "원하는 날·주·월을 프로젝트별로 요약합니다. 슬랙 스레드나 노션 페이지처럼 레포가 없는 기록은 맞는 프로젝트에 넣고, 어디인지 분명하지 않으면 '기타'로 둡니다.",
    "features.review.title": "평가서 초안은 기억이 아니라 기록으로",
    "features.review.description":
      "분기 평가와 월간 점검 초안을 Linear·GitHub의 사실 신호로 만듭니다. 점수나 등급은 매기지 않고, 개인 프로젝트는 뺄 수 있습니다.",
    "features.agent.title": "에이전트가 한 일도 읽히게",
    "features.agent.description":
      "레포 밖에서 돈 에이전트 세션을 실제로 만진 레포별로 나누고 'via agent' 로 표시합니다.",
    "features.connectors.title": "모든 소스를 한 타임라인에",
    "features.connectors.description":
      "커넥터는 내 Mac에서 내 토큰으로 돕니다. Notion은 여러 계정을 지원하고, 개인 토큰이면 내가 편집한 페이지의 제목만 기록합니다.",

    "privacy.heading": "모든 데이터는 당신의 기기 안에만",
    "privacy.description":
      "클라우드 없음. 계정 없음. 당신의 기록은 LogRoom 서버로 전송되지 않습니다.",
    "privacy.architecture.capture": "캡처",
    "privacy.network.heading": "LogRoom이 내보내는 네트워크 요청은 이것뿐입니다",
    "privacy.network.connectors":
      "켠 커넥터 — 내 토큰으로 GitHub·Slack·Linear·Notion에 직접 요청합니다.",
    "privacy.network.summary":
      "고른 요약 엔진 — 내 CLI 나 API 키로 보냅니다. 보낼 내용은 미리 볼 수 있습니다.",
    "privacy.network.update":
      "업데이트 확인 — 릴리스 목록 파일을 받아올 뿐 아무것도 보내지 않으며, 끌 수 있습니다.",
    "privacy.verify": "코드로 직접 확인하세요 → GitHub",
    "privacy.proof": "이 페이지는 외부 요청이 0회입니다 — DevTools로 직접 확인하세요.",
    "privacy.tile.telemetry.title": "텔레메트리 없음",
    "privacy.tile.telemetry.description": "사용 데이터도, 식별자도, 분석 도구도 없습니다.",
    "privacy.tile.scrubbing.title": "시크릿 스크러빙",
    "privacy.tile.scrubbing.description":
      "API 키·토큰 같은 알려진 시크릿 패턴은 저장 전에 가립니다.",
    "privacy.tile.pause.title": "언제든 일시정지",
    "privacy.tile.pause.description": "재시작 없이 즉시 캡처 중단.",
    "privacy.tile.openFormat.title": "오픈 포맷",
    "privacy.tile.openFormat.description":
      "기록은 내 Mac의 SQLite에 저장됩니다. 언제든 통째로 NDJSON으로 내보낼 수 있습니다.",

    "install.heading": "설치",
    "install.brew.title": "Homebrew",
    "install.brew.description":
      "설치할 때 격리(quarantine) 속성을 지우므로 처음 실행할 때 경고가 뜨지 않습니다.",
    "install.dmg.title": "DMG — 처음 실행하면 경고가 뜹니다",
    "install.description":
      "Apple 공증을 받지 않은 앱입니다 — 개발자 한 명이 만들고 있습니다. 경고를 넘어가는 방법은 이렇습니다.",
    "install.step1": ".dmg를 열고 LogRoom을 응용 프로그램으로 드래그하세요.",
    "install.step2": '실행하면 macOS가 "개발자를 확인할 수 없다"며 거부합니다.',
    "install.step3":
      '시스템 설정 → 개인정보 보호 및 보안으로 가서 아래로 내린 뒤 "그래도 열기"를 누르세요.',
    "install.updateNote":
      "이건 처음 한 번뿐입니다. 이후 모든 업데이트는 앱에 내장된 서명 키로 검증됩니다 — 변조된 릴리스는 설치되지 않습니다.",
    "install.archNote": "Apple Silicon 전용입니다. Intel Mac은 아직 지원하지 않습니다.",

    "openSource.heading": "MIT 라이선스 오픈소스",
    "openSource.description":
      "코드를 전부 읽어 보고, 직접 빌드하고, 고친 걸 보내 주세요. 기여를 환영합니다.",
    "openSource.repo": "저장소",
    "openSource.contributing": "기여 안내",
    "openSource.license": "MIT 라이선스",
    "openSource.build.heading": "소스에서 빌드",
    "openSource.build.note":
      "Node.js 20 이상, pnpm 10, 안정판 Rust 툴체인, Xcode Command Line Tools가 필요합니다.",
    "openSource.issue.heading": "버그나 아이디어가 있나요?",
    "openSource.issue.cta": "이슈 열기",
    "openSource.issue.mailto": "또는 이메일 보내기",

    "footer.tagline": "macOS 우선 출시 · Windows 지원 예정",
    "footer.github": "GitHub",
    "footer.issues": "이슈",
    "footer.license": "MIT 라이선스",

    "lang.nav.ariaLabel": "언어",
    "lang.switchToEnglish": "영어로 보기",
    "lang.switchToKorean": "한국어로 보기",
  },
} as const;
