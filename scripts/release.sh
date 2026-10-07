#!/usr/bin/env bash
# LogRoom 데스크톱 릴리스 스크립트(M5, ADR-0013 "공증 없는 배포").
#
# 사용법: ./scripts/release.sh <version> ["release notes"]
#   예)   ./scripts/release.sh 0.2.0 "검색 성능 개선, 캡처 헬스 배지"
#
# 절차:
#   1) 버전 bump — tauri.conf.json + Cargo.toml([package]만) + apps/desktop/package.json
#   2) `pnpm --filter @logroom/desktop tauri build` (updater 아티팩트 포함, 서명 필요)
#   3) 산출물(.dmg, .app.tar.gz + .sig)을 release-out/<version>/ 에 수집
#   4) latest.json 생성(version/notes/pub_date/platforms.darwin-aarch64)
#   5) 배포 안내 출력 — 실제 업로드는 scripts/publish.sh 가 한다(빌드와 배포를 분리)
#
# 공증(Apple notarization) 없는 배포 전략(ADR-0013): 무결성은 Tauri updater의 minisign 서명으로
# 보장한다. 첫 실행 시 Gatekeeper가 "확인되지 않은 개발자" 경고를 띄울 수 있으며, 이는 M6에서
# 공증 도입 전까지 알려진 제약이다(docs/04-privacy-security.md 참고).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DESKTOP_DIR="$REPO_ROOT/apps/desktop"
SRC_TAURI_DIR="$DESKTOP_DIR/src-tauri"
BUNDLE_DIR="$SRC_TAURI_DIR/target/release/bundle"

# ADR-0008(macOS 우선)이라 이번 스크립트는 Apple Silicon(darwin-aarch64) 타깃만 다룬다.
# Windows/x86_64는 M7 이후 이 스크립트를 확장해서 대응한다.
TARGET_KEY="darwin-aarch64"
DEFAULT_SIGNING_KEY_PATH="$HOME/.tauri/logroom.key"
RELEASES_HOST="https://releases.logroom.app"

log() { printf '[release] %s\n' "$1"; }
die() {
  printf '[release] 오류: %s\n' "$1" >&2
  exit 1
}

# ── 0) 인자 검증 ────────────────────────────────────────────────

VERSION="${1:-}"
NOTES="${2:-}"

if [ -z "$VERSION" ]; then
  die "사용법: $0 <version> [\"release notes\"]  (예: $0 0.2.0)"
fi

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  die "버전 형식이 SemVer가 아닙니다: $VERSION (예: 0.2.0)"
fi

if [ -z "$NOTES" ]; then
  NOTES="LogRoom v$VERSION"
  log "release notes 인자가 없어 기본값을 사용합니다: \"$NOTES\" — 업로드 전 latest.json에서 직접 수정하세요."
fi

command -v pnpm >/dev/null 2>&1 || die "pnpm이 필요합니다."
command -v node >/dev/null 2>&1 || die "node가 필요합니다(버전 bump용 JSON 처리)."

# ── 1) 버전 bump ────────────────────────────────────────────────

bump_json_version() {
  local file="$1"
  node -e '
    const fs = require("node:fs");
    const [, path, version] = process.argv;
    const json = JSON.parse(fs.readFileSync(path, "utf8"));
    json.version = version;
    fs.writeFileSync(path, `${JSON.stringify(json, null, 2)}\n`);
  ' "$file" "$VERSION"
}

