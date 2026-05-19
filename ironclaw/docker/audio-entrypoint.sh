#!/usr/bin/env bash
# Audio sandbox container entrypoint.
#
# Starts PulseAudio with a loopback virtual device, then keeps the container
# alive for `docker exec` commands from AudioSandboxManager.
#
# Security notes:
#   - PulseAudio is started in user mode (no system instance).
#   - The virtual sink/source is a loopback — no hardware device enumeration.
#   - The container runs as UID 1000 (worker), not root.
#   - SIGTERM/SIGINT are handled for clean shutdown.

set -euo pipefail

# ── Signal handling ───────────────────────────────────────────────────────────

cleanup() {
    echo "[audio-entrypoint] Received shutdown signal, cleaning up..."
    if [ -n "${PULSE_PID:-}" ] && kill -0 "$PULSE_PID" 2>/dev/null; then
        kill "$PULSE_PID" 2>/dev/null || true
        wait "$PULSE_PID" 2>/dev/null || true
    fi
    echo "[audio-entrypoint] Shutdown complete."
    exit 0
}

trap cleanup SIGTERM SIGINT

# ── PulseAudio setup ──────────────────────────────────────────────────────────

echo "[audio-entrypoint] Starting PulseAudio (loopback virtual device)..."

# Ensure the PulseAudio socket directory exists.
mkdir -p /run/pulse

# Start PulseAudio in user mode with loopback module.
# --load: load the null sink (virtual output) and null source (virtual input).
# --exit-idle-time=-1: keep running even when idle.
pulseaudio \
    --start \
    --exit-idle-time=-1 \
    --log-target=stderr \
    --log-level=warning \
    --load="module-null-sink sink_name=virtual_out sink_properties=device.description=VirtualOutput" \
    --load="module-null-source source_name=virtual_in source_properties=device.description=VirtualInput" \
    --load="module-loopback source=virtual_in.monitor sink=virtual_out" \
    2>&1 | sed 's/^/[pulseaudio] /' &

PULSE_PID=$!

# Wait for PulseAudio socket to be ready.
PULSE_SOCKET="/run/pulse/native"
MAX_WAIT=10
WAITED=0
while [ ! -S "$PULSE_SOCKET" ] && [ "$WAITED" -lt "$MAX_WAIT" ]; do
    sleep 0.5
    WAITED=$((WAITED + 1))
done

if [ ! -S "$PULSE_SOCKET" ]; then
    echo "[audio-entrypoint] WARNING: PulseAudio socket not ready after ${MAX_WAIT}s" >&2
    echo "[audio-entrypoint] Continuing anyway — API-based STT/TTS will still work." >&2
else
    echo "[audio-entrypoint] PulseAudio ready (socket: $PULSE_SOCKET)"
fi

# Set default sink/source for paplay/parec.
export PULSE_SERVER="unix:${PULSE_SOCKET}"

# ── Ready ─────────────────────────────────────────────────────────────────────

echo "[audio-entrypoint] Audio sandbox ready."
echo "[audio-entrypoint] STT_BACKEND=${STT_BACKEND:-whisper_local}"
echo "[audio-entrypoint] TTS_BACKEND=${TTS_BACKEND:-piper}"
echo "[audio-entrypoint] WHISPER_MODEL=${WHISPER_MODEL:-base}"
echo "[audio-entrypoint] PIPER_VOICE=${PIPER_VOICE:-en_US-lessac-medium}"

# Keep the container alive for `docker exec` commands.
# Sleep in a loop so SIGTERM is handled promptly.
while true; do
    sleep 60 &
    wait $!
done
