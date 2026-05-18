"""Self-improvement dispatcher for Hermes Agent.

Routes all self-improvement work (background review, curator, SWE runner)
through the IronClaw sandbox stack instead of running in the main agent process.

## LLM Client Selection

| Mode | LLM Used | When |
|------|----------|------|
| ``auxiliary`` (default) | Resolved by ``get_text_auxiliary_client("self_improve")`` | Always, unless overridden |
| ``main`` (opt-in) | Same provider/model as the parent agent turn | ``SELF_IMPROVE_LLM_CLIENT=main`` |
| ``local`` (opt-in) | OpenAI-compatible local server | ``SELF_IMPROVE_LLM_CLIENT=local`` |

If ``auxiliary`` mode is selected and no auxiliary provider is configured,
the dispatcher logs a warning and skips the cycle (never silently falls back
to the main model to avoid surprise token spend).

## Feature Flag

The dispatcher is only active when ``HERMES_SECURE_SELF_IMPROVE=true``.
When the flag is not set, the caller should fall back to the existing
``spawn_background_review_thread()`` path.
"""

from __future__ import annotations

import base64
import hashlib
import json
import logging
import os
import threading
import time
import urllib.request
import urllib.error
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional, Tuple
from uuid import uuid4

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Job types
# ---------------------------------------------------------------------------

JOB_TYPE_MEMORY_REVIEW = "MEMORY_REVIEW"
JOB_TYPE_SKILL_REVIEW = "SKILL_REVIEW"
JOB_TYPE_CURATOR_RUN = "CURATOR_RUN"
JOB_TYPE_SWE_TASK = "SWE_TASK"

# ---------------------------------------------------------------------------
# Configuration helpers
# ---------------------------------------------------------------------------


def _env_bool(name: str, default: bool = False) -> bool:
    val = os.environ.get(name, "").lower()
    if val in ("1", "true", "yes"):
        return True
    if val in ("0", "false", "no"):
        return False
    return default


def _env_int(name: str, default: int) -> int:
    try:
        return int(os.environ.get(name, str(default)))
    except (ValueError, TypeError):
        return default


def is_secure_self_improve_enabled() -> bool:
    """Return True if the sandboxed self-improvement feature flag is set.

    .. deprecated::
        Prefer :func:`should_use_ironclaw` which auto-detects orchestrator
        availability instead of requiring an explicit opt-in flag.
    """
    return _env_bool("HERMES_SECURE_SELF_IMPROVE", False)


def is_local_self_improve_forced() -> bool:
    """Return True when the caller has explicitly opted out of IronClaw dispatch.

    Set ``HERMES_PREFER_LOCAL_SELF_IMPROVE=true`` to force the legacy in-process
    Hermes review fork even when the IronClaw orchestrator is reachable.
    """
    return _env_bool("HERMES_PREFER_LOCAL_SELF_IMPROVE", False)


def is_orchestrator_reachable(timeout: float = 3.0) -> bool:
    """Probe the IronClaw orchestrator health endpoint.

    Returns ``True`` when the orchestrator responds with HTTP 200 within
    *timeout* seconds.  Returns ``False`` on any network error, timeout, or
    non-200 response — never raises.

    The orchestrator URL is read from ``IRONCLAW_ORCHESTRATOR_URL``
    (default ``http://localhost:8080``).  The probe hits ``GET /health``
    which requires no authentication.
    """
    orchestrator_url = os.environ.get(
        "IRONCLAW_ORCHESTRATOR_URL", "http://localhost:8080"
    ).rstrip("/")
    health_url = f"{orchestrator_url}/health"
    try:
        req = urllib.request.Request(health_url, method="GET")
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status == 200
    except Exception:
        return False


