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
# commit + tag → push the commit → draft GitHub release with the files attached → push the tag
# → publish the release. Users see nothing before the last step. If a step fails, run the same
# command again: it continues where it stopped (a failure before the commit restores Cargo.toml
# and Cargo.lock).
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

VERSION="${1:-}"
NOTES="${2:-}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "usage: scripts/release.sh <x.y.z> [notes.md]" >&2; exit 1; }
TAG="v$VERSION"
BRANCH="master"
CURRENT="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
FILES=("dist/Peler-$VERSION.dmg" "dist/Peler-$VERSION.pkg" "dist/Peler-$VERSION.zip" "dist/homebrew/peler.rb")

fail() { echo "Error: $*" >&2; exit 1; }
command -v gh >/dev/null || fail "the GitHub CLI (gh) is needed: brew install gh"
gh auth status >/dev/null 2>&1 || fail "log in first: gh auth login"
[ "$(git symbolic-ref --quiet --short HEAD || echo "a detached HEAD")" = "$BRANCH" ] ||
    fail "releases are made from $BRANCH (this is $(git symbolic-ref --quiet --short HEAD || echo "a detached HEAD"))"
[ -z "$(git status --porcelain)" ] || fail "commit or stash your changes first"
[ -z "$NOTES" ] || [ -f "$NOTES" ] || fail "no such notes file: $NOTES"
git fetch --quiet origin "$BRANCH"
git merge-base --is-ancestor "origin/$BRANCH" HEAD ||
    fail "$BRANCH is behind origin/$BRANCH (or has diverged): pull first"

# Where an earlier run stopped, if it did.
release_state() {
    gh release view "$TAG" --json isDraft --jq 'if .isDraft then "draft" else "published" end' 2>/dev/null ||
        echo "none"
}
STATE="$(release_state)"
[ "$STATE" != "published" ] || { echo "Peler $TAG is already released."; exit 0; }
LOCAL_TAG="$(git rev-parse -q --verify "refs/tags/$TAG^{commit}" || true)"
# The commit the tag on origin points at (an annotated tag is listed again peeled, with ^{}).
REMOTE_TAG="$(git ls-remote --tags origin |
    awk -v t="refs/tags/$TAG" '$2 == t { c = $1 } $2 == t "^{}" { p = $1 } END { print (p ? p : c) }')"
HEAD_SHA="$(git rev-parse HEAD)"
release_commit=false
[ "$CURRENT" = "$VERSION" ] && [ "$(git log -1 --format=%s)" = "Release $VERSION" ] && release_commit=true

if [ -n "$LOCAL_TAG" ] || [ -n "$REMOTE_TAG" ] || [ "$STATE" = "draft" ] || $release_commit; then
    echo "==> Continuing the release of $VERSION"
    [ -z "$REMOTE_TAG" ] || [ "$REMOTE_TAG" = "$HEAD_SHA" ] ||
        fail "tag $TAG on origin is commit $REMOTE_TAG, not this $BRANCH ($HEAD_SHA)"
    [ -z "$LOCAL_TAG" ] || [ "$LOCAL_TAG" = "$HEAD_SHA" ] ||
        fail "tag $TAG is commit $LOCAL_TAG, not this $BRANCH ($HEAD_SHA)"
    $release_commit || fail "$TAG was started, but this $BRANCH is not its \"Release $VERSION\" commit"
    if [ -z "$LOCAL_TAG" ]; then
        if [ -n "$REMOTE_TAG" ]; then
            git fetch --quiet origin "refs/tags/$TAG:refs/tags/$TAG"
        else
            git tag -a "$TAG" -m "Peler $VERSION"
        fi
    fi
    for f in "${FILES[@]}"; do
        if [ ! -f "$f" ]; then
            echo "==> Building (dist/ has no $f)"
            scripts/package.sh
            break
        fi
    done
else
    newest="$(printf '%s\n%s\n' "$CURRENT" "$VERSION" | sort -V | tail -1)"
    [ "$VERSION" != "$CURRENT" ] && [ "$newest" = "$VERSION" ] || fail "$VERSION must be newer than $CURRENT"

    echo "==> Releasing $CURRENT → $VERSION"
    # Until the release commit exists, a failure (or ^C) puts the manifests back.
    restore_manifests() {
        git checkout --quiet -- Cargo.toml Cargo.lock
        echo "Cargo.toml and Cargo.lock were restored." >&2
    }
    trap restore_manifests EXIT
    perl -0pi -e "s/^version = \"\Q$CURRENT\E\"/version = \"$VERSION\"/m" Cargo.toml
    cargo update --workspace --quiet

    echo "==> Testing"
    cargo test --quiet

    echo "==> Building"
    scripts/package.sh

    git commit --quiet -m "Release $VERSION" -- Cargo.toml Cargo.lock
    trap - EXIT
    git tag -a "$TAG" -m "Peler $VERSION"
    HEAD_SHA="$(git rev-parse HEAD)"
fi

echo "==> Pushing $BRANCH ($(git rev-list --count "origin/$BRANCH..HEAD") commit(s) not on origin yet)"
git push --quiet origin "HEAD:$BRANCH"

# Release notes: yours (or GitHub's generated list of changes), then how to install.
BODY="$(mktemp)"
trap 'rm -f "$BODY"' EXIT
if [ -n "$NOTES" ]; then
    cat "$NOTES" > "$BODY"
else
    REPO_SLUG="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
    gh api "repos/$REPO_SLUG/releases/generate-notes" \
        -f tag_name="$TAG" -f target_commitish="$HEAD_SHA" --jq .body > "$BODY"
fi
# The app shows the notes above the marker in its update dialog.
cat >> "$BODY" <<'INSTALL'

<!-- peler:end-of-notes -->

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

# A draft is invisible to users and to the in-app updater, and creates no tag yet.
if [ "$(release_state)" = "draft" ]; then
    echo "==> Updating the draft release"
    gh release upload "$TAG" "${FILES[@]}" --clobber
    gh release edit "$TAG" --title "Peler $VERSION" --notes-file "$BODY"
else
    echo "==> Creating a draft release"
    gh release create "$TAG" "${FILES[@]}" \
        --draft --target "$HEAD_SHA" --title "Peler $VERSION" --notes-file "$BODY"
fi

echo "==> Pushing the tag"
git push --quiet origin "refs/tags/$TAG"

echo "==> Publishing"
gh release edit "$TAG" --draft=false --latest

echo
echo "Released $TAG. Installed copies of Peler will offer the update on their next check."