bump_cargo_toml_version() {
  local file="$1"
  # [package] 섹션 안의 version 필드만 바꾼다(의존성의 version = "..." 문자열은 건드리지 않음).
  awk -v new="$VERSION" '
    /^\[package\]/ { in_package = 1; print; next }
    /^\[/          { in_package = 0; print; next }
    in_package && /^version[[:space:]]*=/ { print "version = \"" new "\""; next }
    { print }
  ' "$file" > "$file.tmp"
  mv "$file.tmp" "$file"
}

log "버전 bump → $VERSION"
bump_json_version "$SRC_TAURI_DIR/tauri.conf.json"
bump_json_version "$DESKTOP_DIR/package.json"
bump_cargo_toml_version "$SRC_TAURI_DIR/Cargo.toml"

# ── 2) 빌드(서명 포함) ───────────────────────────────────────────

if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  if [ -f "$DEFAULT_SIGNING_KEY_PATH" ]; then
    log "TAURI_SIGNING_PRIVATE_KEY 미설정 — 기본 경로 사용: $DEFAULT_SIGNING_KEY_PATH"
    export TAURI_SIGNING_PRIVATE_KEY="$DEFAULT_SIGNING_KEY_PATH"
  else
    die "TAURI_SIGNING_PRIVATE_KEY 환경변수가 없고 $DEFAULT_SIGNING_KEY_PATH 도 없습니다. 다음처럼 설정 후 재실행하세요:
  export TAURI_SIGNING_PRIVATE_KEY=\"\$HOME/.tauri/logroom.key\"   # 경로 또는 키 내용
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=\"\"                   # 비밀번호 없이 생성한 키면 빈 문자열"
  fi
fi
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"

# desktop 크레이트를 강제 재컴파일한다(의존성은 유지 — 1~2분). 이유: 프론트엔드는
# `tauri build`의 beforeBuildCommand(vite)로 매번 새로 빌드되지만, Tauri는 그 산출물(dist)을
# **desktop 바이너리에 컴파일타임 임베드**한다. Rust 소스가 안 바뀐 릴리스(예: FE 전용 변경)에서는
# cargo가 desktop을 재컴파일하지 않아 **이전 dist가 임베드된 바이너리가 그대로 번들되는** 버그가
# 있다(Info 버전만 bump되고 화면은 구버전 — 실제 발생). 매 릴리스 clean으로 최신 dist 임베드를
# 보장한다(릴리스는 자주 하지 않으므로 1~2분 추가는 신뢰성 대비 값싸다).
log "desktop 크레이트 clean(최신 프론트 임베드 보장)"
cargo clean -p desktop --release --manifest-path "$SRC_TAURI_DIR/Cargo.toml"

# 데스크톱은 @logroom/core를 소스가 아니라 **빌드 산출물(packages/core/dist)** 로 읽는다(package.json의
# main/types). `tauri build`의 beforeBuildCommand는 데스크톱만 빌드하므로, core가 바뀐 릴리스에서
# dist가 예전 것이면 타입 검사가 실패하거나(v0.14.0 Notion 추가 때 실제 발생 — Source에 "notion"이
# 없었다) 더 나쁘면 예전 스키마가 번들된다. 매 릴리스 core를 먼저 빌드한다.
log "@logroom/core 빌드(데스크톱이 읽는 dist 갱신)"
pnpm --filter @logroom/core build

log "빌드 시작: pnpm --filter @logroom/desktop tauri build (updater 아티팩트 포함)"
pnpm --filter @logroom/desktop tauri build

# ── 3) 산출물 수집 ───────────────────────────────────────────────

OUT_DIR="$REPO_ROOT/release-out/$VERSION"
mkdir -p "$OUT_DIR"

log "산출물 수집 → $OUT_DIR"

shopt -s nullglob
dmg_files=("$BUNDLE_DIR"/dmg/*.dmg)
updater_archives=("$BUNDLE_DIR"/macos/*.app.tar.gz)
updater_signatures=("$BUNDLE_DIR"/macos/*.app.tar.gz.sig)
shopt -u nullglob

[ ${#dmg_files[@]} -gt 0 ] || die ".dmg 산출물을 찾지 못했습니다: $BUNDLE_DIR/dmg/*.dmg (빌드 실패 여부 확인)"
[ ${#updater_archives[@]} -gt 0 ] || die "updater .app.tar.gz 산출물을 찾지 못했습니다(createUpdaterArtifacts 설정 확인)"
[ ${#updater_signatures[@]} -gt 0 ] || die "updater .sig 서명 파일을 찾지 못했습니다(TAURI_SIGNING_PRIVATE_KEY 설정 확인)"

cp "${dmg_files[@]}" "${updater_archives[@]}" "${updater_signatures[@]}" "$OUT_DIR/"

# ── 4) latest.json 생성 ──────────────────────────────────────────

UPDATER_ARCHIVE_NAME="$(basename "${updater_archives[0]}")"
SIGNATURE_CONTENT="$(cat "${updater_signatures[0]}")"
PUB_DATE="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
DOWNLOAD_URL="$RELEASES_HOST/$VERSION/$UPDATER_ARCHIVE_NAME"

LATEST_JSON="$OUT_DIR/../latest.json"
node -e '
  const fs = require("node:fs");
  const [, outPath, version, notes, pubDate, targetKey, signature, url] = process.argv;
  const manifest = {
    version,
    notes,
    pub_date: pubDate,
    platforms: {
      [targetKey]: { signature, url },
    },
  };
  fs.writeFileSync(outPath, `${JSON.stringify(manifest, null, 2)}\n`);
' "$LATEST_JSON" "$VERSION" "$NOTES" "$PUB_DATE" "$TARGET_KEY" "$SIGNATURE_CONTENT" "$DOWNLOAD_URL"

log "latest.json 생성 완료 → $(cd "$(dirname "$LATEST_JSON")" && pwd)/$(basename "$LATEST_JSON")"

# ── 5) 배포 안내 ────────────────────────────────────────────────

cat <<EOF

[release] 빌드·수집 완료. 이제 아래 한 줄로 배포하세요:

  ./scripts/publish.sh $VERSION

publish.sh가 하는 일(순서가 중요해 스크립트로 고정했다):
  1) 사전 검증 — latest.json의 version/signature가 산출물과 일치하는지
  2) release-out/$VERSION/* 를 버킷의 "$VERSION/" prefix 아래로 업로드
  3) 라이브에서 다시 받아 sha256 대조
  4) 사람용 고정 주소 latest/LogRoom.dmg 갱신(랜딩 다운로드 버튼이 가리키는 곳)
  5) **마지막에** latest.json 업로드 — 먼저 올리면 업데이터가 404를 받는다
  6) 라이브 재확인

업로드 전에 release-out/latest.json의 "notes" 필드가 실제 릴리스 노트인지 다시 확인하세요.
EOF
