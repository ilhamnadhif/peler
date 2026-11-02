#!/usr/bin/env bash
# Publishes the release of the pushed tag from CI (the publish job, after the tests passed and
# the bundle jobs built dist/): scripts/release.sh drafted it with the macOS files; this adds the
# Windows and Linux files and publishes it. Installed copies are offered it once published.
#
#   GITHUB_REF_NAME=v1.2.3 GH_TOKEN=... scripts/ci-publish.sh
#
# A tag pushed without scripts/release.sh gets a draft with these files, to finish by hand.
set -euo pipefail

cd "$(dirname "$0")/.."

TAG="${GITHUB_REF_NAME:?the tag to publish}"
VERSION="${TAG#v}"
fail() { echo "Error: $*" >&2; exit 1; }
grep -q "^version = \"$VERSION\"" Cargo.toml || fail "Cargo.toml is not at version $VERSION"

FILES=("dist/TernaryShell-$VERSION-windows-x64-setup.exe" "dist/TernaryShell-$VERSION-windows-x64.zip")
for arch in x86_64 aarch64; do
    case "$arch" in x86_64) deb=amd64 ;; aarch64) deb=arm64 ;; esac
    FILES+=("dist/TernaryShell-$VERSION-linux-$arch.tar.gz" "dist/TernaryShell-$VERSION-linux-$arch.AppImage"
        "dist/TernaryShell-$VERSION-linux-$deb.deb" "dist/TernaryShell-$VERSION-linux-$arch.rpm")
done
for f in "${FILES[@]}"; do [ -f "$f" ] || fail "missing $f"; done

STATE="$(gh release view "$TAG" --json isDraft --jq 'if .isDraft then "draft" else "published" end' 2>/dev/null || echo none)"
case "$STATE" in
    draft)
        echo "==> Adding the Windows and Linux files to the draft"
        gh release upload "$TAG" --clobber "${FILES[@]}"
        echo "==> Publishing"
        gh release edit "$TAG" --draft=false --latest
        ;;
    published)
        echo "==> $TAG is published already: replacing its Windows and Linux files"
        gh release upload "$TAG" --clobber "${FILES[@]}"
        ;;
    none)
        echo "==> No release for $TAG (not made by scripts/release.sh): a draft with these files"
        gh release create "$TAG" --draft --verify-tag --title "TernaryShell $VERSION" --generate-notes "${FILES[@]}"
        ;;
esac
echo "==> $(gh release view "$TAG" --json url --jq .url)"
