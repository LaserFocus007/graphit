#!/bin/sh
# Baut Graphit und installiert es für den aktuellen Benutzer (kein sudo nötig).
set -e
cd "$(dirname "$0")"
. "$HOME/.cargo/env"
cargo build --release
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor"
mkdir -p "$HOME/.local/bin" "$APPS" "$ICONS/scalable/apps"
install -m 755 target/release/graphit "$HOME/.local/bin/graphit"
install -m 644 data/io.github.graphit.svg "$ICONS/scalable/apps/io.github.graphit.svg"
cat > "$APPS/io.github.graphit.desktop" <<DESK
[Desktop Entry]
Name=Graphit
Comment=Handschriftliche Notizen und PDF-Annotation (kompatibel mit Xournal++)
Exec=$HOME/.local/bin/graphit %F
Icon=io.github.graphit
Terminal=false
Type=Application
Categories=Office;Education;Graphics;
MimeType=application/x-xopp;application/pdf;
StartupWMClass=io.github.graphit
DESK
update-desktop-database "$APPS" 2>/dev/null || true
gtk-update-icon-cache -q -t "$ICONS" 2>/dev/null || true
echo "Graphit installiert: $HOME/.local/bin/graphit"
