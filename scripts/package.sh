#!/usr/bin/env bash
# Builds Peler.app (universal: Apple Silicon + Intel), signs it, and produces the installers:
#   dist/Peler-<version>.dmg   drag-to-Applications disk image
#   dist/Peler-<version>.pkg   macOS Installer package (installs into /Applications)
#   dist/Peler-<version>.zip   the app alone, which the in-app updater downloads
#   dist/homebrew/peler.rb     Homebrew cask pointing at the GitHub release
#
#   scripts/package.sh                 # ad-hoc signed (runs on this Mac)
#   PELER_SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" \
#   PELER_INSTALLER_IDENTITY="Developer ID Installer: Name (TEAMID)" \
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
# The built app sits in a `.noindex` folder: Spotlight skips it, so macOS doesn't offer (and
# open) this copy instead of the installed one when you launch Peler.
APP="$DIST/app.noindex/$APP_NAME.app"
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
DMG="$DIST/$APP_NAME-$VERSION.dmg"
PKG="$DIST/$APP_NAME-$VERSION.pkg"
ZIP="$DIST/$APP_NAME-$VERSION.zip"
REPO="${PELER_REPO:-ilhamnadhif/peler}"
export MACOSX_DEPLOYMENT_TARGET=12.0

echo "==> Building $APP_NAME $VERSION for: $ARCHS"
BINS=()
for arch in $ARCHS; do
    target="$arch-apple-darwin"
    rustup target add "$target" >/dev/null 2>&1 || true
    cargo build --release --locked --target "$target"
    BINS+=("target/$target/release/$BIN_NAME")
done

# Where earlier versions of this script put the app.
if [ -d "$DIST/$APP_NAME.app" ]; then
    "$LSREGISTER" -u "$DIST/$APP_NAME.app" 2>/dev/null || true
    rm -rf "${DIST:?}/$APP_NAME.app"
fi
rm -rf "$APP" "$DMG" "$PKG" "$ZIP"
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
    <key>NSHumanReadableCopyright</key><string>© 2026 ilhamnadhif. MIT License.</string>
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

echo "==> Creating update archive"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"

echo "==> Creating DMG"
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
if [ -z "${PELER_NOTARY_PROFILE:-}" ]; then
    # Not notarized: say how to get past Gatekeeper, right next to the app.
    cat > "$STAGE/If Peler does not open.txt" <<'HELP'
macOS says "Peler" Not Opened / "Apple could not verify Peler is free of malware"?

This build of Peler is not notarized by Apple yet, so macOS blocks it the first time.
To open it:

1. Drag Peler onto Applications (in this window).
2. Open Peler. When the warning appears, click "Done".
3. Open System Settings > Privacy & Security, scroll down to "Security",
   and click "Open Anyway" next to "Peler was blocked...". Enter your password.
4. Open Peler again and click "Open Anyway".

Or, in Terminal:   xattr -dr com.apple.quarantine /Applications/Peler.app

Easiest of all, install from Terminal instead (nothing is blocked):
   curl -fsSL https://raw.githubusercontent.com/ilhamnadhif/peler/master/scripts/install.sh | bash
HELP
fi
hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"
if [ -n "${PELER_SIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --sign "$PELER_SIGN_IDENTITY" "$DMG"
fi

echo "==> Creating installer package"
PKG_ROOT="$(mktemp -d)"
PKG_WORK="$(mktemp -d)"
cp -R "$APP" "$PKG_ROOT/"
pkgbuild --analyze --root "$PKG_ROOT" "$PKG_WORK/component.plist" >/dev/null
# Always install into /Applications, never "update" a copy found elsewhere on the disk.
plutil -replace 0.BundleIsRelocatable -bool NO "$PKG_WORK/component.plist"
pkgbuild --quiet \
    --root "$PKG_ROOT" \
    --component-plist "$PKG_WORK/component.plist" \
    --install-location /Applications \
    --identifier "$BUNDLE_ID.pkg" \
    --version "$VERSION" \
    "$PKG_WORK/Peler-component.pkg"
cat > "$PKG_WORK/distribution.xml" <<DIST
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
    <title>$APP_NAME $VERSION</title>
    <options customize="never" require-scripts="false" hostArchitectures="arm64,x86_64"/>
    <allowed-os-versions><os-version min="12.0"/></allowed-os-versions>
    <domains enable_localSystem="true"/>
    <choices-outline><line choice="default"><line choice="$BUNDLE_ID"/></line></choices-outline>
    <choice id="default"/>
    <choice id="$BUNDLE_ID" visible="false"><pkg-ref id="$BUNDLE_ID.pkg"/></choice>
    <pkg-ref id="$BUNDLE_ID.pkg" version="$VERSION" onConclusion="none">Peler-component.pkg</pkg-ref>
</installer-gui-script>
DIST
PRODUCT_SIGN=()
if [ -n "${PELER_INSTALLER_IDENTITY:-}" ]; then
    PRODUCT_SIGN=(--sign "$PELER_INSTALLER_IDENTITY" --timestamp)
fi
productbuild --quiet ${PRODUCT_SIGN[@]+"${PRODUCT_SIGN[@]}"} \
    --distribution "$PKG_WORK/distribution.xml" \
    --package-path "$PKG_WORK" \
    "$PKG"
rm -rf "$PKG_ROOT" "$PKG_WORK"

if [ -n "${PELER_NOTARY_PROFILE:-}" ]; then
    echo "==> Notarizing (this can take a few minutes)"
    for artifact in "$DMG" "$PKG"; do
        xcrun notarytool submit "$artifact" --keychain-profile "$PELER_NOTARY_PROFILE" --wait
        xcrun stapler staple "$artifact"
        xcrun stapler validate "$artifact"
    done
fi

SHA="$(shasum -a 256 "$DMG" | awk '{print $1}')"
mkdir -p "$DIST/homebrew"
cat > "$DIST/homebrew/peler.rb" <<CASK
cask "peler" do
  version "$VERSION"
  sha256 "$SHA"

  url "https://github.com/$REPO/releases/download/v#{version}/Peler-#{version}.dmg"
  name "Peler"
  desc "Lightweight SSH and SFTP client"
  homepage "https://github.com/$REPO"

  depends_on macos: ">= :monterey"

  app "Peler.app"

  zap trash: [
    "~/Library/Application Support/peler",
  ]
end
CASK

# Forget the build if anything registered it, so "Peler" keeps meaning the installed app.
"$LSREGISTER" -u "$APP" 2>/dev/null || true

echo
echo "App:      $APP ($(du -sh "$APP" | awk '{print $1}'))"
echo "DMG:      $DMG ($(du -h "$DMG" | awk '{print $1}'))"
echo "PKG:      $PKG ($(du -h "$PKG" | awk '{print $1}'))"
echo "ZIP:      $ZIP ($(du -h "$ZIP" | awk '{print $1}'))"
echo "Cask:     $DIST/homebrew/peler.rb"
echo "Binary:   $(lipo -archs "$APP/Contents/MacOS/$BIN_NAME")"
