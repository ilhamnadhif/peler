#!/usr/bin/env bash
# Installs (or updates) Peler from its GitHub releases into /Applications.
#
#   curl -fsSL https://raw.githubusercontent.com/ilhamnadhif/peler/master/scripts/install.sh | bash
#   ... | bash -s -- v1.0.0          # a specific version
#
# The app is downloaded with curl, so macOS does not quarantine it and it opens without the
# "unidentified developer" prompt.
set -euo pipefail

REPO="${PELER_REPO:-ilhamnadhif/peler}"
TAG="${1:-latest}"
APP_DIR="/Applications"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf 'Error: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "Peler is a macOS app."

if [ "$TAG" = "latest" ]; then
    API="https://api.github.com/repos/$REPO/releases/latest"
else
    API="https://api.github.com/repos/$REPO/releases/tags/$TAG"
fi
say "Looking up the release ($TAG)"
URL="$(curl -fsSL "$API" | grep -o '"browser_download_url": *"[^"]*\.dmg"' | head -1 | sed 's/.*"\(https[^"]*\)"/\1/')"
[ -n "$URL" ] || die "no DMG found in release $TAG of $REPO"

TMP="$(mktemp -d)"
MNT="$TMP/mnt"
cleanup() {
    hdiutil detach -quiet "$MNT" 2>/dev/null || true
    rm -rf "$TMP"
}
trap cleanup EXIT

say "Downloading $(basename "$URL")"
curl -fL --progress-bar -o "$TMP/Peler.dmg" "$URL"

mkdir -p "$MNT"
hdiutil attach -quiet -nobrowse -readonly -mountpoint "$MNT" "$TMP/Peler.dmg"
[ -d "$MNT/Peler.app" ] || die "the disk image does not contain Peler.app"

if pgrep -xq peler; then
    say "Quitting the running Peler"
    osascript -e 'quit app "Peler"' >/dev/null 2>&1 || true
    for _ in $(seq 1 20); do pgrep -xq peler || break; sleep 0.5; done
    pgrep -xq peler && die "Peler is still running; quit it and run this again."
fi

SUDO=""
[ -w "$APP_DIR" ] || SUDO="sudo"
say "Installing into $APP_DIR"
$SUDO rm -rf "$APP_DIR/Peler.app"
$SUDO ditto "$MNT/Peler.app" "$APP_DIR/Peler.app"
$SUDO xattr -dr com.apple.quarantine "$APP_DIR/Peler.app" 2>/dev/null || true

say "Done. Peler $(defaults read "$APP_DIR/Peler.app/Contents/Info" CFBundleShortVersionString 2>/dev/null || true) is in $APP_DIR."
open -a "$APP_DIR/Peler.app" || true
