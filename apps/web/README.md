# @logroom/web

LogRoom 랜딩(오픈소스 소개·다운로드). Astro + Tailwind v4, 정적 1페이지(SSG). 애널리틱스/외부 스크립트/폰트 CDN 없음(ADR-0011).

## 로컬 개발

모노레포 루트에서:

```bash
pnpm install
pnpm --filter @logroom/web dev      # http://localhost:4321
pnpm --filter @logroom/web build    # dist/ 생성
pnpm --filter @logroom/web preview  # 빌드 결과 로컬 미리보기
```

또는 루트 `pnpm run build`(turbo)로 다른 워크스페이스와 함께 빌드된다.

## 배포 (Vercel 또는 Cloudflare Pages, 모노레포에서 `apps/web`만)

공통: GitHub 에서 이 저장소를 그대로 연결하고, 아래처럼 **루트 디렉터리를 `apps/web`으로 지정**한다.

### Vercel

1. New Project → 이 레포 선택.
2. Root Directory: `apps/web`.
3. Framework Preset: Astro (자동 인식). Build Command/Output Directory는 기본값(`astro build` / `dist`) 유지.
4. Install Command는 모노레포 인식을 위해 `pnpm install --frozen-lockfile` (루트 lockfile 기준, Vercel이 workspace 루트를 자동 탐지).

환경변수는 필요 없다(폼·외부 수집 없음).

### Cloudflare Pages

1. Workers & Pages → Create → Connect to Git → 이 레포 선택.
2. Build command: `cd ../.. && pnpm install && pnpm --filter @logroom/web build`
3. Build output directory: `apps/web/dist`
4. Root directory: 리포 루트(모노레포 lockfile을 보게 하기 위해 `apps/web`로 좁히지 않는다).

## 도메인 연결 (logroom.app, 구매 완료)

1. 위 배포가 정상 동작(임시 URL로 확인)하는 시점에 진행.
2. 배포 플랫폼의 Custom Domain에 `logroom.app`(+ 필요 시 `www.logroom.app`) 추가.
3. 도메인 등록기관 DNS에 플랫폼이 안내하는 레코드 등록:
   - Vercel: `A`/`ALIAS`(apex) + `CNAME`(www) — Vercel이 제시하는 값 그대로.
   - Cloudflare Pages: 도메인이 이미 Cloudflare에 있다면 자동 프록시 CNAME, 아니면 안내되는 `CNAME`/`A` 값 등록.
4. DNS 전파 확인 후 HTTPS(자동 발급) 확인.
5. `astro.config.mjs`의 `site: "https://logroom.app"`는 이미 확정 도메인으로 설정되어 있음(OG/canonical 메타에 사용).

## 검색 엔진 등록

도메인 연결과 `sitemap-index.xml` 배포(`astro.config.mjs`의 `@astrojs/sitemap` 통합으로 빌드 시 자동 생성)가 끝난 뒤 진행한다.

### Google Search Console

1. [Google Search Console](https://search.google.com/search-console)에서 **속성 추가 → 도메인** 선택 후 `logroom.app` 입력(도메인 속성은 www/서브도메인/http·https를 모두 포괄한다).
2. 안내되는 **TXT 레코드**를 Cloudflare DNS(도메인이 등록된 CF 계정)의 DNS 설정에서 `logroom.app` 루트에 추가해 소유권을 확인한다.
3. 확인 후 좌측 메뉴 **Sitemaps**에서 `https://logroom.app/sitemap-index.xml`을 제출한다.

### 네이버 서치어드바이저

1. [네이버 서치어드바이저](https://searchadvisor.naver.com)에서 사이트 등록 → `https://logroom.app` 입력.
2. 소유 확인 방식으로 **HTML 태그** 방식을 선택한 경우, 발급되는 `<meta name="naver-site-verification" content="...">` 태그를 `src/layouts/BaseLayout.astro`의 `<head>` 안, 다른 `<meta>` 태그들 옆에 추가한다. (실제 값 없이는 추가할 수 없어 이번 작업에는 코드가 포함되지 않았다 — 등록 시점에 발급받은 값으로 직접 추가할 것.)
3. 소유 확인 후 **요청 → 사이트맵 제출**에서 `https://logroom.app/sitemap-index.xml`을 등록한다.

## 참고

- 이 사이트는 애널리틱스·이메일 수집·외부 요청이 없고, 데스크톱 앱도 어떤 사용 데이터도 보내지 않는다(ADR-0011, `docs/07-decisions.md`).
- 제품 화면은 `public/screens/{resume,digest,timeline,summary}.png` 실제 스크린샷이다(`src/components/ProductMockup.astro`). 파일을 바꾸면 같은 파일의 width/height 도 실제 픽셀 크기로 맞춘다.
- biome 2.5.1은 `.astro` 파서를 지원하지 않는다(frontmatter만 읽어 템플릿에서의 변수 사용을 인식 못 하고 오탐 발생 확인됨). 그래서 루트 `biome.json`의 `files.includes`에 `!**/*.astro` / `!**/apps/web/.astro`를 추가해 `.astro` 소스 파일과 Astro가 생성하는 `.astro/`(타입 캐시, gitignore 대상) 디렉터리를 검사 대상에서 명시적으로 제외했다. `.ts`/`.mjs`/`.json`/`.css` 등 다른 파일은 정상적으로 lint/format 대상이다.
