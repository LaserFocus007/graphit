#!/bin/sh
# Builds a portable Windows folder dist/Graphit (program plus GTK runtime) and zips it.
# Run inside an MSYS2 UCRT64 shell with these packages installed:
#   pacman -S mingw-w64-ucrt-x86_64-{gtk4,libadwaita,poppler,rust,pkgconf,gcc,adwaita-icon-theme} zip
set -e
cd "$(dirname "$0")/../.."
PREFIX=/ucrt64
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# No local paths inside the program, like the .deb build
export CARGO_TARGET_DIR="$PWD/target/package"
export RUSTFLAGS="--remap-path-prefix=$HOME=/build --remap-path-prefix=$PWD=/graphit"
cargo build --release
OUT=dist/Graphit
rm -rf dist
mkdir -p "$OUT/bin" "$OUT/lib" "$OUT/share/glib-2.0/schemas" "$OUT/share/icons" "$OUT/share/graphit"
cp target/package/release/graphit.exe "$OUT/bin/"
strip "$OUT/bin/graphit.exe"
# Image loaders, then every DLL that the program and the loaders pull in from MSYS2
cp -r "$PREFIX/lib/gdk-pixbuf-2.0" "$OUT/lib/"
deps() {
    ldd "$@" | awk -v p="$PREFIX/" 'index($3, p) == 1 { print $3 }' | sort -u
}
for dll in $(deps "$OUT/bin/graphit.exe" "$OUT"/lib/gdk-pixbuf-2.0/2.10.0/loaders/*.dll); do
    cp -n "$dll" "$OUT/bin/"
done
# Settings schemas, icons, fonts configuration and GTK's own translations
cp "$PREFIX"/share/glib-2.0/schemas/*.xml "$OUT/share/glib-2.0/schemas/"
glib-compile-schemas "$OUT/share/glib-2.0/schemas"
cp -r "$PREFIX/share/icons/Adwaita" "$PREFIX/share/icons/hicolor" "$OUT/share/icons/"
mkdir -p "$OUT/share/icons/hicolor/scalable/apps"
cp data/io.github.graphit.svg "$OUT/share/icons/hicolor/scalable/apps/"
[ -d "$PREFIX/etc/fonts" ] && mkdir -p "$OUT/etc" && cp -r "$PREFIX/etc/fonts" "$OUT/etc/"
[ -d "$PREFIX/share/poppler" ] && cp -r "$PREFIX/share/poppler" "$OUT/share/"
for lang in de; do
    for domain in gtk40 libadwaita glib20; do
        mo="$PREFIX/share/locale/$lang/LC_MESSAGES/$domain.mo"
        [ -f "$mo" ] && install -D -m644 "$mo" "$OUT/share/locale/$lang/LC_MESSAGES/$domain.mo"
    done
done
cp tools/install-voice.ps1 "$OUT/share/graphit/"
cp LICENSE "$OUT/LICENSE.txt"
(cd dist && zip -qr "graphit-$VERSION-windows-x64.zip" Graphit)
echo "Done: dist/graphit-$VERSION-windows-x64.zip"
