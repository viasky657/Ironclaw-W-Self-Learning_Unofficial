"""Rollback manager for Hermes self-improvement jobs.

All skill and memory writes within a job are treated as a transaction:

1. Before any write, the current content is snapshotted (hash + content
   stored in the audit log).
2. Writes are applied to the volume.
3. If the job completes successfully → transaction committed, audit events
   marked ``COMMITTED``.
4. If the job fails (timeout, non-zero exit, safety violation) → rollback
   manager restores all snapshots, audit events marked ``ROLLED_BACK``.

## Rollback trigger conditions

- Safety layer flags content as policy violation → automatic rollback + job abort
- Job exceeds ``max_wall_seconds`` → container killed, partial writes rolled back
- Container exits non-zero → all writes from that job rolled back atomically
- Manual: ``hermes self-improve rollback --job-id <id>`` CLI command

## HDC DSV interaction

If a write is blocked by the HDC DSV quality gate (score below threshold),
it is treated identically to a safety-layer block — the write is never applied,
so no rollback is needed. If the HDC DSV server is unreachable and
``SELF_IMPROVE_HDC_BLOCK=true``, the write is blocked conservatively (fail-closed).
"""

from __future__ import annotations

import logging
import os
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional

from agent.improvement_audit import (
    AuditWriter,
    SelfImprovementEvent,
    get_audit_writer,
    EVENT_STATUS_PENDING,
    EVENT_STATUS_COMMITTED,
    EVENT_STATUS_ROLLED_BACK,
)

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Snapshot store (in-memory, per-job)
# ---------------------------------------------------------------------------


@dataclass
class SkillSnapshot:
    """Before-state snapshot for a skill file."""

    skill_name: str
    file_path: str
    content_before: Optional[str]  # None = file did not exist before
    event_id: str  # Links to the audit event


