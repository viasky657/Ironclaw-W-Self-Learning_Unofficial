# Audio sandbox container image for IronClaw.
#
# Provides a secure, isolated environment for audio I/O:
#   - Speech-to-Text (STT) via whisper.cpp (local) or OpenAI Whisper API
#   - Text-to-Speech (TTS) via Piper (local) or OpenAI TTS API
#   - Audio capture via PulseAudio loopback (no direct hardware access)
#   - Audio playback via PulseAudio virtual sink → host speakers
#
# Security properties:
#   - Non-root UID 1000 (worker)
#   - Read-only root filesystem with explicit tmpfs for /tmp and /run/pulse
#   - No host display socket mount
#   - No host clipboard bridge
#   - PulseAudio loopback only — container cannot enumerate host audio devices
#   - Network proxied through domain allowlist (HTTP_PROXY / HTTPS_PROXY)
#
# Build:
#   docker build -f ironclaw/docker/audio-sandbox.Dockerfile \
#                -t ironclaw-audio:latest ironclaw/
#
# Environment variables (injected at runtime by AudioSandboxManager):
#   STT_BACKEND       — "whisper_local" | "whisper_api" | "chat_completions"
#   TTS_BACKEND       — "piper" | "espeak" | "openai_tts" | "chat_completions_tts"
#   WHISPER_MODEL     — Whisper model size: "tiny" | "base" | "small" | "medium"
#   PIPER_VOICE       — Piper voice name, e.g. "en_US-lessac-medium"
#   STT_MODEL         — API STT model name (default: "whisper-1")
#   TTS_MODEL         — API TTS model name (default: "tts-1")
#   TTS_VOICE         — API TTS voice (default: "alloy")
#   HTTP_PROXY        — Injected by AudioSandboxManager (domain allowlist proxy)
#   HTTPS_PROXY       — Injected by AudioSandboxManager (domain allowlist proxy)

# ── Stage 1: Build whisper.cpp ────────────────────────────────────────────────
FROM ubuntu:24.04 AS whisper-build

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    cmake \
    git \
    libopenblas-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Clone and build whisper.cpp (CPU-only, no CUDA dependency).
RUN git clone --depth 1 --branch v1.7.2 \
    https://github.com/ggerganov/whisper.cpp.git && \
    cd whisper.cpp && \
    cmake -B build \
          -DWHISPER_BUILD_TESTS=OFF \
          -DWHISPER_BUILD_EXAMPLES=ON \
          -DGGML_OPENBLAS=ON \
          -DCMAKE_BUILD_TYPE=Release && \
    cmake --build build --config Release -j"$(nproc)" && \
    cp build/bin/whisper-cli /usr/local/bin/whisper-cli

# Download Whisper models (base and tiny for fast local inference).
RUN cd whisper.cpp && \
    bash models/download-ggml-model.sh base && \
    bash models/download-ggml-model.sh tiny && \
    mkdir -p /usr/local/share/whisper && \
    cp models/ggml-base.bin models/ggml-tiny.bin /usr/local/share/whisper/

# ── Stage 2: Download Piper TTS ───────────────────────────────────────────────
FROM ubuntu:24.04 AS piper-build

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /piper

# Download Piper binary and default voice (en_US-lessac-medium).
RUN PIPER_VERSION="2023.11.14-2" && \
    ARCH="$(dpkg --print-architecture)" && \
    curl -fsSL \
      "https://github.com/rhasspy/piper/releases/download/${PIPER_VERSION}/piper_linux_${ARCH}.tar.gz" \
      -o piper.tar.gz && \
    tar -xzf piper.tar.gz && \
    mv piper/piper /usr/local/bin/piper && \
    chmod +x /usr/local/bin/piper

# Download default voice model.
RUN mkdir -p /usr/local/share/piper/voices && \
    VOICE="en_US-lessac-medium" && \
    BASE_URL="https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium" && \
    curl -fsSL "${BASE_URL}/${VOICE}.onnx" \
         -o "/usr/local/share/piper/voices/${VOICE}.onnx" && \
    curl -fsSL "${BASE_URL}/${VOICE}.onnx.json" \
         -o "/usr/local/share/piper/voices/${VOICE}.onnx.json"

