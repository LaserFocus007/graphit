#!/bin/sh
# Baut graphit_<version>_amd64.deb. Voraussetzungen zum Bauen:
#   sudo apt install build-essential dpkg-dev libgtk-4-dev libadwaita-1-dev libpoppler-glib-dev
set -e
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# Eigener Build ohne lokale Pfade im Programm, damit kein Benutzername mit ausgeliefert wird
export CARGO_TARGET_DIR="$PWD/target/package"
export RUSTFLAGS="--remap-path-prefix=$HOME/.cargo/registry/src=/cargo --remap-path-prefix=$PWD=/graphit --remap-path-prefix=$HOME=/build"
cargo build --release
ROOT=target/package/deb
rm -rf "$ROOT"
install -Dm755 target/package/release/graphit "$ROOT/usr/bin/graphit"
strip "$ROOT/usr/bin/graphit"
install -Dm644 data/io.github.graphit.desktop "$ROOT/usr/share/applications/io.github.graphit.desktop"
install -Dm644 data/io.github.graphit.svg "$ROOT/usr/share/icons/hicolor/scalable/apps/io.github.graphit.svg"
install -Dm644 data/io.github.graphit.mime.xml "$ROOT/usr/share/mime/packages/io.github.graphit.xml"
install -Dm755 tools/install-stimme.sh "$ROOT/usr/share/graphit/install-stimme.sh"
install -Dm644 LICENSE "$ROOT/usr/share/doc/graphit/copyright"
# Abhängigkeiten aus dem fertigen Programm ableiten
WORK=$(mktemp -d)
mkdir "$WORK/debian"
: > "$WORK/debian/control"
BIN="$PWD/$ROOT/usr/bin/graphit"
DEPENDS=$(cd "$WORK" && dpkg-shlibdeps -O "$BIN" 2>/dev/null | sed 's/^shlibs:Depends=//')
rm -rf "$WORK"
SIZE=$(du -sk "$ROOT" | cut -f1)
mkdir -p "$ROOT/DEBIAN"
cat > "$ROOT/DEBIAN/control" <<CONTROL
Package: graphit
Version: $VERSION
Section: graphics
Priority: optional
Architecture: amd64
Maintainer: Graphit <graphit@users.noreply.github.com>
Installed-Size: $SIZE
Depends: $DEPENDS
Recommends: poppler-utils, pandoc, texlive-latex-base, texlive-latex-recommended, texlive-fonts-recommended, alsa-utils
Suggests: speech-dispatcher
Description: Handschriftliche Notizen und PDF-Annotation
 Notiz-App für Stift und Touchscreen, kompatibel mit Xournal++-Dateien.
 Optional mit Claude Code als Lernhilfe direkt auf der Seite.
CONTROL
OUT="graphit_${VERSION}_amd64.deb"
dpkg-deb --root-owner-group --build "$ROOT" "$OUT"
echo "Fertig: $OUT"
