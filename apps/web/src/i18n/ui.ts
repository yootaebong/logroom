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
    "site.title": "LogRoom — Your work, remembered and organized. 100% locally.",
    "site.description":
      "LogRoom captures everything you do and organizes it by project — 100% on your device. No cloud, zero telemetry.",
    "meta.keywords":
      "work log, activity tracker, AI session recorder, local-first, privacy, developer tools, Claude Code, daily digest",
    "meta.ogImageAlt":
      "LogRoom — your work, remembered and organized, 100% locally, with a parallel-stream timeline of four colored lanes.",

    "site.name": "LogRoom",

    "hero.brand": "LogRoom",
    "hero.title.line1": "Close Claude Code —",
    "hero.title.highlight": "your next session continues.",
    "hero.subtitle": "Turn it on and LogRoom briefs you on where you left off.",
    "hero.description":
      "Copy the briefing, paste it into your coding agent, and pick up from where you stopped. Everything — capture and summaries alike — stays on your Mac.",
    "hero.download": "Download for macOS",
    "hero.downloadNote": "Apple Silicon · free while in early access",
    "hero.requirements": "macOS · Apple Silicon · best with Claude Code",
    "hero.badge": "100% local · zero telemetry",

    "sourceStrip.works": "Works with: Claude Code · Kiro CLI · Slack · GitHub · Linear",

    "mockup.heading": "See LogRoom in action",
    "mockup.srDescription":
      "A preview of the LogRoom desktop app: a resume briefing you can copy into your coding agent, a daily digest of captured AI sessions, a parallel-stream timeline, and full-text search across every prompt.",
    "mockup.tablist.ariaLabel": "Product preview view",
    "mockup.tab.resume": "Resume briefing",
    "mockup.tab.digest": "Digest",
    "mockup.tab.timeline": "Timeline",
    "mockup.tab.search": "Search",

    "problem.heading": "“What did I even do today?” “Where was I on this?”",
    "problem.item1":
      "Your day is scattered across AI sessions, Slack threads, tickets, and design comments.",
    "problem.item2": "Each tool remembers its own — nobody keeps the full picture, per project.",
    "problem.item3": "So you end every day and start every morning asking the same two questions.",

    "features.heading": "Two questions, always answered",
    "features.summary.title": "“What did I do?” — a daily, weekly, monthly story",
    "features.summary.description":
      "AI turns your captured sessions into a real narrative of your day, week, or month, per project — not just a list of timestamps. Skim the highlights, or read the full story.",
    "features.resume.title": "“Where did I leave off?” — a resume briefing",
    "features.resume.description":
      "AI writes up recent work, what's unfinished, and what's next for each project. Copy it to your clipboard and paste it straight into your coding agent to pick up exactly where you stopped.",
    "features.connectors.title": "Every source, one timeline",
    "features.connectors.description":
      "Claude Code, Kiro CLI, Slack, GitHub, and Linear all land in the same per-project view — no filing, no tagging.",
    "features.byoKey.title": "Pick your summary AI — bring your own key",
    "features.byoKey.description":
      "Choose Claude, GPT, or Gemini — via CLI (reuse your subscription) or your own API key. Summaries go straight from your device to your own account. Zero passes through our servers.",
    "features.tile.telemetry.title": "Zero telemetry — one exception",
    "features.tile.telemetry.description":
      "No usage data, no identifiers. The only request LogRoom makes is an update check that sends a version string and nothing else — and you can turn it off in Settings.",
    "features.tile.scrubbing.title": "Secret scrubbing",
    "features.tile.scrubbing.description":
      "API keys and tokens are redacted before they're ever stored.",
    "features.tile.pause.title": "Pause anytime",
    "features.tile.pause.description": "Turn capture off instantly, no restart required.",
    "features.tile.openFormat.title": "Open format",
    "features.tile.openFormat.description":
      "Your history lives in a local SQLite database. Export the whole thing to NDJSON anytime — grep it, move it, own it.",

    "install.heading": "First launch will show a warning",
    "install.description":
      "LogRoom isn't notarized by Apple yet — it's built by one developer, and notarization costs money we'd rather spend once people actually want this. Here's how to get past it.",
    "install.step1": "Open the .dmg and drag LogRoom to Applications.",
    "install.step2": "Launch it. macOS will refuse and say it can't verify the developer.",
    "install.step3":
      'Go to System Settings → Privacy & Security, scroll down, and click "Open Anyway".',
    "install.updateNote":
      "That's a one-time step. Every update after it is verified against a signing key compiled into the app — a tampered release can't install itself.",
    "install.archNote": "Apple Silicon only. Intel Macs aren't supported yet.",
    "privacy.heading": "Everything stays on your device",
    "privacy.description": "No cloud, no accounts. Your history is never sent to a LogRoom server.",
    "privacy.architecture.capture": "Capture",
    "privacy.byoKeyNote":
      "AI summaries go straight from your device to your own Claude, GPT, or Gemini account — zero passes through our servers.",
    "privacy.proof": "This page makes zero external requests — check DevTools.",

    "waitlist.heading": "Want the Windows build?",
    "waitlist.description":
      "LogRoom is macOS (Apple Silicon) only right now. Leave your email and we'll tell you when Windows support lands.",
    "waitlist.emailLabel": "Email address",
    "waitlist.submit": "Notify me",
    "waitlist.mailtoCta": "Email us to join",
    "waitlist.status.submitting": "Submitting…",
    "waitlist.status.success": "Thanks! You're on the list.",
    "waitlist.status.duplicate": "You're already on the list.",
    "waitlist.status.error": "Something went wrong. Please try again.",
    "waitlist.privacyNote":
      "Submitting makes a single request to store your email — nothing else is ever sent.",

    "feedback.heading": "Feedback or questions?",
    "feedback.description": "Tell us what's on your mind — a bug, an idea, anything.",
    "feedback.messageLabel": "Your feedback",
    "feedback.messagePlaceholder": "What's on your mind?",
    "feedback.emailLabel": "Email (optional, if you'd like a reply)",
    "feedback.submit": "Send feedback",
    "feedback.mailtoCta": "Email us your feedback",
    "feedback.status.submitting": "Sending…",
    "feedback.status.success": "Thanks for the feedback!",
    "feedback.status.error": "Something went wrong. Please try again.",
    "feedback.privacyNote":
      "Submitting makes a single request to store your feedback — nothing else is ever sent.",

    "footer.tagline": "macOS first · Windows on the roadmap",

    "lang.nav.ariaLabel": "Language",
    "lang.switchToEnglish": "Switch to English",
    "lang.switchToKorean": "Switch to Korean",
  },
  ko: {
    "site.title": "LogRoom — 당신의 모든 작업, 기억하고 정리합니다. 100% 로컬에서.",
    "site.description":
      "당신이 한 모든 일을 캡처해 프로젝트별로 정리합니다 — 100% 내 기기 안에서. 클라우드 없이, 텔레메트리 제로.",
    "meta.keywords":
      "작업 기록, 업무 로그, AI 세션 기록, 로컬, 프라이버시, 개발자 도구, Claude Code, 데일리 다이제스트",
    "meta.ogImageAlt":
      "LogRoom — 당신의 모든 작업을 기억하고 정리합니다. 100% 로컬에서, 4색 병렬 스트림 타임라인.",

    "site.name": "LogRoom",

    "hero.brand": "LogRoom",
    "hero.title.line1": "Claude Code를 닫아도,",
    "hero.title.highlight": "다음 작업은 이어집니다.",
    "hero.subtitle": "켜두면 어디까지 했는지 AI가 브리핑합니다.",
    "hero.description":
      "브리핑을 복사해 코딩 에이전트에 붙여넣으면 멈춘 지점부터 이어서 일합니다. 캡처도 요약도 전부 내 Mac 안에서.",
    "hero.download": "macOS용 다운로드",
    "hero.downloadNote": "Apple Silicon · 얼리 액세스 기간 무료",
    "hero.requirements": "macOS · Apple Silicon · Claude Code 사용자에게 가장 잘 맞습니다",
    "hero.badge": "100% 로컬 · 텔레메트리 제로",

    "sourceStrip.works": "지원: Claude Code · Kiro CLI · Slack · GitHub · Linear",

    "mockup.heading": "LogRoom이 실제로 작동하는 모습",
    "mockup.srDescription":
      "LogRoom 데스크톱 앱 미리보기입니다. 코딩 에이전트에 붙여넣을 수 있는 재개 브리핑, 캡처된 AI 세션의 데일리 다이제스트, 병렬 스트림 타임라인, 모든 프롬프트에 대한 전문 검색을 보여줍니다.",
    "mockup.tablist.ariaLabel": "제품 미리보기 화면",
    "mockup.tab.resume": "재개 브리핑",
    "mockup.tab.digest": "다이제스트",
    "mockup.tab.timeline": "타임라인",
    "mockup.tab.search": "검색",

    "problem.heading": "“내가 오늘 뭐 했더라?” “이거 어디까지 했더라?”",
    "problem.item1": "하루가 AI 세션, 슬랙 스레드, 티켓, 디자인 코멘트로 조각납니다.",
    "problem.item2": "각각의 툴은 자기 것만 기억합니다 — 프로젝트별 전체 그림은 어디에도 없습니다.",
    "problem.item3": "그래서 매일 하루의 끝과 시작마다, 같은 두 질문을 반복합니다.",

    "features.heading": "두 가지 질문에, 항상 답합니다",
    "features.summary.title": "“내가 뭐 했더라?” — 일·주·월 단위 서사",
    "features.summary.description":
      "캡처된 세션을 AI가 하루·한 주·한 달의 진짜 이야기로 정리합니다, 프로젝트별로 — 타임스탬프 나열이 아니라. 한눈에 훑거나, 전체를 자세히 읽거나.",
    "features.resume.title": "“어디까지 했더라?” — 재개 브리핑",
    "features.resume.description":
      "최근 작업, 미완결 항목, 다음 할 일을 프로젝트별로 AI가 정리합니다. 클립보드에 복사해서 코딩 에이전트에 그대로 붙여넣으면 멈춘 지점부터 바로 이어집니다.",
    "features.connectors.title": "모든 소스가 한 타임라인에",
    "features.connectors.description":
      "Claude Code, Kiro CLI, Slack, GitHub, Linear가 프로젝트별로 한곳에 모입니다 — 분류도 태깅도 필요 없이.",
    "features.byoKey.title": "요약 AI를 직접 선택 — 내 키로 직행",
    "features.byoKey.description":
      "Claude·GPT·Gemini 중 선택하세요 — CLI(구독 재활용) 또는 본인 API 키로. 요약은 내 기기에서 내 계정으로 직행합니다. 우리 서버 경유는 0.",
    "features.tile.telemetry.title": "텔레메트리 제로 — 예외 하나",
    "features.tile.telemetry.description":
      "사용 데이터도 식별자도 보내지 않습니다. 나가는 요청은 업데이트 확인 하나뿐이고, 보내는 건 현재 버전 문자열이 전부입니다. 설정에서 끌 수 있습니다.",
    "features.tile.scrubbing.title": "시크릿 스크러빙",
    "features.tile.scrubbing.description": "API 키와 토큰은 저장 전에 마스킹됩니다.",
    "features.tile.pause.title": "언제든 일시정지",
    "features.tile.pause.description": "재시작 없이 즉시 캡처 중단.",
    "features.tile.openFormat.title": "오픈 포맷",
    "features.tile.openFormat.description":
      "기록은 내 Mac의 SQLite에 저장됩니다. 언제든 통째로 NDJSON으로 내보내 grep하고, 소유하세요.",

    "install.heading": "처음 실행하면 경고가 뜹니다",
    "install.description":
      "아직 Apple 공증을 받지 않았습니다 — 개발자 한 명이 만들고 있고, 공증 비용은 실제로 필요로 하는 사람이 생긴 뒤에 쓰려고 합니다. 넘어가는 방법은 이렇습니다.",
    "install.step1": ".dmg를 열고 LogRoom을 응용 프로그램으로 드래그하세요.",
    "install.step2": '실행하면 macOS가 "개발자를 확인할 수 없다"며 거부합니다.',
    "install.step3":
      '시스템 설정 → 개인정보 보호 및 보안으로 가서 아래로 내린 뒤 "그래도 열기"를 누르세요.',
    "install.updateNote":
      "이건 처음 한 번뿐입니다. 이후 모든 업데이트는 앱에 내장된 서명 키로 검증됩니다 — 변조된 릴리스는 설치되지 않습니다.",
    "install.archNote": "Apple Silicon 전용입니다. Intel Mac은 아직 지원하지 않습니다.",
    "privacy.heading": "모든 데이터는 당신의 기기 안에만",
    "privacy.description":
      "클라우드 없음. 계정 없음. 당신의 기록은 LogRoom 서버로 전송되지 않습니다.",
    "privacy.architecture.capture": "캡처",
    "privacy.byoKeyNote":
      "AI 요약도 내 기기에서 본인의 Claude·GPT·Gemini 계정으로 직행합니다 — 우리 서버 경유는 0.",
    "privacy.proof": "이 페이지는 외부 요청이 0회입니다 — DevTools로 직접 확인하세요.",

    "waitlist.heading": "Windows 버전이 필요하신가요?",
    "waitlist.description":
      "지금은 macOS(Apple Silicon)만 지원합니다. 이메일을 남겨주시면 Windows 지원이 되는 대로 알려드릴게요.",
    "waitlist.emailLabel": "이메일 주소",
    "waitlist.submit": "알림 받기",
    "waitlist.mailtoCta": "이메일로 등록하기",
    "waitlist.status.submitting": "등록하는 중…",
    "waitlist.status.success": "감사합니다! 명단에 등록되었습니다.",
    "waitlist.status.duplicate": "이미 등록되어 있습니다.",
    "waitlist.status.error": "문제가 발생했습니다. 다시 시도해주세요.",
    "waitlist.privacyNote":
      "제출 시 이메일 저장을 위한 요청 1회만 발생합니다 — 그 외엔 아무것도 전송되지 않습니다.",

    "feedback.heading": "피드백 또는 문의",
    "feedback.description": "버그, 아이디어, 무엇이든 편하게 남겨주세요.",
    "feedback.messageLabel": "피드백 내용",
    "feedback.messagePlaceholder": "어떤 내용이든 남겨주세요",
    "feedback.emailLabel": "이메일 (답장 원하실 때만, 선택)",
    "feedback.submit": "피드백 보내기",
    "feedback.mailtoCta": "이메일로 피드백 보내기",
    "feedback.status.submitting": "보내는 중…",
    "feedback.status.success": "소중한 의견 감사합니다!",
    "feedback.status.error": "문제가 발생했습니다. 다시 시도해주세요.",
    "feedback.privacyNote":
      "제출 시 피드백 저장을 위한 요청 1회만 발생합니다 — 그 외엔 아무것도 전송되지 않습니다.",

    "footer.tagline": "macOS 우선 출시 · Windows 지원 예정",

    "lang.nav.ariaLabel": "언어",
    "lang.switchToEnglish": "영어로 보기",
    "lang.switchToKorean": "한국어로 보기",
  },
} as const;
