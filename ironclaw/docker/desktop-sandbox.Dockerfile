# Desktop Sandbox Image — Xvfb virtual display + accessibility tools
#
# Architecture:
#   Host
#     └── Docker container (this image)
#           ├── Xvfb :99  — virtual framebuffer, NO connection to host DISPLAY
#           ├── fluxbox   — minimal window manager (needed for app layout)
#           ├── x11vnc    — pixel stream out (read-only view, never exposes X socket to host)
#           ├── at-spi2   — accessibility bus (structured UI state → JSON, not raw X events)
#           ├── xdotool   — input injection (mouse/keyboard) inside the virtual display only
#           └── scrot     — screenshot tool (captures Xvfb framebuffer, not host screen)
#
# Security properties:
#   - DISPLAY=:99 is a virtual X server with NO connection to the host display server.
#   - The container filesystem is isolated from the host (no host mounts beyond /workspace).
#   - Network traffic is proxied through the domain allowlist (http_proxy env var).
#   - Clipboard is NOT shared with the host (no xclip/xsel host bridge).
#   - Container runs as non-root UID 1000 (worker).
#   - No privileged mode; capabilities are dropped to the minimum required.
#
# Residual risks (documented for operator awareness):
#   - The AI sees everything rendered in the virtual display. Do not open documents
#     containing secrets inside the desktop session.
#   - xdotool can inject keystrokes into any window in the virtual display, including
#     password fields.
#
# Build:
#   docker build -f docker/desktop-sandbox.Dockerfile -t ironclaw-desktop:latest .
#
# The entrypoint starts Xvfb and fluxbox, then keeps the container alive.
# Individual commands are run via `docker exec` by the DesktopSandboxManager.

FROM ubuntu:24.04

# Prevent interactive prompts during package installation
ENV DEBIAN_FRONTEND=noninteractive
ENV TZ=UTC

# ── System packages ──────────────────────────────────────────────────────────
RUN apt-get update && apt-get install -y --no-install-recommends \
    # Virtual display
    xvfb \
    # Minimal window manager (required for proper app window placement)
    fluxbox \
    # VNC server — streams pixels out, never exposes raw X socket to host
    x11vnc \
    # Accessibility bus (AT-SPI2) — structured UI state queries
    at-spi2-core \
    libatk-adaptor \
    # Input injection (mouse + keyboard events inside the virtual display)
    xdotool \
    # Screenshot tool (captures Xvfb framebuffer)
    scrot \
    # Image processing utilities (used by screenshot pipeline)
    imagemagick \
    # OCR engine (used by credential redaction pipeline to locate text regions)
    tesseract-ocr \
    tesseract-ocr-eng \
    # Python AT-SPI bindings for accessibility tree queries
    python3 \
    python3-pyatspi \
    # Common desktop apps available inside the sandbox
    firefox \
    libreoffice \
    gedit \
    # Fonts (required for readable screenshots)
    fonts-liberation \
    fonts-dejavu-core \
    # Utilities
    curl \
    wget \
    ca-certificates \
    procps \
    && rm -rf /var/lib/apt/lists/*

# ── Non-root user ─────────────────────────────────────────────────────────────
RUN groupadd -g 1000 worker && useradd -u 1000 -g worker -m -s /bin/bash worker

# ── Workspace directory ───────────────────────────────────────────────────────
RUN mkdir -p /workspace && chown worker:worker /workspace

# ── Virtual display configuration ────────────────────────────────────────────
# DISPLAY :99 is the virtual X server — completely isolated from the host.
# The host DISPLAY (typically :0 or :1) is never set or accessible inside
# this container.
ENV DISPLAY=:99
ENV SCREEN_WIDTH=1920
ENV SCREEN_HEIGHT=1080
ENV SCREEN_DEPTH=24

# ── Accessibility bus ─────────────────────────────────────────────────────────
# Required for AT-SPI2 to function inside the container.
ENV NO_AT_BRIDGE=0
ENV DBUS_SESSION_BUS_ADDRESS=autolaunch:

# ── Entrypoint script ─────────────────────────────────────────────────────────
COPY docker/desktop-entrypoint.sh /usr/local/bin/desktop-entrypoint.sh
RUN chmod +x /usr/local/bin/desktop-entrypoint.sh

# ── Accessibility tree query script ──────────────────────────────────────────
# Queries AT-SPI2 and outputs structured JSON — the AI never gets raw X11 access.
COPY docker/desktop-accessibility-query.py /usr/local/bin/desktop-accessibility-query.py
RUN chmod +x /usr/local/bin/desktop-accessibility-query.py

# ── Screenshot credential redaction script ────────────────────────────────────
# Uses tesseract OCR + imagemagick to black out hidden credential values in
# screenshots before they are returned to the AI.
COPY docker/desktop-redact-screenshot.py /usr/local/bin/desktop-redact-screenshot.py
RUN chmod +x /usr/local/bin/desktop-redact-screenshot.py

USER worker
WORKDIR /workspace

# The container stays alive; commands are run via `docker exec`.
ENTRYPOINT ["/usr/local/bin/desktop-entrypoint.sh"]
