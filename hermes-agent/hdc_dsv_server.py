#!/usr/bin/env python3
"""HDC DSV (Hyperdimensional Computing Distributed Sparse Vector) local model server.

.. deprecated::
    This Python implementation is superseded by the Rust ``ironclaw-hdc-server`` binary.
    The Rust binary provides:
    - Bearer token authentication on ``/v1/train`` and ``/v1/chat/completions``
    - ``bincode`` model serialization (replaces Python pickle — no RCE on model load)
    - Loopback-only binding (127.0.0.1, not configurable)
    - ``zeroize`` on key material

    To use the Rust binary::

        IRONCLAW_HDC_SERVER_TOKEN=my-secret-token \\
        IRONCLAW_HDC_MODEL_PATH=~/.ironclaw/hdc_model.bin \\
        ironclaw-hdc-server

    This file will exec the Rust binary if it is found on PATH.
    If the Rust binary is not available, it falls back to this Python implementation
    with a deprecation warning.

----

Original HDC DSV (Hyperdimensional Computing Distributed Sparse Vector) local model server.

Exposes an OpenAI-compatible HTTP API for the HDC DSV model so that IronClaw's
orchestrator LLM proxy can route self-improvement review calls to the local model
with zero changes to the proxy code.

## Endpoints

- ``POST /v1/chat/completions`` — encode message as hypervector, return
  nearest-class label + confidence as the "completion"
- ``POST /v1/train`` — online update with labeled example (GOOD_WRITE / BAD_WRITE)
- ``GET /v1/models`` — returns ``[{"id": "hdc-dsv-local"}]`` for IronClaw model discovery

## Security

- Binds to ``127.0.0.1`` only (no external interface)
- Model state file protected by OS file permissions (0600)
- No telemetry, no cloud sync

## Usage

    python hdc_dsv_server.py

    # Or with custom settings:
    IRONCLAW_HDC_MODEL_PATH=~/.ironclaw/hdc_model.bin \\
    IRONCLAW_HDC_PORT=8765 \\
    python hdc_dsv_server.py

## Environment Variables

- ``IRONCLAW_HDC_MODEL_PATH``: Path to model state file (default: ``~/.ironclaw/hdc_model.bin``)
- ``IRONCLAW_HDC_PORT``: Port to bind to (default: ``8765``)
- ``IRONCLAW_HDC_DIMENSIONS``: Hypervector dimensions (default: ``10000``)
- ``IRONCLAW_HDC_SEED``: Random seed for reproducible hypervectors (default: ``42``)
"""

# ---------------------------------------------------------------------------
# Deprecation shim: exec the Rust binary if available
# ---------------------------------------------------------------------------
# This block runs before any other imports so that the Rust binary takes over
# as early as possible, before fastapi/uvicorn are imported.

import os as _os
import shutil as _shutil
import sys as _sys
import warnings as _warnings

_RUST_BINARY = _shutil.which("ironclaw-hdc-server")

if _RUST_BINARY is not None:
    _warnings.warn(
        "hdc_dsv_server.py is deprecated. "
        "The Rust binary 'ironclaw-hdc-server' was found and will be exec'd instead. "
        "The Rust binary provides bearer token auth, bincode model serialization "
        "(no pickle RCE), and loopback-only binding. "
        "Remove hdc_dsv_server.py from your startup scripts.",
        DeprecationWarning,
        stacklevel=1,
    )
    # Replace this Python process with the Rust binary.
    # os.execvp replaces the current process — no return.
    _os.execvp(_RUST_BINARY, [_RUST_BINARY] + _sys.argv[1:])
    # If execvp returns (should not happen), fall through to Python implementation.

_warnings.warn(
    "hdc_dsv_server.py is deprecated and will be removed in a future release. "
    "Use the Rust binary 'ironclaw-hdc-server' instead. "
    "Build it with: cd ironclaw && cargo build --release -p ironclaw_hdc_server\n"
    "Security vulnerabilities in this Python implementation:\n"
    "  - No authentication on /v1/train (HDC model poisoning risk)\n"
    "  - Model file uses Python pickle (RCE on load)\n"
    "  - AES-256-GCM may silently degrade to base64",
    DeprecationWarning,
    stacklevel=1,
)

