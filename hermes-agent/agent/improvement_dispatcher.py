"""Self-improvement dispatcher for Hermes Agent.

This module is a thin wrapper that delegates to the Rust
``ironclaw_self_improve_dispatcher`` PyO3 extension module when available.

If the Rust extension is not built, it falls back to the pure-Python
implementation in ``agent._improvement_dispatcher_py`` with a security warning.

## Security note

The Python fallback has known security vulnerabilities:
- AES-256-GCM may silently degrade to base64 if ``cryptography`` is not installed.
- SHA-256 hashing uses ``hashlib`` which can be monkey-patched.
- Snapshot serialization uses ``json.dumps`` with no type enforcement.

Build the Rust extension to eliminate these vulnerabilities:

    cd ironclaw && cargo build --release -p ironclaw_self_improve_dispatcher

## Public API (unchanged from original)

- ``trigger_self_improvement(agent, job_type, conversation_snapshot=None) -> Optional[str]``
- ``trigger_self_improvement_async(agent, job_type, conversation_snapshot=None) -> None``
- ``should_use_ironclaw() -> bool``
- ``JOB_TYPE_MEMORY_REVIEW``, ``JOB_TYPE_SKILL_REVIEW``, ``JOB_TYPE_CURATOR_RUN``, ``JOB_TYPE_SWE_TASK``
"""

from __future__ import annotations

import logging
from typing import Any, Dict, Optional

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Job type constants (kept for backward compatibility)
# ---------------------------------------------------------------------------

JOB_TYPE_MEMORY_REVIEW = "MEMORY_REVIEW"
JOB_TYPE_SKILL_REVIEW = "SKILL_REVIEW"
JOB_TYPE_CURATOR_RUN = "CURATOR_RUN"
JOB_TYPE_SWE_TASK = "SWE_TASK"

# ---------------------------------------------------------------------------
# Load Rust extension or fall back to Python implementation
# ---------------------------------------------------------------------------

try:
    from ironclaw_self_improve_dispatcher import (  # type: ignore[import]
        trigger_self_improvement_py as _trigger_self_improvement_rust,
        trigger_self_improvement_async_py as _trigger_self_improvement_async_rust,
        should_use_ironclaw_py as should_use_ironclaw,
    )
    _RUST_BACKEND = True
    logger.debug("ironclaw_self_improve_dispatcher: using Rust backend")

except ImportError:
    logger.critical(
        "ironclaw_self_improve_dispatcher Rust extension not found. "
        "Running with the Python fallback which has known security vulnerabilities:\n"
        "  - AES-256-GCM may silently degrade to base64 if 'cryptography' is not installed\n"
        "  - SHA-256 hashing uses hashlib which can be monkey-patched\n"
        "Build the Rust extension: cd ironclaw && cargo build --release -p ironclaw_self_improve_dispatcher"
    )
    from agent._improvement_dispatcher_py import (  # type: ignore[import]
        trigger_self_improvement as _trigger_self_improvement_py,
        trigger_self_improvement_async as _trigger_self_improvement_async_py,
        should_use_ironclaw,
    )
    _RUST_BACKEND = False


# ---------------------------------------------------------------------------
# Public API wrappers
# ---------------------------------------------------------------------------


def trigger_self_improvement(
    agent: Any,
    job_type: str,
    conversation_snapshot: Optional[Dict[str, Any]] = None,
) -> Optional[str]:
    """Trigger a sandboxed self-improvement job via the IronClaw orchestrator.

    Returns the job_id string on success, or ``None`` when IronClaw is not
    available / opted-out (caller should fall back to local review).

    Args:
        agent: The parent AIAgent instance.
        job_type: One of JOB_TYPE_* constants.
        conversation_snapshot: Optional dict with conversation context.
    """
    if _RUST_BACKEND:
        result = _trigger_self_improvement_rust(agent, job_type, conversation_snapshot)
        if result.skipped or result.job_id is None:
            return None
        return result.job_id
    else:
        return _trigger_self_improvement_py(agent, job_type, conversation_snapshot)


def trigger_self_improvement_async(
    agent: Any,
    job_type: str,
    conversation_snapshot: Optional[Dict[str, Any]] = None,
) -> None:
    """Trigger a sandboxed self-improvement job in a background thread.

    Non-blocking wrapper around ``trigger_self_improvement``.
    """
    if _RUST_BACKEND:
        _trigger_self_improvement_async_rust(agent, job_type, conversation_snapshot)
    else:
        _trigger_self_improvement_async_py(agent, job_type, conversation_snapshot)


__all__ = [
    "JOB_TYPE_MEMORY_REVIEW",
    "JOB_TYPE_SKILL_REVIEW",
    "JOB_TYPE_CURATOR_RUN",
    "JOB_TYPE_SWE_TASK",
    "trigger_self_improvement",
    "trigger_self_improvement_async",
    "should_use_ironclaw",
    "_RUST_BACKEND",
]
