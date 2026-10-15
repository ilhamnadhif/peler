#!/usr/bin/env bash
# Builds Peler.app (universal: Apple Silicon + Intel), signs it, wraps it in a DMG and writes a
# Homebrew cask.
#
#   scripts/package.sh                 # ad-hoc signed (runs on this Mac)
#   PELER_SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" \
#   PELER_NOTARY_PROFILE=peler-notary scripts/package.sh
#                                      # Developer ID signing + Apple notarization
#
# PELER_NOTARY_PROFILE is a notarytool keychain profile, created once with:
#   xcrun notarytool store-credentials peler-notary --apple-id <id> --team-id <TEAMID>
# Set PELER_ARCHS="aarch64" to build only for Apple Silicon (faster, smaller).
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

APP_NAME="Peler"
BIN_NAME="peler"
BUNDLE_ID="${PELER_BUNDLE_ID:-dev.peler.app}"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
ARCHS="${PELER_ARCHS:-aarch64 x86_64}"
DIST="dist"
APP="$DIST/$APP_NAME.app"
DMG="$DIST/$APP_NAME-$VERSION.dmg"
export MACOSX_DEPLOYMENT_TARGET=12.0

echo "==> Building $APP_NAME $VERSION for: $ARCHS"
BINS=()
for arch in $ARCHS; do
    target="$arch-apple-darwin"
    rustup target add "$target" >/dev/null 2>&1 || true
    cargo build --release --locked --target "$target"
    BINS+=("target/$target/release/$BIN_NAME")
done

rm -rf "$APP" "$DMG"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
if [ "${#BINS[@]}" -gt 1 ]; then
    lipo -create -output "$APP/Contents/MacOS/$BIN_NAME" "${BINS[@]}"
else
    cp "${BINS[0]}" "$APP/Contents/MacOS/$BIN_NAME"
fi
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleLocalizations</key><array><string>en</string><string>id</string></array>
    <key>CFBundleDisplayName</key><string>$APP_NAME</string>
    <key>CFBundleExecutable</key><string>$BIN_NAME</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>$APP_NAME</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
    <key>NSHumanReadableCopyright</key><string>Peler — lightweight SSH/SFTP client</string>
</dict>
</plist>
PLIST

echo "==> Signing"
if [ -n "${PELER_SIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --options runtime --sign "$PELER_SIGN_IDENTITY" "$APP"
else
    echo "    (ad-hoc: set PELER_SIGN_IDENTITY for a Developer ID signature)"
    codesign --force --sign - "$APP"
fi
codesign --verify --strict --verbose=1 "$APP"

echo "==> Creating DMG"
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"
if [ -n "${PELER_SIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --sign "$PELER_SIGN_IDENTITY" "$DMG"
fi

if [ -n "${PELER_NOTARY_PROFILE:-}" ]; then
    echo "==> Notarizing (this can take a few minutes)"
    xcrun notarytool submit "$DMG" --keychain-profile "$PELER_NOTARY_PROFILE" --wait
    xcrun stapler staple "$DMG"
    xcrun stapler validate "$DMG"
fi

SHA="$(shasum -a 256 "$DMG" | awk '{print $1}')"
mkdir -p "$DIST/homebrew"
cat > "$DIST/homebrew/peler.rb" <<CASK
cask "peler" do
  version "$VERSION"
  sha256 "$SHA"

  # Upload the DMG to your release host and adjust this URL.
  url "https://example.com/peler/releases/Peler-#{version}.dmg"
  name "Peler"
  desc "Lightweight SSH and SFTP client"
  homepage "https://example.com/peler"

  depends_on macos: ">= :monterey"

  app "Peler.app"

  zap trash: [
    "~/Library/Application Support/peler",
  ]
end
CASK

echo
echo "App:      $APP ($(du -sh "$APP" | awk '{print $1}'))"
echo "DMG:      $DMG ($(du -h "$DMG" | awk '{print $1}'))"
echo "Cask:     $DIST/homebrew/peler.rb"
echo "Binary:   $(lipo -archs "$APP/Contents/MacOS/$BIN_NAME")"
