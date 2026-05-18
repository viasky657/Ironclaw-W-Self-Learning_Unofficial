"""Rollback manager for Hermes self-improvement jobs.

This module is a thin wrapper that delegates to the Rust
``ironclaw_self_improve_dispatcher.PyRollbackManager`` when available.

If the Rust extension is not built, it falls back to the pure-Python
implementation in ``agent._improvement_rollback_py`` with a security warning.

## Security note

The Python fallback stores skill content in plain Python strings (not zeroed on drop).
The Rust implementation uses ``zeroize::Zeroizing`` to zero content_before on drop.

Build the Rust extension to eliminate this vulnerability:

    cd ironclaw && cargo build --release -p ironclaw_self_improve_dispatcher

## Public API (unchanged from original)

- ``RollbackManager`` — manages rollback for a single job
- ``SkillSnapshot`` — before-state snapshot dataclass
- ``get_rollback_manager(job_id, skills_path=None) -> RollbackManager``
- ``cleanup_rollback_manager(job_id) -> None``
- ``rollback_job(job_id, reason="manual rollback") -> bool``
"""

from __future__ import annotations

import logging
import threading
from dataclasses import dataclass
from typing import Dict, Optional

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Load Rust extension or fall back to Python implementation
# ---------------------------------------------------------------------------

try:
    from ironclaw_self_improve_dispatcher import PyRollbackManager as _RustRollbackManager  # type: ignore[import]
    _RUST_BACKEND = True
    logger.debug("improvement_rollback: using Rust RollbackManager backend")

except ImportError:
    logger.critical(
        "ironclaw_self_improve_dispatcher Rust extension not found. "
        "Running with the Python rollback fallback which has known security vulnerabilities:\n"
        "  - Skill content is stored in plain Python strings (not zeroed on drop)\n"
        "Build the Rust extension: cd ironclaw && cargo build --release -p ironclaw_self_improve_dispatcher"
    )
    from agent._improvement_rollback_py import (  # type: ignore[import]
        RollbackManager as _PyRollbackManager,
        SkillSnapshot,
        get_rollback_manager as _get_rollback_manager_py,
        cleanup_rollback_manager as _cleanup_rollback_manager_py,
        rollback_job as _rollback_job_py,
    )
    _RustRollbackManager = None
    _RUST_BACKEND = False


# ---------------------------------------------------------------------------
# SkillSnapshot dataclass (Python DTO — kept for backward compatibility)
# ---------------------------------------------------------------------------

@dataclass
class SkillSnapshot:
    """Before-state snapshot for a skill file."""
    skill_name: str
    file_path: str
    content_before: Optional[str]
    event_id: str


# ---------------------------------------------------------------------------
# RollbackManager — delegates to Rust when available
# ---------------------------------------------------------------------------


class RollbackManager:
    """Manages rollback of self-improvement writes for a single job.

    Delegates to ``ironclaw_self_improve_dispatcher.PyRollbackManager``
    (Rust, with ``zeroize::Zeroizing`` on content_before) when available.

    Usage::

        rm = RollbackManager(job_id="...", skills_path="/hermes-skills")

        # Before each write:
        rm.snapshot_skill("my_skill", content_before, event_id)

        # After all writes succeed:
        rm.commit()

        # On failure:
        rm.rollback()
    """

    def __init__(self, job_id: str, skills_path: Optional[str] = None) -> None:
        self.job_id = job_id
        if _RUST_BACKEND:
            self._inner = _RustRollbackManager(job_id, skills_path)
        else:
            self._inner = _PyRollbackManager(job_id, skills_path)

    def snapshot_skill(
        self,
        skill_name: str,
        content_before: Optional[str],
        event_id: str,
    ) -> None:
        """Record the before-state of a skill file."""
        self._inner.snapshot_skill(skill_name, content_before, event_id)

    def commit(self) -> bool:
        """Mark all writes as committed. Returns True on success."""
        return self._inner.commit()

    def rollback(self, reason: str = "job failure") -> bool:
        """Roll back all writes for this job. Returns True on success."""
        return self._inner.rollback(reason)

    @property
    def snapshot_count(self) -> int:
        return self._inner.snapshot_count

    @property
    def is_committed(self) -> bool:
        return self._inner.is_committed

    @property
    def is_rolled_back(self) -> bool:
        return self._inner.is_rolled_back


# ---------------------------------------------------------------------------
# Global rollback registry (keyed by job_id)
# ---------------------------------------------------------------------------

_rollback_managers: Dict[str, RollbackManager] = {}
_registry_lock = threading.Lock()


def get_rollback_manager(job_id: str, skills_path: Optional[str] = None) -> RollbackManager:
    """Get or create a rollback manager for a job."""
    with _registry_lock:
        if job_id not in _rollback_managers:
            _rollback_managers[job_id] = RollbackManager(
                job_id=job_id, skills_path=skills_path
            )
        return _rollback_managers[job_id]


def cleanup_rollback_manager(job_id: str) -> None:
    """Remove a rollback manager from the registry after the job completes."""
    with _registry_lock:
        _rollback_managers.pop(job_id, None)


def rollback_job(job_id: str, reason: str = "manual rollback") -> bool:
    """Roll back a job by ID.

    This is the entry point for the CLI command:
    ``hermes self-improve rollback --job-id <id>``
    """
    with _registry_lock:
        manager = _rollback_managers.get(job_id)

    if manager is None:
        if not _RUST_BACKEND:
            return _rollback_job_py(job_id, reason)
        logger.info(
            "Rollback: no active manager for job %s — "
            "no audit-log-based rollback available in Rust backend",
            job_id,
        )
        return False

    return manager.rollback(reason=reason)


__all__ = [
    "RollbackManager",
    "SkillSnapshot",
    "get_rollback_manager",
    "cleanup_rollback_manager",
    "rollback_job",
    "_RUST_BACKEND",
]
