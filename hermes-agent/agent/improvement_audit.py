"""Audit log for Hermes self-improvement events.

Every self-modification (skill write, memory write) is recorded as an
immutable audit event. Events are stored in IronClaw's database (PostgreSQL
or libSQL) and are never deleted — per IronClaw's "LLM data is never deleted"
invariant.

## Event lifecycle

1. ``PENDING``: Write has been proposed but not yet committed.
2. ``COMMITTED``: Write was applied successfully and passed all checks.
3. ``ROLLED_BACK``: Write was rolled back (timeout, non-zero exit, safety violation).

## Storage

Events are written to the IronClaw orchestrator's database via
``POST /orchestrator/audit-event``. The orchestrator persists them using
the configured database backend (PostgreSQL or libSQL).

For local-only deployments (``IRONCLAW_AUDIT_BACKEND=libsql``), events are
written directly to the libSQL database file at ``~/.ironclaw/ironclaw.db``.
"""

from __future__ import annotations

import hashlib
import json
import logging
import os
import sqlite3
import threading
import urllib.request
import urllib.error
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from pathlib import Path
from typing import List, Optional
from uuid import uuid4

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Event dataclass
# ---------------------------------------------------------------------------

EVENT_STATUS_PENDING = "PENDING"
EVENT_STATUS_COMMITTED = "COMMITTED"
EVENT_STATUS_ROLLED_BACK = "ROLLED_BACK"

SAFETY_VERDICT_PASS = "PASS"
SAFETY_VERDICT_FLAGGED = "FLAGGED"
SAFETY_VERDICT_BLOCKED = "BLOCKED"


@dataclass
class SelfImprovementEvent:
    """An immutable audit record for a single self-improvement write."""

    event_id: str = field(default_factory=lambda: str(uuid4()))
    job_id: str = ""
    job_type: str = ""  # MEMORY_REVIEW | SKILL_REVIEW | CURATOR_RUN | SWE_TASK
    timestamp: str = field(
        default_factory=lambda: datetime.now(timezone.utc).isoformat()
    )
    action: str = ""  # skill_create | skill_update | memory_save | memory_update
    target: str = ""  # skill name or memory key
    before_hash: Optional[str] = None  # SHA-256 of content before (None = new)
    after_hash: str = ""  # SHA-256 of content after
    safety_verdict: str = SAFETY_VERDICT_PASS
    hdc_score: Optional[float] = None  # HDC DSV quality score (None = not scored)
    llm_model: str = ""  # which model produced this
    container_id: str = ""  # Docker container that ran it (empty = in-process WASM)
    status: str = EVENT_STATUS_PENDING

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, d: dict) -> "SelfImprovementEvent":
        return cls(**{k: v for k, v in d.items() if k in cls.__dataclass_fields__})


def sha256_hex(content: str) -> str:
    """Compute SHA-256 hex digest of a string."""
    return hashlib.sha256(content.encode("utf-8")).hexdigest()


# ---------------------------------------------------------------------------
# Audit writer
# ---------------------------------------------------------------------------