# ---------------------------------------------------------------------------
# Python fallback implementation (kept for backward compatibility)
# ---------------------------------------------------------------------------

from __future__ import annotations

import hashlib
import json
import logging
import math
import os
import pickle
import random
import stat
import threading
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

try:
    from fastapi import FastAPI, HTTPException, Request
    from fastapi.responses import JSONResponse
    import uvicorn
    from pydantic import BaseModel
except ImportError as _e:
    raise ImportError(
        "hdc_dsv_server requires fastapi and uvicorn. "
        "Install with: pip install fastapi uvicorn"
    ) from _e

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

MODEL_PATH = Path(
    os.environ.get("IRONCLAW_HDC_MODEL_PATH", str(Path.home() / ".ironclaw" / "hdc_model.bin"))
)
PORT = int(os.environ.get("IRONCLAW_HDC_PORT", "8765"))
DIMENSIONS = int(os.environ.get("IRONCLAW_HDC_DIMENSIONS", "10000"))
SEED = int(os.environ.get("IRONCLAW_HDC_SEED", "42"))

# ---------------------------------------------------------------------------
# HDC DSV Model
# ---------------------------------------------------------------------------


class HdcDsvModel:
    """Hyperdimensional Computing Distributed Sparse Vector model.

    Encodes text as high-dimensional binary/bipolar vectors (hypervectors)
    and learns by online, one-shot bundling — no gradient descent, no backprop.

    Classes:
    - ``GOOD_WRITE`` (label=1): Skill/memory writes that were committed and passed checks.
    - ``BAD_WRITE`` (label=0): Writes that were blocked, rolled back, or flagged.
    """

    GOOD_WRITE = "GOOD_WRITE"
    BAD_WRITE = "BAD_WRITE"

    def __init__(self, dimensions: int = DIMENSIONS, seed: int = SEED) -> None:
        self.dimensions = dimensions
        self.seed = seed
        self._rng = random.Random(seed)

        # Class prototype hypervectors (sum of all training examples).
        self._prototypes: Dict[str, List[float]] = {
            self.GOOD_WRITE: [0.0] * dimensions,
            self.BAD_WRITE: [0.0] * dimensions,
        }
        # Training example counts per class.
        self._counts: Dict[str, int] = {self.GOOD_WRITE: 0, self.BAD_WRITE: 0}
        # Character-level random hypervectors (seeded, deterministic).
        self._char_hvs: Dict[str, List[float]] = {}
        self._lock = threading.Lock()

    @property
    def training_count(self) -> int:
        return sum(self._counts.values())

    def _get_char_hv(self, char: str) -> List[float]:
        """Get or generate a random hypervector for a character."""
        if char not in self._char_hvs:
            hv = [1.0 if self._rng.random() > 0.5 else -1.0 for _ in range(self.dimensions)]
            self._char_hvs[char] = hv
        return self._char_hvs[char]

    def _encode_text(self, text: str) -> List[float]:
        """Encode text as a hypervector using n-gram bundling."""
        if not text:
            return [0.0] * self.dimensions

        # Bag-of-characters encoding with position binding.
        result = [0.0] * self.dimensions
        n = min(len(text), 512)  # Cap at 512 chars for speed.

        for i, char in enumerate(text[:n]):
            char_hv = self._get_char_hv(char)
            # Position binding: XOR with a position hypervector.
            pos_hv = self._get_char_hv(f"__pos_{i % 10}__")
            for j in range(self.dimensions):
                result[j] += char_hv[j] * pos_hv[j]

        # Normalize.
        magnitude = math.sqrt(sum(x * x for x in result)) or 1.0
        return [x / magnitude for x in result]

    def _cosine_similarity(self, a: List[float], b: List[float]) -> float:
        """Compute cosine similarity between two hypervectors."""
        dot = sum(x * y for x, y in zip(a, b))
        mag_a = math.sqrt(sum(x * x for x in a)) or 1.0
        mag_b = math.sqrt(sum(x * x for x in b)) or 1.0
        return dot / (mag_a * mag_b)

    def score(self, content: str, job_type: str = "", target: str = "") -> Tuple[float, str, float]:
        """Score a write payload.

        Returns (score, label, confidence) where:
        - score: float in [0.0, 1.0] (higher = more likely GOOD_WRITE)
        - label: "GOOD_WRITE" or "BAD_WRITE"
        - confidence: float in [0.0, 1.0]
        """
        with self._lock:
            if self.training_count == 0:
                # No training data — return neutral score.
                return (0.5, self.GOOD_WRITE, 0.0)

            # Encode the input.
            combined = f"{job_type}|{target}|{content}"
            query_hv = self._encode_text(combined)

            # Compare to class prototypes.
            good_proto = self._prototypes[self.GOOD_WRITE]
            bad_proto = self._prototypes[self.BAD_WRITE]

            good_count = self._counts[self.GOOD_WRITE]
            bad_count = self._counts[self.BAD_WRITE]

            if good_count == 0:
                return (0.0, self.BAD_WRITE, 1.0)
            if bad_count == 0:
                return (1.0, self.GOOD_WRITE, 1.0)

            # Normalize prototypes by count.
            good_norm = [x / good_count for x in good_proto]
            bad_norm = [x / bad_count for x in bad_proto]

            sim_good = self._cosine_similarity(query_hv, good_norm)
            sim_bad = self._cosine_similarity(query_hv, bad_norm)

            # Convert to score in [0, 1].
            total = abs(sim_good) + abs(sim_bad) + 1e-9
            score = (sim_good + 1.0) / 2.0  # Map [-1, 1] → [0, 1]
            confidence = abs(sim_good - sim_bad) / total

            label = self.GOOD_WRITE if sim_good > sim_bad else self.BAD_WRITE
            return (float(score), label, float(confidence))

    def train(self, content: str, label: str, job_type: str = "", target: str = "") -> int:
        """Online update with a labeled example.

        Returns the new total training count.
        """
        if label not in (self.GOOD_WRITE, self.BAD_WRITE):
            raise ValueError(f"Invalid label: {label}. Must be GOOD_WRITE or BAD_WRITE.")

        combined = f"{job_type}|{target}|{content}"
        hv = self._encode_text(combined)

        with self._lock:
            proto = self._prototypes[label]
            for i in range(self.dimensions):
                proto[i] += hv[i]
            self._counts[label] += 1
            return self.training_count

    def save(self, path: Path) -> None:
        """Save model state to a binary file."""
        path.parent.mkdir(parents=True, exist_ok=True)
        state = {
            "dimensions": self.dimensions,
            "seed": self.seed,
            "prototypes": self._prototypes,
            "counts": self._counts,
            "char_hvs": self._char_hvs,
        }
        tmp_path = path.with_suffix(".tmp")
        with open(tmp_path, "wb") as f:
            pickle.dump(state, f, protocol=pickle.HIGHEST_PROTOCOL)
        tmp_path.rename(path)
        # Set file permissions to 0600 (owner read/write only).
        path.chmod(stat.S_IRUSR | stat.S_IWUSR)

    @classmethod
    def load(cls, path: Path) -> "HdcDsvModel":
        """Load model state from a binary file."""
        with open(path, "rb") as f:
            state = pickle.load(f)
        model = cls(dimensions=state["dimensions"], seed=state["seed"])
        model._prototypes = state["prototypes"]
        model._counts = state["counts"]
        model._char_hvs = state["char_hvs"]
        return model


