"""Audit log for Hermes self-improvement events.

This module is a thin wrapper that delegates SHA-256 hashing and write event
recording to the Rust ``ironclaw_audit_py`` PyO3 extension module when available.

If the Rust extension is not built, it falls back to the pure-Python
implementation with a security warning.

## Security note

The Python fallback uses ``hashlib.sha256`` which can be monkey-patched.
The Rust extension uses ``sha2::Sha256`` which cannot be patched from Python.

Build the Rust extension to eliminate this vulnerability:

    cd ironclaw && cargo build --release -p ironclaw_audit_py

## Public API (unchanged from original)

- ``SelfImprovementEvent`` — dataclass (Python DTO, no logic)
- ``sha256_hex(content: str) -> str``
- ``record_write_event(...) -> SelfImprovementEvent``
- ``AuditWriter`` — delegates DB operations to Rust when available
- ``get_audit_writer() -> AuditWriter``
- ``EVENT_STATUS_PENDING``, ``EVENT_STATUS_COMMITTED``, ``EVENT_STATUS_ROLLED_BACK``
- ``SAFETY_VERDICT_PASS``, ``SAFETY_VERDICT_FLAGGED``, ``SAFETY_VERDICT_BLOCKED``
"""

from __future__ import annotations

import logging
import threading
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from typing import List, Optional
from uuid import uuid4

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Event status / safety verdict constants
# ---------------------------------------------------------------------------

EVENT_STATUS_PENDING = "PENDING"
EVENT_STATUS_COMMITTED = "COMMITTED"
EVENT_STATUS_ROLLED_BACK = "ROLLED_BACK"

SAFETY_VERDICT_PASS = "PASS"
SAFETY_VERDICT_FLAGGED = "FLAGGED"
SAFETY_VERDICT_BLOCKED = "BLOCKED"

# ---------------------------------------------------------------------------
# Load Rust extension or fall back to Python implementation
# ---------------------------------------------------------------------------

try:
    from ironclaw_audit_py import (  # type: ignore[import]
        sha256_hex_py as _sha256_hex_rust,
        record_write_event_py as _record_write_event_rust,
        mark_committed_py as _mark_committed_rust,
        mark_rolled_back_py as _mark_rolled_back_rust,
    )
    _RUST_BACKEND = True
    logger.debug("ironclaw_audit_py: using Rust backend")

except ImportError:
    logger.critical(
        "ironclaw_audit_py Rust extension not found. "
        "Running with the Python fallback which has known security vulnerabilities:\n"
        "  - sha256_hex() uses hashlib which can be monkey-patched\n"
        "Build the Rust extension: cd ironclaw && cargo build --release -p ironclaw_audit_py"
    )
    _RUST_BACKEND = False
    _sha256_hex_rust = None
    _record_write_event_rust = None
    _mark_committed_rust = None
    _mark_rolled_back_rust = None


# ---------------------------------------------------------------------------
# SelfImprovementEvent dataclass (Python DTO — no logic, just data)
# ---------------------------------------------------------------------------

@dataclass
class SelfImprovementEvent:
    """An immutable audit record for a single self-improvement write."""

    event_id: str = field(default_factory=lambda: str(uuid4()))
    job_id: str = ""
    job_type: str = ""
    timestamp: str = field(
        default_factory=lambda: datetime.now(timezone.utc).isoformat()
    )
    action: str = ""
    target: str = ""
    before_hash: Optional[str] = None
    after_hash: str = ""
    safety_verdict: str = SAFETY_VERDICT_PASS
    hdc_score: Optional[float] = None
    llm_model: str = ""
    container_id: str = ""
    status: str = EVENT_STATUS_PENDING

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, d: dict) -> "SelfImprovementEvent":
        return cls(**{k: v for k, v in d.items() if k in cls.__dataclass_fields__})


# ---------------------------------------------------------------------------
# SHA-256 hashing — delegates to Rust when available
# ---------------------------------------------------------------------------


