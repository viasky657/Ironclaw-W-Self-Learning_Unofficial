#!/usr/bin/env bash
# Transcribe a WAV file to text using the configured STT backend.
#
# Usage: audio-transcribe.sh <input_wav> <output_json>
#
# Outputs a JSON file with the structure:
#   {"text": "...", "language": "en", "confidence": 0.95}
#
# Backends (selected via STT_BACKEND env var):
#   whisper_local     — whisper.cpp local inference (default)
#   whisper_api       — OpenAI Whisper API (requires API key via proxy)
#   chat_completions  — OpenAI-compatible chat completions with audio input
#
# Security notes:
#   - Input must be under /tmp.
#   - Output must be under /tmp.
#   - API keys are never passed as CLI arguments; they are read from env vars
#     injected by the proxy (HTTP_PROXY / HTTPS_PROXY).

set -euo pipefail

INPUT_WAV="${1:?Usage: audio-transcribe.sh <input_wav> <output_json>}"
OUTPUT_JSON="${2:?Usage: audio-transcribe.sh <input_wav> <output_json>}"

# Validate paths are under /tmp.
case "$INPUT_WAV" in
    /tmp/*) ;;
    *) echo "ERROR: input path must be under /tmp (got: $INPUT_WAV)" >&2; exit 1 ;;
esac
case "$OUTPUT_JSON" in
    /tmp/*) ;;
    *) echo "ERROR: output path must be under /tmp (got: $OUTPUT_JSON)" >&2; exit 1 ;;
esac

if [ ! -f "$INPUT_WAV" ]; then
    echo "ERROR: input file not found: $INPUT_WAV" >&2
    exit 1
fi

STT_BACKEND="${STT_BACKEND:-whisper_local}"
WHISPER_MODEL="${WHISPER_MODEL:-base}"
WHISPER_MODELS_DIR="${WHISPER_MODELS_DIR:-/usr/local/share/whisper}"

echo "[audio-transcribe] Backend: $STT_BACKEND" >&2

case "$STT_BACKEND" in
    whisper_local)
        MODEL_FILE="${WHISPER_MODELS_DIR}/ggml-${WHISPER_MODEL}.bin"
        if [ ! -f "$MODEL_FILE" ]; then
            echo "ERROR: Whisper model not found: $MODEL_FILE" >&2
            echo "Available models:" >&2
            ls "${WHISPER_MODELS_DIR}/"*.bin 2>/dev/null || echo "  (none)" >&2
            exit 1
        fi

        # Run whisper.cpp and capture JSON output.
        WHISPER_TMP="/tmp/whisper_out_$$"
        whisper-cli \
            --model "$MODEL_FILE" \
            --file "$INPUT_WAV" \
            --output-json \
            --output-file "$WHISPER_TMP" \
            --language auto \
            --no-timestamps \
            2>&1 | sed 's/^/[whisper] /' >&2

        # whisper.cpp writes <output_file>.json
        WHISPER_JSON="${WHISPER_TMP}.json"
        if [ ! -f "$WHISPER_JSON" ]; then
            echo "ERROR: whisper.cpp did not produce output JSON" >&2
            exit 1
        fi

        # Extract text and language from whisper.cpp JSON output.
        # whisper.cpp JSON format: {"transcription": [{"text": "...", ...}], ...}
        TEXT=$(jq -r '[.transcription[].text] | join(" ") | ltrimstr(" ")' "$WHISPER_JSON" 2>/dev/null || echo "")
        LANGUAGE=$(jq -r '.result.language // "unknown"' "$WHISPER_JSON" 2>/dev/null || echo "unknown")

        rm -f "$WHISPER_JSON"

        jq -n \
            --arg text "$TEXT" \
            --arg language "$LANGUAGE" \
            '{"text": $text, "language": $language, "confidence": null, "backend": "whisper_local"}' \
            > "$OUTPUT_JSON"
        ;;

    whisper_api|chat_completions)
        # Delegate to Python script for API-based transcription.
        "${AUDIO_VENV:-/opt/audio-venv}/bin/python3" \
            /usr/local/bin/audio-transcribe-api.py \
            "$INPUT_WAV" \
            "$OUTPUT_JSON" \
            "$STT_BACKEND"
        ;;

    *)
        echo "ERROR: Unknown STT_BACKEND: $STT_BACKEND" >&2
        echo "Valid values: whisper_local, whisper_api, chat_completions" >&2
        exit 1
        ;;
esac

# Validate output JSON.
if [ ! -f "$OUTPUT_JSON" ]; then
    echo "ERROR: transcription did not produce output JSON" >&2
    exit 1
fi

TEXT=$(jq -r '.text // ""' "$OUTPUT_JSON" 2>/dev/null || echo "")
echo "[audio-transcribe] Transcript (${#TEXT} chars): ${TEXT:0:80}..." >&2