class AuditWriter:
    """Writes self-improvement audit events to the configured backend.

    Backend selection (in priority order):
    1. ``IRONCLAW_AUDIT_BACKEND=libsql`` → local libSQL file
    2. ``IRONCLAW_AUDIT_BACKEND=postgres`` → orchestrator HTTP API
    3. Default: orchestrator HTTP API (``IRONCLAW_ORCHESTRATOR_URL``)
    """

    def __init__(self) -> None:
        self._backend = os.environ.get("IRONCLAW_AUDIT_BACKEND", "orchestrator").lower()
        self._lock = threading.Lock()
        self._libsql_conn: Optional[sqlite3.Connection] = None

        if self._backend == "libsql":
            self._init_libsql()

    def _init_libsql(self) -> None:
        """Initialize the local libSQL (SQLite) audit database."""
        db_path_str = os.environ.get(
            "LIBSQL_DB_PATH",
            str(Path.home() / ".ironclaw" / "ironclaw.db"),
        )
        db_path = Path(db_path_str)
        db_path.parent.mkdir(parents=True, exist_ok=True)

        try:
            conn = sqlite3.connect(str(db_path), check_same_thread=False)
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("PRAGMA foreign_keys=ON")
            conn.execute("""
                CREATE TABLE IF NOT EXISTS self_improvement_audit (
                    event_id TEXT PRIMARY KEY,
                    job_id TEXT NOT NULL,
                    job_type TEXT NOT NULL,
                    timestamp TEXT NOT NULL,
                    action TEXT NOT NULL,
                    target TEXT NOT NULL,
                    before_hash TEXT,
                    after_hash TEXT NOT NULL,
                    safety_verdict TEXT NOT NULL DEFAULT 'PASS',
                    hdc_score REAL,
                    llm_model TEXT NOT NULL DEFAULT '',
                    container_id TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'PENDING'
                )
            """)
            conn.commit()
            self._libsql_conn = conn
            logger.debug("Audit: libSQL backend initialized at %s", db_path)
        except Exception as exc:
            logger.warning(
                "Audit: failed to initialize libSQL backend at %s: %s — "
                "falling back to orchestrator API",
                db_path,
                exc,
            )
            self._backend = "orchestrator"

    def insert_event(self, event: SelfImprovementEvent) -> bool:
        """Insert a new audit event. Returns True on success."""
        if self._backend == "libsql":
            return self._insert_libsql(event)
        return self._insert_via_orchestrator(event)

    def mark_committed(self, job_id: str) -> bool:
        """Mark all PENDING events for a job as COMMITTED."""
        return self._update_status(job_id, EVENT_STATUS_COMMITTED)

    def mark_rolled_back(self, job_id: str) -> bool:
        """Mark all PENDING events for a job as ROLLED_BACK."""
        return self._update_status(job_id, EVENT_STATUS_ROLLED_BACK)

    def get_events_for_job(self, job_id: str) -> List[SelfImprovementEvent]:
        """Retrieve all audit events for a job."""
        if self._backend == "libsql":
            return self._get_events_libsql(job_id)
        return []  # Orchestrator API doesn't expose a query endpoint yet.

    # -- libSQL backend -------------------------------------------------------

    def _insert_libsql(self, event: SelfImprovementEvent) -> bool:
        if self._libsql_conn is None:
            return False
        try:
            with self._lock:
                self._libsql_conn.execute(
                    """
                    INSERT OR IGNORE INTO self_improvement_audit
                        (event_id, job_id, job_type, timestamp, action, target,
                         before_hash, after_hash, safety_verdict, hdc_score,
                         llm_model, container_id, status)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event.event_id,
                        event.job_id,
                        event.job_type,
                        event.timestamp,
                        event.action,
                        event.target,
                        event.before_hash,
                        event.after_hash,
                        event.safety_verdict,
                        event.hdc_score,
                        event.llm_model,
                        event.container_id,
                        event.status,
                    ),
                )
                self._libsql_conn.commit()
            return True
        except Exception as exc:
            logger.warning("Audit: libSQL insert failed: %s", exc)
            return False

    def _update_status(self, job_id: str, new_status: str) -> bool:
        if self._backend == "libsql" and self._libsql_conn is not None:
            try:
                with self._lock:
                    self._libsql_conn.execute(
                        """
                        UPDATE self_improvement_audit
                        SET status = ?
                        WHERE job_id = ? AND status = 'PENDING'
                        """,
                        (new_status, job_id),
                    )
                    self._libsql_conn.commit()
                return True
            except Exception as exc:
                logger.warning("Audit: libSQL status update failed: %s", exc)
                return False
        # Orchestrator API path.
        return self._post_status_update(job_id, new_status)

    def _get_events_libsql(self, job_id: str) -> List[SelfImprovementEvent]:
        if self._libsql_conn is None:
            return []
        try:
            with self._lock:
                cursor = self._libsql_conn.execute(
                    "SELECT * FROM self_improvement_audit WHERE job_id = ? ORDER BY timestamp",
                    (job_id,),
                )
                cols = [d[0] for d in cursor.description]
                return [
                    SelfImprovementEvent.from_dict(dict(zip(cols, row)))
                    for row in cursor.fetchall()
                ]
        except Exception as exc:
            logger.warning("Audit: libSQL query failed: %s", exc)
            return []

    # -- Orchestrator API backend ---------------------------------------------

    def _insert_via_orchestrator(self, event: SelfImprovementEvent) -> bool:
        orchestrator_url = os.environ.get(
            "IRONCLAW_ORCHESTRATOR_URL", "http://localhost:8080"
        ).rstrip("/")
        orchestrator_token = os.environ.get("IRONCLAW_ORCHESTRATOR_TOKEN", "")

        body = json.dumps(event.to_dict()).encode("utf-8")
        url = f"{orchestrator_url}/orchestrator/audit-event"

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
            with urllib.request.urlopen(req, timeout=5):
                pass
            return True
        except Exception as exc:
            logger.warning("Audit: orchestrator API insert failed: %s", exc)
            return False

    def _post_status_update(self, job_id: str, new_status: str) -> bool:
        orchestrator_url = os.environ.get(
            "IRONCLAW_ORCHESTRATOR_URL", "http://localhost:8080"
        ).rstrip("/")
        orchestrator_token = os.environ.get("IRONCLAW_ORCHESTRATOR_TOKEN", "")

        body = json.dumps({"job_id": job_id, "status": new_status}).encode("utf-8")
        url = f"{orchestrator_url}/orchestrator/audit-status"

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
            with urllib.request.urlopen(req, timeout=5):
                pass
            return True
        except Exception as exc:
            logger.warning("Audit: orchestrator status update failed: %s", exc)
            return False


# ---------------------------------------------------------------------------
# Module-level singleton
# ---------------------------------------------------------------------------

_audit_writer: Optional[AuditWriter] = None
_audit_writer_lock = threading.Lock()


def get_audit_writer() -> AuditWriter:
    """Get or create the module-level audit writer singleton."""
    global _audit_writer
    if _audit_writer is None:
        with _audit_writer_lock:
            if _audit_writer is None:
                _audit_writer = AuditWriter()
    return _audit_writer


def record_write_event(
    job_id: str,
    job_type: str,
    action: str,
    target: str,
    content_before: Optional[str],
    content_after: str,
    safety_verdict: str = SAFETY_VERDICT_PASS,
    hdc_score: Optional[float] = None,
    llm_model: str = "",
    container_id: str = "",
) -> SelfImprovementEvent:
    """Record a self-improvement write event in the audit log.

    Returns the created event (with status=PENDING).
    """
    event = SelfImprovementEvent(
        job_id=job_id,
        job_type=job_type,
        action=action,
        target=target,
        before_hash=sha256_hex(content_before) if content_before is not None else None,
        after_hash=sha256_hex(content_after),
        safety_verdict=safety_verdict,
        hdc_score=hdc_score,
        llm_model=llm_model,
        container_id=container_id,
        status=EVENT_STATUS_PENDING,
    )
    writer = get_audit_writer()
    writer.insert_event(event)
    return event
