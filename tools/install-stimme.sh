#!/bin/sh
# Installiert Piper und die deutsche Stimme "thorsten" für das Vorlesen in Graphit.
# Alles landet in ~/.local/share/graphit/piper, sudo ist nicht nötig. Rund 90 MB.
set -e
DIR="$HOME/.local/share/graphit/piper"
mkdir -p "$DIR"
cd "$DIR"
curl -fL -o piper.tar.gz https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_linux_x86_64.tar.gz
tar xzf piper.tar.gz
rm piper.tar.gz
BASE=https://huggingface.co/rhasspy/piper-voices/resolve/main/de/de_DE/thorsten/medium
curl -fL -o de_DE-thorsten-medium.onnx "$BASE/de_DE-thorsten-medium.onnx"
curl -fL -o de_DE-thorsten-medium.onnx.json "$BASE/de_DE-thorsten-medium.onnx.json"
echo "Fertig. Graphit liest ab jetzt mit der neuen Stimme vor."