def should_use_ironclaw() -> bool:
    """Return True when IronClaw should handle self-improvement work.

    Decision logic (in priority order):

    1. ``HERMES_PREFER_LOCAL_SELF_IMPROVE=true`` → always use local Hermes fork
       (explicit opt-out).
    2. ``HERMES_SECURE_SELF_IMPROVE=true`` → always use IronClaw (explicit
       opt-in; skips the reachability probe so a misconfigured orchestrator
       URL surfaces as a submission error rather than a silent fallback).
    3. Otherwise → probe ``GET /health`` on the orchestrator.  Use IronClaw
       when reachable; fall back to the local Hermes fork when not.
    """
    if is_local_self_improve_forced():
        logger.debug(
            "Self-improve: HERMES_PREFER_LOCAL_SELF_IMPROVE=true — "
            "using local Hermes review fork"
        )
        return False
    if is_secure_self_improve_enabled():
        # Explicit opt-in: trust the configured URL, skip the probe.
        return True
    # Auto-detect: prefer IronClaw when the orchestrator is reachable.
    reachable = is_orchestrator_reachable()
    if reachable:
        logger.debug(
            "Self-improve: IronClaw orchestrator reachable at %s — "
            "routing self-improvement through sandbox",
            os.environ.get("IRONCLAW_ORCHESTRATOR_URL", "http://localhost:8080"),
        )
    else:
        logger.debug(
            "Self-improve: IronClaw orchestrator not reachable — "
            "falling back to local Hermes review fork"
        )
    return reachable


# ---------------------------------------------------------------------------
# LLM client resolution
# ---------------------------------------------------------------------------


def _resolve_llm_client(agent: Any) -> Optional[Tuple[str, str, Optional[str]]]:
    """Resolve the (provider, model, base_url) triple for the review fork.

    Returns None if no client is available (caller should skip the cycle).

    Resolution:
    - ``SELF_IMPROVE_LLM_CLIENT=main``: use parent agent's runtime
    - ``SELF_IMPROVE_LLM_CLIENT=local``: use local OpenAI-compatible server
    - ``SELF_IMPROVE_LLM_CLIENT=auxiliary`` (default): use auxiliary client
    """
    mode = os.environ.get("SELF_IMPROVE_LLM_CLIENT", "auxiliary").lower().strip()

    if mode == "main":
        # Use the same provider/model as the parent agent turn.
        try:
            provider = getattr(agent, "provider", None) or "unknown"
            model = getattr(agent, "model", None) or "unknown"
            runtime = getattr(agent, "_current_main_runtime", lambda: {})()
            base_url = runtime.get("base_url") if isinstance(runtime, dict) else None
            logger.debug(
                "Self-improve LLM: main mode (provider=%s, model=%s)", provider, model
            )
            return (provider, model, base_url)
        except Exception as exc:
            logger.warning(
                "Self-improve: failed to resolve main runtime: %s — skipping cycle", exc
            )
            return None

    if mode == "local":
        # Use a local OpenAI-compatible server.
        base_url = os.environ.get("SELF_IMPROVE_LLM_BASE_URL", "").strip()
        model = os.environ.get("SELF_IMPROVE_LLM_MODEL", "hdc-dsv-local").strip()
        if not base_url:
            logger.warning(
                "Self-improve: SELF_IMPROVE_LLM_CLIENT=local but "
                "SELF_IMPROVE_LLM_BASE_URL is not set — skipping cycle"
            )
            return None
        # Verify the local server is reachable.
        try:
            health_url = base_url.rstrip("/").replace("/v1", "") + "/v1/models"
            req = urllib.request.Request(health_url, method="GET")
            with urllib.request.urlopen(req, timeout=3):
                pass
        except Exception as exc:
            logger.warning(
                "Self-improve: local LLM server at %s is unreachable (%s) — skipping cycle",
                base_url,
                exc,
            )
            return None
        logger.debug(
            "Self-improve LLM: local mode (base_url=%s, model=%s)", base_url, model
        )
        return ("openai_compatible", model, base_url)

    # Default: auxiliary mode.
    try:
        from agent.auxiliary_client import get_text_auxiliary_client

        result = get_text_auxiliary_client("self_improve")
        if result is None or (isinstance(result, tuple) and result[0] is None):
            logger.warning(
                "Self-improve: no auxiliary LLM configured "
                "(SELF_IMPROVE_LLM_CLIENT=auxiliary) — skipping cycle. "
                "Set SELF_IMPROVE_LLM_CLIENT=main to use the main model, "
                "or configure an auxiliary provider."
            )
            return None
        # get_text_auxiliary_client returns (client, model) or similar.
        # We need (provider, model, base_url) for the orchestrator.
        if isinstance(result, tuple) and len(result) >= 2:
            client_obj, model = result[0], result[1]
            # Extract provider/base_url from the client object if possible.
            provider = getattr(client_obj, "_provider", "auxiliary")
            base_url = getattr(client_obj, "base_url", None)
            if hasattr(base_url, "__str__"):
                base_url = str(base_url).rstrip("/")
            logger.debug(
                "Self-improve LLM: auxiliary mode (provider=%s, model=%s)", provider, model
            )
            return (provider, model, base_url)
        logger.warning(
            "Self-improve: unexpected auxiliary client result type %s — skipping cycle",
            type(result),
        )
        return None
    except ImportError:
        logger.warning(
            "Self-improve: auxiliary_client not available — skipping cycle"
        )
        return None
    except Exception as exc:
        logger.warning(
            "Self-improve: failed to resolve auxiliary client: %s — skipping cycle", exc
        )
        return None


