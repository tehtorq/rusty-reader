#!/usr/bin/env bash
# Install Piper, a natural-sounding offline voice, for reading aloud on the desktop.
#
#   scripts/install-piper.sh                       # Piper, plus a British and an American voice
#   scripts/install-piper.sh en_US-ryan-high ...   # Piper, plus the voices named
#
# Voices are listed at https://huggingface.co/rhasspy/piper-voices (and can be heard at
# https://rhasspy.github.io/piper-samples). Everything goes in ~/.local/share/rusty-reader/piper;
# delete that folder to remove it again.
set -euo pipefail

DIR="${XDG_DATA_HOME:-$HOME/.local/share}/rusty-reader/piper"
RELEASE="https://github.com/rhasspy/piper/releases/download/2023.11.14-2"
VOICES="https://huggingface.co/rhasspy/piper-voices/resolve/main"

case "$(uname -m)" in
    x86_64) ARCH=x86_64 ;;
    aarch64 | arm64) ARCH=aarch64 ;;
    *) echo "No Piper build for $(uname -m)" >&2; exit 1 ;;
esac

mkdir -p "$DIR/voices"
if [ ! -x "$DIR/piper/piper" ]; then
    echo "Downloading Piper…"
    curl -fL# "$RELEASE/piper_linux_$ARCH.tar.gz" | tar -xz -C "$DIR"
fi

voices=("$@")
[ ${#voices[@]} -eq 0 ] && voices=(en_GB-alan-medium en_US-lessac-medium)
for voice in "${voices[@]}"; do
    # en_GB-alan-medium lives at en/en_GB/alan/medium/.
    IFS=- read -r locale name quality <<<"$voice"
    path="${locale%%_*}/$locale/$name/$quality/$voice"
    for ext in onnx onnx.json; do
        if [ ! -s "$DIR/voices/$voice.$ext" ]; then
            echo "Downloading $voice.$ext…"
            curl -fL# -o "$DIR/voices/$voice.$ext.part" "$VOICES/$path.$ext"
            mv "$DIR/voices/$voice.$ext.part" "$DIR/voices/$voice.$ext"
        fi
    done
done

echo "Piper is ready in $DIR"
