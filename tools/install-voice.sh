#!/bin/sh
# Installs Piper and natural voices for reading aloud in Graphit.
# Usage: install-voice.sh [all|en|de] (default: all)
# Everything goes to ~/.local/share/graphit/piper, sudo is not needed.
# About 140 MB per language, 250 MB for both.
set -e
case "${1:-all}" in
    all) LANGS="de en" ;;
    en|de) LANGS="$1" ;;
    *) echo "Usage: $0 [all|en|de]" >&2; exit 1 ;;
esac
BASE=https://huggingface.co/rhasspy/piper-voices/resolve/main
DIR="$HOME/.local/share/graphit/piper"
mkdir -p "$DIR"
cd "$DIR"
# Download to a temporary name first, so an interrupted download never counts as installed
get() {
    if command -v curl >/dev/null; then curl -fsSL -o "$2.part" "$1"; else wget -q -O "$2.part" "$1"; fi
    mv "$2.part" "$2"
}
if [ ! -x piper/piper ]; then
    get https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_linux_x86_64.tar.gz piper.tar.gz
    tar xzf piper.tar.gz
    rm piper.tar.gz
fi
for L in $LANGS; do
    case "$L" in
        de) VOICE=de_DE-thorsten-high; PATH_=de/de_DE/thorsten/high ;;
        en) VOICE=en_US-lessac-high; PATH_=en/en_US/lessac/high ;;
    esac
    [ -f "$VOICE.onnx.json" ] || get "$BASE/$PATH_/$VOICE.onnx.json" "$VOICE.onnx.json"
    [ -f "$VOICE.onnx" ] || get "$BASE/$PATH_/$VOICE.onnx" "$VOICE.onnx"
done
echo "Done. Graphit now reads aloud with natural voices."
