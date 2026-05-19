#!/usr/bin/env python3
"""API-based speech-to-text transcription script.

Used by audio-transcribe.sh when STT_BACKEND is 'whisper_api' or
'chat_completions'. Reads API credentials from environment variables
(injected by the domain-allowlist proxy, never passed as CLI args).

Usage:
    audio-transcribe-api.py <input_wav> <output_json> <backend>

Backends:
    whisper_api       — OpenAI Whisper API (/v1/audio/transcriptions)
    chat_completions  — OpenAI-compatible chat completions with audio input

Output JSON format:
    {"text": "...", "language": "en", "confidence": 0.95, "backend": "..."}

Security notes:
    - API keys are read from environment variables only (never CLI args).
    - Input/output paths are validated to be under /tmp.
    - The proxy (HTTP_PROXY / HTTPS_PROXY) enforces the domain allowlist.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path


def validate_tmp_path(path: str, name: str) -> Path:
    """Validate that a path is under /tmp."""
    p = Path(path).resolve()
    if not str(p).startswith("/tmp/"):
        print(f"ERROR: {name} must be under /tmp (got: {path})", file=sys.stderr)
        sys.exit(1)
    return p


def transcribe_whisper_api(input_wav: Path, output_json: Path) -> None:
    """Transcribe using OpenAI Whisper API (/v1/audio/transcriptions)."""
    import openai

    api_key = os.environ.get("OPENAI_API_KEY") or os.environ.get("LLM_API_KEY")
    if not api_key:
        print(
            "ERROR: OPENAI_API_KEY or LLM_API_KEY must be set for whisper_api backend",
            file=sys.stderr,
        )
        sys.exit(1)

    base_url = os.environ.get("STT_BASE_URL") or os.environ.get("LLM_BASE_URL")
    model = os.environ.get("STT_MODEL", "whisper-1")

    client = openai.OpenAI(
        api_key=api_key,
        base_url=base_url,
        # Proxy is set via HTTP_PROXY / HTTPS_PROXY env vars (standard).
    )

    print(f"[audio-transcribe-api] Calling Whisper API (model={model})...", file=sys.stderr)

    with open(input_wav, "rb") as f:
        response = client.audio.transcriptions.create(
            model=model,
            file=f,
            response_format="verbose_json",
            language=None,  # Auto-detect.
        )

    result = {
        "text": response.text,
        "language": getattr(response, "language", None),
        "confidence": None,
        "backend": "whisper_api",
    }

    with open(output_json, "w") as f:
        json.dump(result, f)

    print(
        f"[audio-transcribe-api] Transcript ({len(response.text)} chars): "
        f"{response.text[:80]}...",
        file=sys.stderr,
    )


def transcribe_chat_completions(input_wav: Path, output_json: Path) -> None:
    """Transcribe using OpenAI-compatible chat completions with audio input."""
    import base64

    import openai

    api_key = os.environ.get("OPENAI_API_KEY") or os.environ.get("LLM_API_KEY")
    if not api_key:
        print(
            "ERROR: OPENAI_API_KEY or LLM_API_KEY must be set for chat_completions backend",
            file=sys.stderr,
        )
        sys.exit(1)

    base_url = os.environ.get("STT_BASE_URL") or os.environ.get("LLM_BASE_URL")
    model = os.environ.get("STT_MODEL", "gpt-4o-audio-preview")

    client = openai.OpenAI(api_key=api_key, base_url=base_url)

    # Encode audio as base64.
    with open(input_wav, "rb") as f:
        audio_b64 = base64.b64encode(f.read()).decode()

    print(
        f"[audio-transcribe-api] Calling chat completions (model={model})...",
        file=sys.stderr,
    )

    response = client.chat.completions.create(
        model=model,
        messages=[
            {
                "role": "user",
                "content": [
                    {
                        "type": "input_audio",
                        "input_audio": {
                            "data": audio_b64,
                            "format": "wav",
                        },
                    },
                    {
                        "type": "text",
                        "text": "Please transcribe this audio exactly as spoken. "
                        "Return only the transcription, no commentary.",
                    },
                ],
            }
        ],
        max_tokens=4096,
    )

    text = response.choices[0].message.content or ""

    result = {
        "text": text.strip(),
        "language": None,
        "confidence": None,
        "backend": "chat_completions",
    }

    with open(output_json, "w") as f:
        json.dump(result, f)

    print(
        f"[audio-transcribe-api] Transcript ({len(text)} chars): {text[:80]}...",
        file=sys.stderr,
    )


def main() -> None:
    if len(sys.argv) != 4:
        print(
            "Usage: audio-transcribe-api.py <input_wav> <output_json> <backend>",
            file=sys.stderr,
        )
        sys.exit(1)

    input_wav = validate_tmp_path(sys.argv[1], "input_wav")
    output_json = validate_tmp_path(sys.argv[2], "output_json")
    backend = sys.argv[3]

    if not input_wav.exists():
        print(f"ERROR: input file not found: {input_wav}", file=sys.stderr)
        sys.exit(1)

    if backend == "whisper_api":
        transcribe_whisper_api(input_wav, output_json)
    elif backend == "chat_completions":
        transcribe_chat_completions(input_wav, output_json)
    else:
        print(
            f"ERROR: Unknown backend: {backend}. "
            "Valid values: whisper_api, chat_completions",
            file=sys.stderr,
        )
        sys.exit(1)


if __name__ == "__main__":
    main()
