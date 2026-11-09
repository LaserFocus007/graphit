#!/bin/sh
# Installs Piper and a natural voice for reading aloud in Graphit.
# Usage: install-voice.sh [en|de] (default: en)
# Everything goes to ~/.local/share/graphit/piper, sudo is not needed. About 90 MB.
set -e
case "${1:-en}" in
    en) VOICE=en_US-lessac-medium; BASE=https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium ;;
    de) VOICE=de_DE-thorsten-medium; BASE=https://huggingface.co/rhasspy/piper-voices/resolve/main/de/de_DE/thorsten/medium ;;
    *) echo "Usage: $0 [en|de]" >&2; exit 1 ;;
esac
DIR="$HOME/.local/share/graphit/piper"
mkdir -p "$DIR"
cd "$DIR"
if [ ! -x piper/piper ]; then
    curl -fL -o piper.tar.gz https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_linux_x86_64.tar.gz
    tar xzf piper.tar.gz
    rm piper.tar.gz
fi
curl -fL -o "$VOICE.onnx" "$BASE/$VOICE.onnx"
curl -fL -o "$VOICE.onnx.json" "$BASE/$VOICE.onnx.json"
echo "Done. Graphit now reads aloud with the new voice."