# ---------------------------------------------------------------------------
# Conversation snapshot encryption (lightweight AES-256-GCM via secrets)
# ---------------------------------------------------------------------------


def _encrypt_snapshot(snapshot: Dict[str, Any]) -> Dict[str, str]:
    """Encrypt a conversation snapshot for transit to the orchestrator.

    Uses AES-256-GCM if the ``cryptography`` package is available,
    otherwise falls back to base64 encoding with a warning.

    Returns a dict with ``ciphertext``, ``nonce``, and ``key_id`` fields.
    """
    payload = json.dumps(snapshot, ensure_ascii=False).encode("utf-8")

    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        import secrets as _secrets

        key = _secrets.token_bytes(32)  # 256-bit key
        nonce = _secrets.token_bytes(12)  # 96-bit nonce for GCM
        key_id = hashlib.sha256(key).hexdigest()[:16]

        aesgcm = AESGCM(key)
        ciphertext = aesgcm.encrypt(nonce, payload, None)

        return {
            "ciphertext": base64.b64encode(ciphertext).decode(),
            "nonce": base64.b64encode(nonce).decode(),
            "key_id": key_id,
        }
    except ImportError:
        logger.warning(
            "Self-improve: 'cryptography' package not installed — "
            "snapshot is base64-encoded (not encrypted). "
            "Install cryptography for AES-256-GCM encryption."
        )
        return {
            "ciphertext": base64.b64encode(payload).decode(),
            "nonce": base64.b64encode(b"\x00" * 12).decode(),
            "key_id": "plaintext",
        }


# ---------------------------------------------------------------------------
# Orchestrator HTTP client
# ---------------------------------------------------------------------------