# ── Stage 3: Final image ──────────────────────────────────────────────────────
FROM ubuntu:24.04

# Install runtime dependencies.
RUN apt-get update && apt-get install -y --no-install-recommends \
    # Audio stack
    pulseaudio \
    pulseaudio-utils \
    alsa-utils \
    sox \
    ffmpeg \
    # espeak-ng fallback TTS
    espeak-ng \
    # Python for transcription/TTS API scripts
    python3 \
    python3-pip \
    python3-venv \
    # Utilities
    curl \
    ca-certificates \
    jq \
    base64 \
    libopenblas0 \
    && rm -rf /var/lib/apt/lists/*

# Install Python dependencies for API-based STT/TTS.
RUN python3 -m venv /opt/audio-venv && \
    /opt/audio-venv/bin/pip install --no-cache-dir \
        openai==1.* \
        requests==2.*

# Copy whisper.cpp binary and models from build stage.
COPY --from=whisper-build /usr/local/bin/whisper-cli /usr/local/bin/whisper-cli
COPY --from=whisper-build /usr/local/share/whisper /usr/local/share/whisper

# Copy Piper binary and voices from build stage.
COPY --from=piper-build /usr/local/bin/piper /usr/local/bin/piper
COPY --from=piper-build /usr/local/share/piper /usr/local/share/piper

# Copy audio scripts.
COPY docker/audio-entrypoint.sh /usr/local/bin/audio-entrypoint.sh
COPY docker/audio-record.sh /usr/local/bin/audio-record.sh
COPY docker/audio-transcribe.sh /usr/local/bin/audio-transcribe.sh
COPY docker/audio-speak.sh /usr/local/bin/audio-speak.sh
COPY docker/audio-transcribe-api.py /usr/local/bin/audio-transcribe-api.py
COPY docker/audio-speak-api.py /usr/local/bin/audio-speak-api.py

RUN chmod +x \
    /usr/local/bin/audio-entrypoint.sh \
    /usr/local/bin/audio-record.sh \
    /usr/local/bin/audio-transcribe.sh \
    /usr/local/bin/audio-speak.sh

# Create non-root worker user (UID 1000).
RUN groupadd -g 1000 worker && \
    useradd -u 1000 -g worker -m -s /bin/bash worker && \
    # Add worker to audio group for PulseAudio access.
    usermod -aG audio worker

# Create audio workspace directory.
RUN mkdir -p /audio-workspace && chown worker:worker /audio-workspace

# PulseAudio configuration: loopback-only, no network, no hardware enumeration.
RUN mkdir -p /etc/pulse && cat > /etc/pulse/client.conf <<'EOF'
# PulseAudio client configuration for audio sandbox.
# Connect to the local daemon only (no network).
default-server = unix:/run/pulse/native
autospawn = no
daemon-binary = /usr/bin/pulseaudio
EOF

RUN cat > /etc/pulse/daemon.conf <<'EOF'
# PulseAudio daemon configuration for audio sandbox.
# Loopback-only: no hardware device enumeration.
daemonize = no
allow-module-loading = no
allow-exit = no
use-pid-file = no
system-instance = no
local-server-type = user
log-target = stderr
log-level = warning
resample-method = speex-float-1
avoid-resampling = false
enable-remixing = yes
enable-lfe-remixing = no
flat-volumes = no
EOF

# Switch to non-root user.
USER worker
WORKDIR /audio-workspace

# Default environment.
ENV STT_BACKEND=whisper_local \
    TTS_BACKEND=piper \
    WHISPER_MODEL=base \
    PIPER_VOICE=en_US-lessac-medium \
    STT_MODEL=whisper-1 \
    TTS_MODEL=tts-1 \
    TTS_VOICE=alloy \
    WHISPER_MODELS_DIR=/usr/local/share/whisper \
    PIPER_VOICES_DIR=/usr/local/share/piper/voices \
    AUDIO_VENV=/opt/audio-venv \
    PATH="/opt/audio-venv/bin:$PATH"

ENTRYPOINT ["/usr/local/bin/audio-entrypoint.sh"]
