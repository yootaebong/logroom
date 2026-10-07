#!/usr/bin/env bash
# LogRoom 릴리스 배포 스크립트 — release.sh가 만든 산출물을 R2에 **올바른 순서로** 업로드한다.
#
# 사용법: ./scripts/publish.sh <version>
#   예)   ./scripts/publish.sh 0.13.0
#
# 왜 스크립트인가: release.sh는 업로드 방법을 "출력"만 했고, 그 결과 v0.13.0은 빌드 9일 뒤에야
# 올라갔다(그동안 사용자는 0.12.0을 받고 있었다). 잊을 수 있는 절차는 문서가 아니라 실행 파일로 둔다.
#
# 절차(순서가 중요하다):
#   1) 사전 검증 — 산출물 존재 · latest.json 버전 일치 · 서명 필드가 .sig와 동일
#   2) 버전 파일 업로드      → <version>/{LogRoom.app.tar.gz,.sig,*.dmg}
#   3) 업로드본 무결성 대조   → 라이브에서 다시 받아 sha256 비교
#   4) 사람용 별칭 갱신       → latest/LogRoom.dmg  (랜딩 다운로드 버튼이 가리키는 고정 주소)
#   5) latest.json 업로드     → **반드시 마지막**. 먼저 올리면 업데이터가 없는 아카이브를 받으러 가 404
#   6) 최종 확인             → 라이브 latest.json 버전 + 모든 URL 200
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BUCKET="logroom-releases"
RELEASES_HOST="https://releases.logroom.app"
# 사람이 받는 DMG의 고정 주소. 버전 경로를 랜딩에 하드코딩하면 릴리스마다 웹을 같이 고쳐야 하고,
# 안 고치면 버튼이 조용히 구버전을 내려준다. 별칭은 이 문제를 구조적으로 없앤다.
ALIAS_KEY="latest/LogRoom.dmg"

log() { printf '[publish] %s\n' "$1"; }
die() {
  printf '[publish] 오류: %s\n' "$1" >&2
  exit 1
}

# wrangler 4.x는 --remote 없이는 **로컬 스토리지**를 만지면서 "Upload complete"를 찍는다.
# 조용히 아무 일도 일어나지 않으므로 모든 호출에 --remote를 강제한다.
r2_put() {
  local key="$1" file="$2" content_type="$3"
  npx --yes wrangler@latest r2 object put "$BUCKET/$key" --remote --file="$file" --content-type="$content_type"
}

sha256() { shasum -a 256 "$1" | awk '{print $1}'; }

remote_sha256() {
  local url="$1"
  curl -fsSL "$url" | shasum -a 256 | awk '{print $1}'
}

# ── 0) 인자 · 사전 검증 ──────────────────────────────────────────

VERSION="${1:-}"
[ -n "$VERSION" ] || die "사용법: $0 <version>  (예: $0 0.13.0)"

OUT_DIR="$REPO_ROOT/release-out/$VERSION"
LATEST_JSON="$REPO_ROOT/release-out/latest.json"
ARCHIVE="$OUT_DIR/LogRoom.app.tar.gz"
SIGNATURE="$OUT_DIR/LogRoom.app.tar.gz.sig"
DMG="$OUT_DIR/LogRoom_${VERSION}_aarch64.dmg"

for f in "$ARCHIVE" "$SIGNATURE" "$DMG" "$LATEST_JSON"; do
  [ -f "$f" ] || die "산출물이 없습니다: ${f#"$REPO_ROOT"/} (release.sh를 먼저 실행하세요)"
done

command -v npx >/dev/null 2>&1 || die "npx가 필요합니다."
command -v jq >/dev/null 2>&1 || die "jq가 필요합니다."

MANIFEST_VERSION="$(jq -r '.version' "$LATEST_JSON")"
[ "$MANIFEST_VERSION" = "$VERSION" ] ||
  die "latest.json의 version($MANIFEST_VERSION)이 인자($VERSION)와 다릅니다 — 다른 릴리스의 매니페스트를 올릴 뻔했습니다."

# 매니페스트의 signature 필드가 실제 .sig와 다르면 업데이터가 서명 검증에 실패한다.
# 업로드 전에 잡아야 하는 결함이라 여기서 막는다.
MANIFEST_SIG="$(jq -r '.platforms."darwin-aarch64".signature' "$LATEST_JSON")"
[ "$MANIFEST_SIG" = "$(cat "$SIGNATURE")" ] ||
  die "latest.json의 signature가 $SIGNATURE 내용과 다릅니다 — 업데이터가 서명 검증에 실패합니다."

log "사전 검증 통과 — v$VERSION"

# ── 1) 버전 파일 업로드 ──────────────────────────────────────────

log "버전 파일 업로드 → $VERSION/"
r2_put "$VERSION/LogRoom.app.tar.gz" "$ARCHIVE" "application/gzip"
r2_put "$VERSION/LogRoom.app.tar.gz.sig" "$SIGNATURE" "application/octet-stream"
r2_put "$VERSION/$(basename "$DMG")" "$DMG" "application/octet-stream"

# ── 2) 업로드본 무결성 대조 ──────────────────────────────────────

log "업로드본 무결성 대조(라이브에서 다시 받아 sha256 비교)"
for pair in \
  "$VERSION/LogRoom.app.tar.gz|$ARCHIVE" \
  "$VERSION/LogRoom.app.tar.gz.sig|$SIGNATURE" \
  "$VERSION/$(basename "$DMG")|$DMG"; do
  key="${pair%%|*}"
  file="${pair##*|}"
  [ "$(remote_sha256 "$RELEASES_HOST/$key")" = "$(sha256 "$file")" ] ||
    die "$key 의 sha256이 로컬과 다릅니다 — 업로드가 손상됐습니다."
  log "  ok $key"
done

# ── 3) 사람용 별칭 ───────────────────────────────────────────────

log "별칭 갱신 → $ALIAS_KEY"
r2_put "$ALIAS_KEY" "$DMG" "application/octet-stream"
[ "$(remote_sha256 "$RELEASES_HOST/$ALIAS_KEY")" = "$(sha256 "$DMG")" ] ||
  die "$ALIAS_KEY 가 원본 DMG와 다릅니다."
log "  ok $ALIAS_KEY (원본 DMG와 sha256 일치)"

# ── 4) latest.json — 반드시 마지막 ───────────────────────────────

log "latest.json 업로드(마지막)"
r2_put "latest.json" "$LATEST_JSON" "application/json"

# ── 5) 최종 확인 ─────────────────────────────────────────────────

LIVE_VERSION="$(curl -fsSL "$RELEASES_HOST/latest.json" | jq -r '.version')"
[ "$LIVE_VERSION" = "$VERSION" ] || die "라이브 latest.json이 아직 $LIVE_VERSION 입니다."

UPDATER_URL="$(curl -fsSL "$RELEASES_HOST/latest.json" | jq -r '.platforms."darwin-aarch64".url')"
for url in "$UPDATER_URL" "$RELEASES_HOST/$ALIAS_KEY"; do
  code="$(curl -s -o /dev/null -w '%{http_code}' "$url")"
  [ "$code" = "200" ] || die "$url 이 $code 를 반환합니다."
done

log "완료 — v$VERSION 배포됨"
log "  업데이터   $UPDATER_URL"
log "  다운로드   $RELEASES_HOST/$ALIAS_KEY"