def _submit_job_to_orchestrator(
    job_type: str,
    snapshot: Dict[str, Any],
    llm_provider: str,
    llm_model: str,
    llm_base_url: Optional[str],
    llm_mode: str,
) -> Optional[str]:
    """Submit a self-improvement job to the IronClaw orchestrator.

    Returns the job_id string on success, or None on failure.
    """
    orchestrator_url = os.environ.get(
        "IRONCLAW_ORCHESTRATOR_URL", "http://localhost:8080"
    ).rstrip("/")
    orchestrator_token = os.environ.get("IRONCLAW_ORCHESTRATOR_TOKEN", "")

    encrypted = _encrypt_snapshot(snapshot)

    payload = {
        "job_type": job_type,
        "snapshot_encrypted": encrypted,
        "llm_client_mode": llm_mode,
        "resolved_llm": {
            "provider": llm_provider,
            "model": llm_model,
            "base_url": llm_base_url,
        },
        "max_turns": _env_int("SELF_IMPROVE_MAX_TURNS", 10),
        "max_wall_seconds": _env_int("SELF_IMPROVE_MAX_WALL_SECS", 120),
        "max_skill_writes": _env_int("SELF_IMPROVE_MAX_SKILL_WRITES", 10),
        "max_memory_writes": _env_int("SELF_IMPROVE_MAX_MEMORY_WRITES", 5),
    }

    body = json.dumps(payload).encode("utf-8")
    url = f"{orchestrator_url}/jobs/self-improve"

    try:
        req = urllib.request.Request(
            url,
            data=body,
            method="POST",
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {orchestrator_token}",
                "Content-Length": str(len(body)),
            },
        )
        with urllib.request.urlopen(req, timeout=10) as resp:
            resp_data = json.loads(resp.read().decode("utf-8"))
            job_id = resp_data.get("job_id")
            logger.info(
                "Self-improve job submitted: job_id=%s, type=%s, llm=%s/%s",
                job_id,
                job_type,
                llm_provider,
                llm_model,
            )
            return job_id
    except urllib.error.HTTPError as exc:
        logger.warning(
            "Self-improve: orchestrator returned HTTP %s for job submission: %s",
            exc.code,
            exc.read().decode("utf-8", errors="replace"),
        )
        return None
    except Exception as exc:
        logger.warning(
            "Self-improve: failed to submit job to orchestrator at %s: %s",
            url,
            exc,
        )
        return None


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def trigger_self_improvement(
    agent: Any,
    job_type: str,
    conversation_snapshot: Optional[Dict[str, Any]] = None,
) -> Optional[str]:
    """Trigger a sandboxed self-improvement job via the IronClaw orchestrator.

    This is the main entry point called from ``conversation_loop.py`` and
    ``curator.py``. It:

    1. Checks whether IronClaw should be used (see :func:`should_use_ironclaw`).
       IronClaw is preferred by default whenever the orchestrator is reachable;
       the caller falls back to the local Hermes fork only when this returns
       ``None``.
    2. Resolves the LLM client (auxiliary / main / local).
    3. Serializes and encrypts the conversation snapshot.
    4. Submits the job to the IronClaw orchestrator.
    5. Returns the job_id (non-blocking — the orchestrator manages the container).

    Returns the job_id string on success, or ``None`` when IronClaw is not
    available / opted-out (caller should fall back to local review).

    Args:
        agent: The parent AIAgent instance.
        job_type: One of JOB_TYPE_* constants.
        conversation_snapshot: Optional dict with conversation context.
            If None, a minimal snapshot is built from the agent state.
    """
    if not should_use_ironclaw():
        # Orchestrator not reachable or caller opted out — signal fallback.
        return None

    # Resolve LLM client.
    llm_mode = os.environ.get("SELF_IMPROVE_LLM_CLIENT", "auxiliary").lower().strip()
    llm_result = _resolve_llm_client(agent)
    if llm_result is None:
        return None  # Warning already logged by _resolve_llm_client.

    llm_provider, llm_model, llm_base_url = llm_result

    # Build snapshot if not provided.
    if conversation_snapshot is None:
        conversation_snapshot = _build_minimal_snapshot(agent)

    # Submit to orchestrator.
    job_id = _submit_job_to_orchestrator(
        job_type=job_type,
        snapshot=conversation_snapshot,
        llm_provider=llm_provider,
        llm_model=llm_model,
        llm_base_url=llm_base_url,
        llm_mode=llm_mode,
    )

    return job_id


def trigger_self_improvement_async(
    agent: Any,
    job_type: str,
    conversation_snapshot: Optional[Dict[str, Any]] = None,
) -> None:
    """Trigger a sandboxed self-improvement job in a background thread.

    Non-blocking wrapper around ``trigger_self_improvement``. Errors are
    logged but do not propagate to the caller.
    """
    def _run() -> None:
        try:
            trigger_self_improvement(agent, job_type, conversation_snapshot)
        except Exception as exc:
            logger.warning(
                "Self-improve: background trigger failed: %s", exc, exc_info=True
            )

    t = threading.Thread(target=_run, daemon=True, name="self-improve-dispatcher")
    t.start()


def _build_minimal_snapshot(agent: Any) -> Dict[str, Any]:
    """Build a minimal conversation snapshot from the agent state."""
    return {
        "session_id": getattr(agent, "session_id", str(uuid4())),
        "model": getattr(agent, "model", "unknown"),
        "provider": getattr(agent, "provider", "unknown"),
        "timestamp": datetime.now(timezone.utc).isoformat(),
        # Include the last few messages for context (not the full history).
        "recent_messages": _get_recent_messages(agent, max_messages=10),
    }


def _get_recent_messages(agent: Any, max_messages: int = 10) -> List[Dict[str, Any]]:
    """Extract the last N messages from the agent's conversation history."""
    try:
        messages = getattr(agent, "messages", []) or []
        # Only include role + content (no tool call details — reduce snapshot size).
        recent = messages[-max_messages:]
        return [
            {
                "role": m.get("role", "unknown") if isinstance(m, dict) else "unknown",
                "content": (
                    m.get("content", "")[:2048] if isinstance(m, dict) else str(m)[:2048]
                ),
            }
            for m in recent
        ]
    except Exception:
        return []
