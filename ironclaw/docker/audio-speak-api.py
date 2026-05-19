#!/usr/bin/env python3
"""API-based text-to-speech synthesis and playback script.

Used by audio-speak.sh when TTS_BACKEND is 'openai_tts' or
'chat_completions_tts'. Reads API credentials from environment variables
(injected by the domain-allowlist proxy, never passed as CLI args).

Usage:
    audio-speak-api.py <input_txt> <voice> <backend>

Backends:
    openai_tts            — OpenAI TTS API (/v1/audio/speech)
    chat_completions_tts  — OpenAI-compatible chat completions TTS

Security notes:
    - API keys are read from environment variables only (never CLI args).
    - Input path is validated to be under /tmp.
    - The proxy (HTTP_PROXY / HTTPS_PROXY) enforces the domain allowlist.
    - Audio is played through PulseAudio virtual sink (no hardware access).
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
from pathlib import Path


def validate_tmp_path(path: str, name: str) -> Path:
    """Validate that a path is under /tmp."""
    p = Path(path).resolve()
    if not str(p).startswith("/tmp/"):
        print(f"ERROR: {name} must be under /tmp (got: {path})", file=sys.stderr)
        sys.exit(1)
    return p


def play_audio(audio_path: Path) -> None:
    """Play audio through PulseAudio virtual sink."""
    pulse_server = os.environ.get("PULSE_SERVER", "unix:/run/pulse/native")
    result = subprocess.run(
        ["paplay", f"--server={pulse_server}", "--sink=virtual_out", str(audio_path)],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(f"[audio-speak-api] paplay stderr: {result.stderr}", file=sys.stderr)
        # Non-fatal: audio may still have played partially.


def speak_openai_tts(input_txt: Path, voice: str) -> None:
    """Synthesize speech using OpenAI TTS API (/v1/audio/speech)."""
    import openai

    api_key = os.environ.get("OPENAI_API_KEY") or os.environ.get("LLM_API_KEY")
    if not api_key:
        print(
            "ERROR: OPENAI_API_KEY or LLM_API_KEY must be set for openai_tts backend",
            file=sys.stderr,
        )
        sys.exit(1)

    base_url = os.environ.get("TTS_BASE_URL") or os.environ.get("LLM_BASE_URL")
    model = os.environ.get("TTS_MODEL", "tts-1")
    effective_voice = voice or os.environ.get("TTS_VOICE", "alloy")

    # Validate voice name (OpenAI TTS voices).
    valid_voices = {"alloy", "echo", "fable", "onyx", "nova", "shimmer"}
    if effective_voice not in valid_voices:
        print(
            f"WARNING: Unknown OpenAI TTS voice '{effective_voice}', using 'alloy'",
            file=sys.stderr,
        )
        effective_voice = "alloy"

    text = input_txt.read_text(encoding="utf-8")
    char_count = len(text)

    print(
        f"[audio-speak-api] Calling OpenAI TTS API "
        f"(model={model}, voice={effective_voice}, chars={char_count})...",
        file=sys.stderr,
    )

    client = openai.OpenAI(api_key=api_key, base_url=base_url)

    with tempfile.NamedTemporaryFile(
        suffix=".mp3", dir="/tmp", delete=False
    ) as tmp_audio:
        tmp_path = Path(tmp_audio.name)

    try:
        response = client.audio.speech.create(
            model=model,
            voice=effective_voice,  # type: ignore[arg-type]
            input=text,
            response_format="mp3",
        )
        response.stream_to_file(str(tmp_path))

        # Convert MP3 to WAV for paplay compatibility.
        wav_path = tmp_path.with_suffix(".wav")
        result = subprocess.run(
            ["ffmpeg", "-y", "-i", str(tmp_path), str(wav_path)],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            print(f"[audio-speak-api] ffmpeg stderr: {result.stderr}", file=sys.stderr)
            # Try playing MP3 directly as fallback.
            play_audio(tmp_path)
        else:
            play_audio(wav_path)
            wav_path.unlink(missing_ok=True)

    finally:
        tmp_path.unlink(missing_ok=True)

    print(
        f"[audio-speak-api] Playback complete ({char_count} chars)",
        file=sys.stderr,
    )


def speak_chat_completions_tts(input_txt: Path, voice: str) -> None:
    """Synthesize speech using OpenAI-compatible chat completions TTS output."""
    import base64

    import openai

    api_key = os.environ.get("OPENAI_API_KEY") or os.environ.get("LLM_API_KEY")
    if not api_key:
        print(
            "ERROR: OPENAI_API_KEY or LLM_API_KEY must be set for chat_completions_tts backend",
            file=sys.stderr,
        )
        sys.exit(1)

    base_url = os.environ.get("TTS_BASE_URL") or os.environ.get("LLM_BASE_URL")
    model = os.environ.get("TTS_MODEL", "gpt-4o-audio-preview")
    effective_voice = voice or os.environ.get("TTS_VOICE", "alloy")

    text = input_txt.read_text(encoding="utf-8")
    char_count = len(text)

    print(
        f"[audio-speak-api] Calling chat completions TTS "
        f"(model={model}, voice={effective_voice}, chars={char_count})...",
        file=sys.stderr,
    )

    client = openai.OpenAI(api_key=api_key, base_url=base_url)

    response = client.chat.completions.create(
        model=model,
        modalities=["text", "audio"],
        audio={"voice": effective_voice, "format": "wav"},
        messages=[
            {
                "role": "user",
                "content": f"Please speak the following text aloud:\n\n{text}",
            }
        ],
    )

    # Extract audio data from response.
    audio_data = None
    if response.choices and response.choices[0].message:
        msg = response.choices[0].message
        if hasattr(msg, "audio") and msg.audio:
            audio_data = msg.audio.data

    if not audio_data:
        print(
            "ERROR: chat completions TTS response did not contain audio data",
            file=sys.stderr,
        )
        sys.exit(1)

    # Decode and play audio.
    with tempfile.NamedTemporaryFile(
        suffix=".wav", dir="/tmp", delete=False
    ) as tmp_audio:
        tmp_path = Path(tmp_audio.name)
        tmp_audio.write(base64.b64decode(audio_data))

    try:
        play_audio(tmp_path)
    finally:
        tmp_path.unlink(missing_ok=True)

    print(
        f"[audio-speak-api] Playback complete ({char_count} chars)",
        file=sys.stderr,
    )


def main() -> None:
    if len(sys.argv) != 4:
        print(
            "Usage: audio-speak-api.py <input_txt> <voice> <backend>",
            file=sys.stderr,
        )
        sys.exit(1)

    input_txt = validate_tmp_path(sys.argv[1], "input_txt")
    voice = sys.argv[2]
    backend = sys.argv[3]

    if not input_txt.exists():
        print(f"ERROR: input file not found: {input_txt}", file=sys.stderr)
        sys.exit(1)

    if backend == "openai_tts":
        speak_openai_tts(input_txt, voice)
    elif backend == "chat_completions_tts":
        speak_chat_completions_tts(input_txt, voice)
    else:
        print(
            f"ERROR: Unknown backend: {backend}. "
            "Valid values: openai_tts, chat_completions_tts",
            file=sys.stderr,
        )
        sys.exit(1)


if __name__ == "__main__":
    main()