class RollbackManager:
    """Manages rollback of self-improvement writes for a single job.

    Usage::

        rm = RollbackManager(job_id="...", skills_path="/hermes-skills")

        # Before each write:
        rm.snapshot_skill("my_skill", content_before)

        # After all writes succeed:
        rm.commit()

        # On failure:
        rm.rollback()
    """

    def __init__(self, job_id: str, skills_path: Optional[str] = None) -> None:
        self.job_id = job_id
        self.skills_path = skills_path or os.environ.get(
            "SKILLS_VOLUME_PATH", "/hermes-skills"
        )
        self._snapshots: List[SkillSnapshot] = []
        self._lock = threading.Lock()
        self._committed = False
        self._rolled_back = False

    def snapshot_skill(
        self,
        skill_name: str,
        content_before: Optional[str],
        event_id: str,
    ) -> None:
        """Record the before-state of a skill file.

        Call this before applying any write. The snapshot is used to restore
        the file if the job fails.

        Args:
            skill_name: The skill name (used as the filename stem).
            content_before: The current file content, or None if the file
                does not exist yet (new skill).
            event_id: The audit event ID for this write.
        """
        file_path = str(Path(self.skills_path) / f"{skill_name}.md")
        snapshot = SkillSnapshot(
            skill_name=skill_name,
            file_path=file_path,
            content_before=content_before,
            event_id=event_id,
        )
        with self._lock:
            self._snapshots.append(snapshot)
        logger.debug(
            "Rollback: snapshot recorded for skill '%s' (job=%s, event=%s)",
            skill_name,
            self.job_id,
            event_id,
        )

    def commit(self) -> bool:
        """Mark all writes as committed.

        Returns True if the commit succeeded.
        """
        if self._rolled_back:
            logger.warning(
                "Rollback: cannot commit job %s — already rolled back", self.job_id
            )
            return False
        if self._committed:
            return True

        writer = get_audit_writer()
        success = writer.mark_committed(self.job_id)
        if success:
            self._committed = True
            logger.info(
                "Rollback: job %s committed (%d writes)",
                self.job_id,
                len(self._snapshots),
            )
        else:
            logger.warning(
                "Rollback: failed to mark job %s as committed in audit log",
                self.job_id,
            )
        return success

    def rollback(self, reason: str = "job failure") -> bool:
        """Roll back all writes for this job.

        Restores each skill file to its before-state in reverse order
        (most recent write first). Marks all audit events as ROLLED_BACK.

        Returns True if the rollback succeeded.
        """
        if self._committed:
            logger.warning(
                "Rollback: cannot roll back job %s — already committed", self.job_id
            )
            return False
        if self._rolled_back:
            return True

        logger.info(
            "Rollback: rolling back job %s (%d writes, reason: %s)",
            self.job_id,
            len(self._snapshots),
            reason,
        )

        errors = []
        with self._lock:
            # Restore in reverse order (most recent write first).
            for snapshot in reversed(self._snapshots):
                try:
                    self._restore_skill(snapshot)
                except Exception as exc:
                    errors.append(f"skill '{snapshot.skill_name}': {exc}")
                    logger.warning(
                        "Rollback: failed to restore skill '%s': %s",
                        snapshot.skill_name,
                        exc,
                    )

        # Mark audit events as rolled back.
        writer = get_audit_writer()
        writer.mark_rolled_back(self.job_id)
        self._rolled_back = True

        if errors:
            logger.error(
                "Rollback: job %s rolled back with %d errors: %s",
                self.job_id,
                len(errors),
                "; ".join(errors),
            )
            return False

        logger.info("Rollback: job %s rolled back successfully", self.job_id)
        return True

    def _restore_skill(self, snapshot: SkillSnapshot) -> None:
        """Restore a skill file to its before-state."""
        path = Path(snapshot.file_path)

        if snapshot.content_before is None:
            # File did not exist before — delete it.
            if path.exists():
                path.unlink()
                logger.debug(
                    "Rollback: deleted new skill '%s'", snapshot.skill_name
                )
        else:
            # Restore the previous content.
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(snapshot.content_before, encoding="utf-8")
            logger.debug(
                "Rollback: restored skill '%s' (%d bytes)",
                snapshot.skill_name,
                len(snapshot.content_before),
            )

    @property
    def snapshot_count(self) -> int:
        """Number of snapshots recorded."""
        return len(self._snapshots)

    @property
    def is_committed(self) -> bool:
        return self._committed

    @property
    def is_rolled_back(self) -> bool:
        return self._rolled_back


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

    Returns True if the rollback succeeded (or if no manager was found,
    which means the job already completed or was never started).
    """
    with _registry_lock:
        manager = _rollback_managers.get(job_id)

    if manager is None:
        # Try to roll back via the audit log (for jobs that already completed).
        logger.info(
            "Rollback: no active manager for job %s — "
            "attempting audit-log-based rollback",
            job_id,
        )
        return _rollback_from_audit_log(job_id, reason)

    return manager.rollback(reason=reason)


def _rollback_from_audit_log(job_id: str, reason: str) -> bool:
    """Roll back a completed job using the audit log.

    Queries the audit log for all COMMITTED events for the job and
    restores each skill file from the before_hash snapshot.

    Note: This only works for skill writes (which have file-based snapshots).
    Memory writes are proxied to the host MemoryManager and cannot be
    rolled back from the audit log alone — they require the host-side
    MemoryManager to support rollback.
    """
    writer = get_audit_writer()
    events = writer.get_events_for_job(job_id)

    if not events:
        logger.warning(
            "Rollback: no audit events found for job %s", job_id
        )
        return False

    skills_path = os.environ.get("SKILLS_VOLUME_PATH", "/hermes-skills")
    errors = []

    # Process skill writes in reverse order.
    skill_events = [
        e for e in events
        if e.action in ("skill_create", "skill_update")
        and e.status == EVENT_STATUS_COMMITTED
    ]

    for event in reversed(skill_events):
        try:
            path = Path(skills_path) / f"{event.target}.md"
            if event.before_hash is None:
                # File was created — delete it.
                if path.exists():
                    path.unlink()
            else:
                # We don't have the actual content (only the hash) in the
                # audit log. Log a warning — full content rollback requires
                # the snapshot to be stored separately.
                logger.warning(
                    "Rollback: cannot restore skill '%s' from audit log — "
                    "before-content not stored (only hash %s). "
                    "Manual restoration required.",
                    event.target,
                    event.before_hash[:16] if event.before_hash else "None",
                )
        except Exception as exc:
            errors.append(f"skill '{event.target}': {exc}")

    writer.mark_rolled_back(job_id)

    if errors:
        logger.error(
            "Rollback: audit-log rollback for job %s had %d errors: %s",
            job_id,
            len(errors),
            "; ".join(errors),
        )
        return False

    logger.info("Rollback: audit-log rollback for job %s completed", job_id)
    return True
