#!/usr/bin/env bash
# Synthesize speech from a text file and play it through PulseAudio.
#
# Usage: audio-speak.sh <input_txt> <voice>
#
# Backends (selected via TTS_BACKEND env var):
#   piper             — Piper local TTS (default, high quality)
#   espeak            — espeak-ng local TTS (fallback, lower quality)
#   openai_tts        — OpenAI TTS API (requires API key via proxy)
#   chat_completions_tts — OpenAI-compatible TTS via chat completions
#
# Security notes:
#   - Input must be under /tmp.
#   - API keys are never passed as CLI arguments; they are read from env vars
#     injected by the proxy (HTTP_PROXY / HTTPS_PROXY).
#   - Audio is played through the PulseAudio virtual sink only.

set -euo pipefail

INPUT_TXT="${1:?Usage: audio-speak.sh <input_txt> <voice>}"
VOICE="${2:-}"

# Validate input path is under /tmp.
case "$INPUT_TXT" in
    /tmp/*) ;;
    *) echo "ERROR: input path must be under /tmp (got: $INPUT_TXT)" >&2; exit 1 ;;
esac

if [ ! -f "$INPUT_TXT" ]; then
    echo "ERROR: input file not found: $INPUT_TXT" >&2
    exit 1
fi

TTS_BACKEND="${TTS_BACKEND:-piper}"
PIPER_VOICE="${VOICE:-${PIPER_VOICE:-en_US-lessac-medium}}"
PIPER_VOICES_DIR="${PIPER_VOICES_DIR:-/usr/local/share/piper/voices}"
PULSE_SERVER="${PULSE_SERVER:-unix:/run/pulse/native}"

TEXT=$(cat "$INPUT_TXT")
CHAR_COUNT=${#TEXT}

echo "[audio-speak] Backend: $TTS_BACKEND, Voice: $PIPER_VOICE, Chars: $CHAR_COUNT" >&2

case "$TTS_BACKEND" in
    piper)
        VOICE_MODEL="${PIPER_VOICES_DIR}/${PIPER_VOICE}.onnx"
        if [ ! -f "$VOICE_MODEL" ]; then
            echo "WARNING: Piper voice model not found: $VOICE_MODEL" >&2
            echo "Falling back to espeak-ng..." >&2
            TTS_BACKEND="espeak"
        fi
        ;;
esac

case "$TTS_BACKEND" in
    piper)
        VOICE_MODEL="${PIPER_VOICES_DIR}/${PIPER_VOICE}.onnx"
        AUDIO_TMP="/tmp/tts_output_$$.wav"

        # Synthesize with Piper.
        echo "$TEXT" | piper \
            --model "$VOICE_MODEL" \
            --output_file "$AUDIO_TMP" \
            2>&1 | sed 's/^/[piper] /' >&2

        if [ ! -f "$AUDIO_TMP" ]; then
            echo "ERROR: Piper did not produce output audio" >&2
            exit 1
        fi

        # Play through PulseAudio virtual sink.
        paplay \
            --server="$PULSE_SERVER" \
            --sink=virtual_out \
            "$AUDIO_TMP" \
            2>&1 | sed 's/^/[paplay] /' >&2

        rm -f "$AUDIO_TMP"
        ;;

    espeak)
        # espeak-ng: synthesize and play directly through PulseAudio.
        ESPEAK_VOICE="${VOICE:-en}"
        espeak-ng \
            --stdout \
            -v "$ESPEAK_VOICE" \
            -s 150 \
            "$TEXT" \
            2>&1 | sed 's/^/[espeak] /' >&2 | \
        paplay \
            --server="$PULSE_SERVER" \
            --sink=virtual_out \
            /dev/stdin \
            2>&1 | sed 's/^/[paplay] /' >&2
        ;;

    openai_tts|chat_completions_tts)
        # Delegate to Python script for API-based TTS.
        "${AUDIO_VENV:-/opt/audio-venv}/bin/python3" \
            /usr/local/bin/audio-speak-api.py \
            "$INPUT_TXT" \
            "$VOICE" \
            "$TTS_BACKEND"
        ;;

    *)
        echo "ERROR: Unknown TTS_BACKEND: $TTS_BACKEND" >&2
        echo "Valid values: piper, espeak, openai_tts, chat_completions_tts" >&2
        exit 1
        ;;
esac

echo "[audio-speak] Playback complete (${CHAR_COUNT} chars)" >&2
