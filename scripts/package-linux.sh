#!/usr/bin/env bash
# Packages the Linux build for this machine's architecture (x86_64 or aarch64) into dist/:
#
#   TernaryShell-<v>-linux-<arch>.tar.gz     any distribution: unpack into ~/.local (see the README)
#   TernaryShell-<v>-linux-<amd64|arm64>.deb Debian, Ubuntu, Mint, Pop!_OS …
#   TernaryShell-<v>-linux-<arch>.rpm        Fedora, RHEL, Rocky, AlmaLinux, openSUSE …
#   TernaryShell-<v>-linux-<arch>.AppImage   most others (Arch, Manjaro …); updates itself
#
#   cargo build --release --locked
#   scripts/package-linux.sh
#
# Needs dpkg-deb, rpmbuild and, for the AppImage, appimagetool (downloaded into target/ unless
# APPIMAGETOOL points to one). Build against the oldest glibc you want to support: CI builds in
# manylinux_2_28 (glibc 2.28: RHEL 8, Debian 10, Ubuntu 18.10 and newer).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) DEB_ARCH=amd64 ;;
  aarch64) DEB_ARCH=arm64 ;;
  *) echo "unsupported architecture: $ARCH" >&2; exit 1 ;;
esac
BIN="${TERNARYSHELL_BIN:-$ROOT/target/release/ternaryshell}"
OUT="$ROOT/dist"
WORK="$ROOT/target/linux-pkg"
REPO="https://github.com/ilhamnadhif/ternaryshell"
SUMMARY="Lightweight SSH and SFTP client"
DESCRIPTION="TernaryShell is a fast, native SSH and SFTP client written in Rust: saved connections in
groups, tabs and split panes, a dual-pane SFTP browser with background transfers, remote file
editing, jump hosts, port forwarding and SSH agents. No account, no telemetry."
NAME="TernaryShell-$VERSION-linux"

[[ -x "$BIN" ]] || { echo "missing $BIN: run cargo build --release first" >&2; exit 1; }
[[ "$("$BIN" --version)" == "TernaryShell $VERSION" ]] || { echo "$BIN is not TernaryShell $VERSION" >&2; exit 1; }
rm -rf "$WORK"
mkdir -p "$WORK" "$OUT"

echo "== file tree"
TREE="$WORK/tree"
install -Dm755 "$BIN" "$TREE/usr/bin/ternaryshell"
install -Dm644 packaging/linux/ternaryshell.desktop "$TREE/usr/share/applications/ternaryshell.desktop"
for icon in packaging/linux/icons/hicolor/*/apps/ternaryshell.png; do
  install -Dm644 "$icon" "$TREE/usr/share/icons/${icon#packaging/linux/icons/}"
done
install -Dm644 README.md "$TREE/usr/share/doc/ternaryshell/README.md"
install -Dm644 LICENSE "$TREE/usr/share/doc/ternaryshell/copyright"

echo "== tar.gz"
# bin/ and share/ at the top, so it unpacks into ~/.local (or /usr/local) as it is.
TAR_DIR="$WORK/ternaryshell-$VERSION"
mkdir -p "$TAR_DIR"
cp -a "$TREE/usr/." "$TAR_DIR/"
tar -C "$WORK" -czf "$OUT/$NAME-$ARCH.tar.gz" "ternaryshell-$VERSION"

echo "== deb"
DEB="$WORK/deb"
cp -a "$TREE" "$DEB"
mkdir -p "$DEB/DEBIAN"
{
  echo "Package: ternaryshell"
  echo "Version: $VERSION"
  echo "Architecture: $DEB_ARCH"
  echo "Maintainer: TernaryShell contributors <ilhamnadhif@users.noreply.github.com>"
  echo "Installed-Size: $(du -sk "$TREE" | cut -f1)"
  # The window system and OpenGL libraries are loaded at run time; desktops have them.
  echo "Depends: libc6 (>= 2.28), libgcc-s1 | libgcc1, libxkbcommon0, libgl1 | libegl1"
  echo "Recommends: xdg-desktop-portal, gnome-keyring | kwalletmanager | keepassxc"
  echo "Section: net"
  echo "Priority: optional"
  echo "Homepage: $REPO"
  echo "Description: $SUMMARY"
  echo "$DESCRIPTION" | sed 's/^/ /'
} > "$DEB/DEBIAN/control"
dpkg-deb --root-owner-group -Zxz --build "$DEB" "$OUT/$NAME-$DEB_ARCH.deb" >/dev/null

echo "== rpm"
RPM="$WORK/rpm"
mkdir -p "$RPM"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
{
  echo "Name: ternaryshell"
  echo "Version: $VERSION"
  echo "Release: 1"
  echo "Summary: $SUMMARY"
  echo "License: MIT"
  echo "URL: $REPO"
  echo "Recommends: xdg-desktop-portal"
  # Prebuilt, already stripped binary: no debug package, no rebuilding.
  echo "%define debug_package %{nil}"
  echo "%define __os_install_post %{nil}"
  echo "%define _build_id_links none"
  echo "%description"
  echo "$DESCRIPTION"
  echo "%install"
  echo "mkdir -p %{buildroot}"
  echo "cp -a $TREE/. %{buildroot}/"
  echo "%files"
  echo "/usr/bin/ternaryshell"
  echo "/usr/share/applications/ternaryshell.desktop"
  echo "/usr/share/icons/hicolor/*/apps/ternaryshell.png"
  echo "%doc /usr/share/doc/ternaryshell/README.md"
  echo "%license /usr/share/doc/ternaryshell/copyright"
} > "$RPM/SPECS/ternaryshell.spec"
rpmbuild -bb --quiet --define "_topdir $RPM" --target "$ARCH" "$RPM/SPECS/ternaryshell.spec"
cp "$RPM/RPMS/$ARCH/ternaryshell-$VERSION-1.$ARCH.rpm" "$OUT/$NAME-$ARCH.rpm"

echo "== AppImage"
APPDIR="$WORK/TernaryShell.AppDir"
cp -a "$TREE" "$APPDIR"
ln -s usr/bin/ternaryshell "$APPDIR/AppRun"
cp packaging/linux/ternaryshell.desktop "$APPDIR/ternaryshell.desktop"
cp packaging/linux/icons/hicolor/256x256/apps/ternaryshell.png "$APPDIR/ternaryshell.png"
TOOL="${APPIMAGETOOL:-$ROOT/target/appimagetool-$ARCH.AppImage}"
if [[ ! -x "$TOOL" ]]; then
  curl --proto '=https' --tlsv1.2 -fsSL -o "$TOOL" \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
  chmod +x "$TOOL"
fi
# Extract-and-run: CI machines have no FUSE.
ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$OUT/$NAME-$ARCH.AppImage" >/dev/null

ls -la "$OUT"/"$NAME"-*