# ---------------------------------------------------------------------------
# FastAPI app
# ---------------------------------------------------------------------------

app = FastAPI(title="HDC DSV Local Model Server", version="0.1.0")

# Global model instance (loaded at startup).
_model: Optional[HdcDsvModel] = None
_model_lock = threading.Lock()


def get_model() -> HdcDsvModel:
    global _model
    if _model is None:
        with _model_lock:
            if _model is None:
                if MODEL_PATH.exists():
                    logger.info("Loading HDC DSV model from %s", MODEL_PATH)
                    _model = HdcDsvModel.load(MODEL_PATH)
                else:
                    logger.info(
                        "No model found at %s — starting fresh (bootstrap mode)", MODEL_PATH
                    )
                    _model = HdcDsvModel(dimensions=DIMENSIONS, seed=SEED)
    return _model


def save_model() -> None:
    model = get_model()
    model.save(MODEL_PATH)
    logger.debug("HDC DSV model saved to %s (training_count=%d)", MODEL_PATH, model.training_count)


# ---------------------------------------------------------------------------
# Request/response models
# ---------------------------------------------------------------------------


class ChatMessage(BaseModel):
    role: str
    content: str


class ChatCompletionRequest(BaseModel):
    model: str = "hdc-dsv-local"
    messages: List[ChatMessage]
    max_tokens: int = 256
    temperature: float = 0.0


