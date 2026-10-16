#!/usr/bin/env bash
# Publishes a new version of Peler on GitHub. Installed copies offer it to their users the next
# time they start (or within a day).
#
#   scripts/release.sh 1.1.0                 # release notes generated from the commits
#   scripts/release.sh 1.1.0 notes.md        # release notes from a file
#
# Set PELER_SIGN_IDENTITY, PELER_INSTALLER_IDENTITY and PELER_NOTARY_PROFILE (see package.sh) to
# publish Developer ID signed, notarized builds that open without any warning.
#
# Steps: checks → version bump in Cargo.toml → tests → build (.app, .dmg, .pkg, .zip, cask) →
# commit + tag → push → GitHub release with the files attached.
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

VERSION="${1:-}"
NOTES="${2:-}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "usage: scripts/release.sh <x.y.z> [notes.md]" >&2; exit 1; }
TAG="v$VERSION"
CURRENT="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"

fail() { echo "Error: $*" >&2; exit 1; }
command -v gh >/dev/null || fail "the GitHub CLI (gh) is needed: brew install gh"
gh auth status >/dev/null 2>&1 || fail "log in first: gh auth login"
[ -z "$(git status --porcelain)" ] || fail "commit or stash your changes first"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && fail "tag $TAG already exists"
newest="$(printf '%s\n%s\n' "$CURRENT" "$VERSION" | sort -V | tail -1)"
[ "$VERSION" != "$CURRENT" ] && [ "$newest" = "$VERSION" ] || fail "$VERSION must be newer than $CURRENT"
[ -z "$NOTES" ] || [ -f "$NOTES" ] || fail "no such notes file: $NOTES"

echo "==> Releasing $CURRENT → $VERSION"
perl -0pi -e "s/^version = \"\Q$CURRENT\E\"/version = \"$VERSION\"/m" Cargo.toml
cargo update --workspace --quiet

echo "==> Testing"
cargo test --quiet

echo "==> Building"
scripts/package.sh

git commit --quiet -am "Release $VERSION"
git tag -a "$TAG" -m "Peler $VERSION"

echo "==> Publishing"
git push --quiet origin HEAD
git push --quiet origin "$TAG"

# Release notes: yours (or GitHub's generated list of changes), then how to install.
BODY="$(mktemp)"
if [ -n "$NOTES" ]; then
    cat "$NOTES" > "$BODY"
else
    REPO_SLUG="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
    gh api "repos/$REPO_SLUG/releases/generate-notes" -f tag_name="$TAG" --jq .body > "$BODY"
fi
cat >> "$BODY" <<'INSTALL'

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/ilhamnadhif/peler/master/scripts/install.sh | bash
```

Or download the `.pkg` or `.dmg` below. Peler 1.1.0 and later update themselves from the app.
INSTALL
if [ -z "${PELER_NOTARY_PROFILE:-}" ]; then
    cat >> "$BODY" <<'GATEKEEPER'

**macOS says “Peler” Not Opened / “Apple could not verify…”?** This build is not notarized by
Apple yet, so macOS blocks a `.dmg`, `.pkg` or app downloaded with a browser. Since macOS 15,
right-click → Open no longer gets past it. Do one of these:

- **Use the one-line installer** above. Nothing downloaded with `curl` is blocked.
- **Allow it once**: click **Done** in the warning, open **System Settings → Privacy & Security**,
  scroll down to **Security**, and click **Open Anyway** next to “Peler was blocked…”. Confirm with
  your password, then open it again.
- **Or in Terminal**, after dragging the app to Applications:
  `xattr -dr com.apple.quarantine /Applications/Peler.app`
GATEKEEPER
fi
gh release create "$TAG" \
    "dist/Peler-$VERSION.dmg" "dist/Peler-$VERSION.pkg" "dist/Peler-$VERSION.zip" \
    "dist/homebrew/peler.rb" \
    --title "Peler $VERSION" --notes-file "$BODY"
rm -f "$BODY"

echo
echo "Released $TAG. Installed copies of Peler will offer the update on their next check."
