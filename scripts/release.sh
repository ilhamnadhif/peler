#!/usr/bin/env bash
# Publishes a new version of Peler on GitHub. Installed copies offer it to their users the next
# time they start (or within a day).
#
#   scripts/release.sh 1.1.0                 # release notes generated from the commits
#   scripts/release.sh 1.1.0 notes.md        # release notes from a file
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
NOTES_ARGS=(--generate-notes)
[ -n "$NOTES" ] && NOTES_ARGS=(--notes-file "$NOTES")
gh release create "$TAG" \
    "dist/Peler-$VERSION.dmg" "dist/Peler-$VERSION.pkg" "dist/Peler-$VERSION.zip" \
    "dist/homebrew/peler.rb" \
    --title "Peler $VERSION" "${NOTES_ARGS[@]}"

echo
echo "Released $TAG. Installed copies of Peler will offer the update on their next check."