class TrainRequest(BaseModel):
    content: str
    label: str  # GOOD_WRITE or BAD_WRITE
    job_type: str = ""
    target: str = ""


# ---------------------------------------------------------------------------
# Endpoints
# ---------------------------------------------------------------------------


@app.get("/v1/models")
async def list_models() -> JSONResponse:
    """Return the available models (for IronClaw model discovery)."""
    return JSONResponse({
        "object": "list",
        "data": [
            {
                "id": "hdc-dsv-local",
                "object": "model",
                "created": int(time.time()),
                "owned_by": "ironclaw-local",
            }
        ],
    })


@app.post("/v1/chat/completions")
async def chat_completions(req: ChatCompletionRequest) -> JSONResponse:
    """Score a write payload using the HDC DSV model.

    The request content is expected to be a JSON string with fields:
    - ``tool``: tool name
    - ``target``: skill name or memory key
    - ``job_type``: job type context
    - ``size_delta``: bytes added/removed
    - ``content_preview``: first 512 chars of the content

    Returns an OpenAI-compatible response where the content is a JSON string
    with fields: ``score``, ``label``, ``confidence``, ``training_count``.
    """
    model = get_model()

    # Extract the last user message.
    user_content = ""
    for msg in reversed(req.messages):
        if msg.role == "user":
            user_content = msg.content
            break

    # Parse the payload JSON.
    try:
        payload = json.loads(user_content)
        content = payload.get("content_preview", user_content)
        job_type = payload.get("job_type", "")
        target = payload.get("target", "")
    except (json.JSONDecodeError, TypeError):
        content = user_content
        job_type = ""
        target = ""

    score, label, confidence = model.score(content, job_type=job_type, target=target)

    response_content = json.dumps({
        "score": round(score, 4),
        "label": label,
        "confidence": round(confidence, 4),
        "training_count": model.training_count,
    })

    return JSONResponse({
        "id": f"hdc-{hashlib.md5(user_content.encode()).hexdigest()[:8]}",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": "hdc-dsv-local",
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": response_content,
                },
                "finish_reason": "stop",
            }
        ],
        "usage": {
            "prompt_tokens": len(user_content.split()),
            "completion_tokens": 10,
            "total_tokens": len(user_content.split()) + 10,
        },
    })


@app.post("/v1/train")
async def train(req: TrainRequest) -> JSONResponse:
    """Online update with a labeled example.

    Accepts ``GOOD_WRITE`` or ``BAD_WRITE`` labels and updates the model's
    class prototype hypervectors in real time.
    """
    model = get_model()

    try:
        new_count = model.train(
            content=req.content,
            label=req.label,
            job_type=req.job_type,
            target=req.target,
        )
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc))

    # Persist model state after each training update.
    save_model()

    return JSONResponse({
        "success": True,
        "training_count": new_count,
        "message": f"Model updated with label={req.label} (total={new_count})",
    })


@app.get("/health")
async def health() -> JSONResponse:
    model = get_model()
    return JSONResponse({
        "status": "ok",
        "model": "hdc-dsv-local",
        "training_count": model.training_count,
        "model_path": str(MODEL_PATH),
    })


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    # Pre-load the model.
    get_model()

    logger.info(
        "HDC DSV server starting on 127.0.0.1:%d (model=%s, training_count=%d)",
        PORT,
        MODEL_PATH,
        get_model().training_count,
    )

    uvicorn.run(
        app,
        host="127.0.0.1",  # Bind to loopback only — no external interface.
        port=PORT,
        log_level="info",
        access_log=False,
    )
