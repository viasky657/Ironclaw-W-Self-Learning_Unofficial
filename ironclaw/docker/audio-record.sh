#!/usr/bin/env bash
# Record audio from the PulseAudio virtual source for a specified duration.
#
# Usage: audio-record.sh <duration_secs> <output_wav>
#
# Captures from the PulseAudio virtual source (virtual_in) using parec,
# then converts to a standard 16kHz mono WAV file suitable for Whisper.
#
# Security notes:
#   - Captures from the virtual PulseAudio source only (no hardware access).
#   - Output is written to /tmp only (tmpfs, not persisted).
#   - Duration is capped at 120 seconds by the caller (AudioSandboxManager).

set -euo pipefail

DURATION_SECS="${1:?Usage: audio-record.sh <duration_secs> <output_wav>}"
OUTPUT_WAV="${2:?Usage: audio-record.sh <duration_secs> <output_wav>}"

# Validate duration is a positive integer.
if ! [[ "$DURATION_SECS" =~ ^[0-9]+$ ]] || [ "$DURATION_SECS" -lt 1 ] || [ "$DURATION_SECS" -gt 120 ]; then
    echo "ERROR: duration_secs must be between 1 and 120 (got: $DURATION_SECS)" >&2
    exit 1
fi

# Validate output path is under /tmp.
case "$OUTPUT_WAV" in
    /tmp/*) ;;
    *) echo "ERROR: output path must be under /tmp (got: $OUTPUT_WAV)" >&2; exit 1 ;;
esac

PULSE_SERVER="${PULSE_SERVER:-unix:/run/pulse/native}"
RAW_PCM="/tmp/recording_raw_$$.pcm"

echo "[audio-record] Recording ${DURATION_SECS}s from PulseAudio virtual source..." >&2

# Record raw PCM from PulseAudio virtual source.
# Format: 16-bit signed little-endian, 16kHz, mono (optimal for Whisper).
timeout "$((DURATION_SECS + 5))" parec \
    --server="$PULSE_SERVER" \
    --source=virtual_in \
    --format=s16le \
    --rate=16000 \
    --channels=1 \
    --latency-msec=100 \
    --record \
    --raw \
    --file-format=raw \
    "$RAW_PCM" &
PAREC_PID=$!

# Let it record for the requested duration.
sleep "$DURATION_SECS"

# Stop recording.
kill "$PAREC_PID" 2>/dev/null || true
wait "$PAREC_PID" 2>/dev/null || true

if [ ! -f "$RAW_PCM" ] || [ ! -s "$RAW_PCM" ]; then
    echo "ERROR: No audio captured (PulseAudio source may not be available)" >&2
    rm -f "$RAW_PCM"
    exit 1
fi

# Convert raw PCM to WAV using sox.
sox \
    --type raw \
    --rate 16000 \
    --encoding signed-integer \
    --bits 16 \
    --channels 1 \
    "$RAW_PCM" \
    --type wav \
    --rate 16000 \
    --encoding signed-integer \
    --bits 16 \
    --channels 1 \
    "$OUTPUT_WAV"

rm -f "$RAW_PCM"

FILESIZE=$(stat -c%s "$OUTPUT_WAV" 2>/dev/null || echo 0)
echo "[audio-record] Recorded ${DURATION_SECS}s → ${OUTPUT_WAV} (${FILESIZE} bytes)" >&2
