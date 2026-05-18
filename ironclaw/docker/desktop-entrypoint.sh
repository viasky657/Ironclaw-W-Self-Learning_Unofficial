#!/bin/bash
# Desktop sandbox entrypoint.
#
# Starts Xvfb (virtual framebuffer) and fluxbox (window manager), then keeps
# the container alive so that `docker exec` commands can interact with the
# virtual display.
#
# Security notes:
#   - DISPLAY=:99 is a virtual X server with NO connection to the host display.
#   - The host DISPLAY is never set or accessible inside this container.
#   - Clipboard is NOT bridged to the host.
#   - This script runs as non-root UID 1000 (worker).

set -euo pipefail

DISPLAY_NUM="${DISPLAY_NUM:-:99}"
SCREEN_WIDTH="${SCREEN_WIDTH:-1920}"
SCREEN_HEIGHT="${SCREEN_HEIGHT:-1080}"
SCREEN_DEPTH="${SCREEN_DEPTH:-24}"

export DISPLAY="${DISPLAY_NUM}"

echo "[desktop-entrypoint] Starting Xvfb on display ${DISPLAY_NUM} (${SCREEN_WIDTH}x${SCREEN_HEIGHT}x${SCREEN_DEPTH})"

# Start Xvfb — virtual framebuffer, no connection to host display server.
Xvfb "${DISPLAY_NUM}" \
    -screen 0 "${SCREEN_WIDTH}x${SCREEN_HEIGHT}x${SCREEN_DEPTH}" \
    -ac \
    -nolisten tcp \
    -nolisten unix \
    +extension RANDR \
    &
XVFB_PID=$!

# Wait for Xvfb to be ready (up to 10 seconds).
for i in $(seq 1 20); do
    if xdpyinfo -display "${DISPLAY_NUM}" >/dev/null 2>&1; then
        echo "[desktop-entrypoint] Xvfb ready on ${DISPLAY_NUM}"
        break
    fi
    sleep 0.5
done

# Start D-Bus session bus (required for AT-SPI2 accessibility).
if command -v dbus-launch >/dev/null 2>&1; then
    eval "$(dbus-launch --sh-syntax)"
    export DBUS_SESSION_BUS_ADDRESS
    echo "[desktop-entrypoint] D-Bus session started: ${DBUS_SESSION_BUS_ADDRESS}"
fi

# Start AT-SPI2 accessibility registry daemon.
if command -v /usr/lib/at-spi2-core/at-spi-bus-launcher >/dev/null 2>&1; then
    /usr/lib/at-spi2-core/at-spi-bus-launcher --launch-immediately &
    echo "[desktop-entrypoint] AT-SPI2 bus launcher started"
fi

# Start fluxbox window manager (required for proper app window placement).
fluxbox -display "${DISPLAY_NUM}" >/dev/null 2>&1 &
FLUXBOX_PID=$!
echo "[desktop-entrypoint] fluxbox started (PID ${FLUXBOX_PID})"

echo "[desktop-entrypoint] Desktop sandbox ready. Container will stay alive for docker exec commands."

# Keep the container alive. Trap SIGTERM/SIGINT for clean shutdown.
cleanup() {
    echo "[desktop-entrypoint] Shutting down..."
    kill "${FLUXBOX_PID}" 2>/dev/null || true
    kill "${XVFB_PID}" 2>/dev/null || true
    exit 0
}
trap cleanup SIGTERM SIGINT

# Wait indefinitely (the container is managed by DesktopSandboxManager via docker exec).
wait "${XVFB_PID}"