def sha256_hex(content: str) -> str:
    """Compute SHA-256 hex digest of a string.

    Delegates to the Rust ``sha2::Sha256`` implementation when the
    ``ironclaw_audit_py`` extension is available (not monkey-patchable).
    Falls back to ``hashlib.sha256`` otherwise.
    """
    if _RUST_BACKEND:
        return _sha256_hex_rust(content)
    else:
        import hashlib
        return hashlib.sha256(content.encode("utf-8")).hexdigest()


# ---------------------------------------------------------------------------
# AuditWriter — delegates DB operations to Rust when available
# ---------------------------------------------------------------------------


class AuditWriter:
    """Writes self-improvement audit events to the configured backend.

    When the Rust extension is available, SHA-256 hashing and DB operations
    are delegated to Rust. The Python fallback uses the original implementation.
    """

    def __init__(self) -> None:
        if not _RUST_BACKEND:
            # Initialize the Python fallback backend.
            from agent._improvement_dispatcher_py import _env_bool  # type: ignore[import]
            import os
            self._backend = os.environ.get("IRONCLAW_AUDIT_BACKEND", "orchestrator").lower()
            self._lock = threading.Lock()
            self._libsql_conn = None
            if self._backend == "libsql":
                self._init_libsql_fallback()
        else:
            self._backend = "rust"
            self._lock = threading.Lock()
            self._libsql_conn = None

    def _init_libsql_fallback(self) -> None:
        """Initialize the local libSQL (SQLite) audit database (Python fallback only)."""
        import os
        import sqlite3
        from pathlib import Path
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
        except Exception as exc:
            logger.warning("Audit: failed to initialize libSQL backend: %s", exc)
            self._backend = "orchestrator"

    def insert_event(self, event: SelfImprovementEvent) -> bool:
        """Insert a new audit event. Returns True on success."""
        if _RUST_BACKEND:
            # Rust handles the insert via record_write_event_py.
            # This path is called when the event was already recorded by record_write_event().
            return True
        return self._insert_fallback(event)

    def mark_committed(self, job_id: str) -> bool:
        """Mark all PENDING events for a job as COMMITTED."""
        if _RUST_BACKEND:
            return _mark_committed_rust(job_id)
        return self._update_status_fallback(job_id, EVENT_STATUS_COMMITTED)

    def mark_rolled_back(self, job_id: str) -> bool:
        """Mark all PENDING events for a job as ROLLED_BACK."""
        if _RUST_BACKEND:
            return _mark_rolled_back_rust(job_id)
        return self._update_status_fallback(job_id, EVENT_STATUS_ROLLED_BACK)

    def get_events_for_job(self, job_id: str) -> List[SelfImprovementEvent]:
        """Retrieve all audit events for a job (Python fallback only)."""
        if _RUST_BACKEND:
            return []  # Rust backend doesn't expose a query endpoint yet.
        return self._get_events_fallback(job_id)

    def _insert_fallback(self, event: SelfImprovementEvent) -> bool:
        if self._backend == "libsql" and self._libsql_conn is not None:
            try:
                with self._lock:
                    self._libsql_conn.execute(
                        """INSERT OR IGNORE INTO self_improvement_audit
                           (event_id, job_id, job_type, timestamp, action, target,
                            before_hash, after_hash, safety_verdict, hdc_score,
                            llm_model, container_id, status)
                           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                        (event.event_id, event.job_id, event.job_type, event.timestamp,
                         event.action, event.target, event.before_hash, event.after_hash,
                         event.safety_verdict, event.hdc_score, event.llm_model,
                         event.container_id, event.status),
                    )
                    self._libsql_conn.commit()
                return True
            except Exception as exc:
                logger.warning("Audit: libSQL insert failed: %s", exc)
                return False
        return self._insert_via_orchestrator_fallback(event)

    def _insert_via_orchestrator_fallback(self, event: SelfImprovementEvent) -> bool:
        import json
        import os
        import urllib.request
        import urllib.error
        orchestrator_url = os.environ.get("IRONCLAW_ORCHESTRATOR_URL", "http://localhost:8080").rstrip("/")
        orchestrator_token = os.environ.get("IRONCLAW_ORCHESTRATOR_TOKEN", "")
        body = json.dumps(event.to_dict()).encode("utf-8")
        url = f"{orchestrator_url}/orchestrator/audit-event"
        try:
            req = urllib.request.Request(
                url, data=body, method="POST",
                headers={"Content-Type": "application/json",
                         "Authorization": f"Bearer {orchestrator_token}",
                         "Content-Length": str(len(body))},
            )
            with urllib.request.urlopen(req, timeout=5):
                pass
            return True
        except Exception as exc:
            logger.warning("Audit: orchestrator API insert failed: %s", exc)
            return False

    def _update_status_fallback(self, job_id: str, new_status: str) -> bool:
        if self._backend == "libsql" and self._libsql_conn is not None:
            try:
                with self._lock:
                    self._libsql_conn.execute(
                        "UPDATE self_improvement_audit SET status = ? WHERE job_id = ? AND status = 'PENDING'",
                        (new_status, job_id),
                    )
                    self._libsql_conn.commit()
                return True
            except Exception as exc:
                logger.warning("Audit: libSQL status update failed: %s", exc)
                return False
        import json
        import os
        import urllib.request
        orchestrator_url = os.environ.get("IRONCLAW_ORCHESTRATOR_URL", "http://localhost:8080").rstrip("/")
        orchestrator_token = os.environ.get("IRONCLAW_ORCHESTRATOR_TOKEN", "")
        body = json.dumps({"job_id": job_id, "status": new_status}).encode("utf-8")
        url = f"{orchestrator_url}/orchestrator/audit-status"
        try:
            req = urllib.request.Request(
                url, data=body, method="POST",
                headers={"Content-Type": "application/json",
                         "Authorization": f"Bearer {orchestrator_token}",
                         "Content-Length": str(len(body))},
            )
            with urllib.request.urlopen(req, timeout=5):
                pass
            return True
        except Exception as exc:
            logger.warning("Audit: orchestrator status update failed: %s", exc)
            return False

    def _get_events_fallback(self, job_id: str) -> List[SelfImprovementEvent]:
        if self._libsql_conn is None:
            return []
        try:
            with self._lock:
                cursor = self._libsql_conn.execute(
                    "SELECT * FROM self_improvement_audit WHERE job_id = ? ORDER BY timestamp",
                    (job_id,),
                )
                cols = [d[0] for d in cursor.description]
                return [SelfImprovementEvent.from_dict(dict(zip(cols, row))) for row in cursor.fetchall()]
        except Exception as exc:
            logger.warning("Audit: libSQL query failed: %s", exc)
            return []


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

    Delegates SHA-256 hashing and DB insert to Rust when available.
    Returns the created event (with status=PENDING).
    """
    if _RUST_BACKEND:
        event_id = _record_write_event_rust(
            job_id=job_id,
            job_type=job_type,
            action=action,
            target=target,
            content_before=content_before,
            content_after=content_after,
            safety_verdict=safety_verdict,
            hdc_score=hdc_score,
            llm_model=llm_model,
            container_id=container_id,
        )
        # Build the Python DTO for backward compatibility with callers.
        return SelfImprovementEvent(
            event_id=event_id or str(uuid4()),
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
    else:
        # Python fallback path.
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


__all__ = [
    "SelfImprovementEvent",
    "AuditWriter",
    "sha256_hex",
    "record_write_event",
    "get_audit_writer",
    "EVENT_STATUS_PENDING",
    "EVENT_STATUS_COMMITTED",
    "EVENT_STATUS_ROLLED_BACK",
    "SAFETY_VERDICT_PASS",
    "SAFETY_VERDICT_FLAGGED",
    "SAFETY_VERDICT_BLOCKED",
    "_RUST_BACKEND",
]
