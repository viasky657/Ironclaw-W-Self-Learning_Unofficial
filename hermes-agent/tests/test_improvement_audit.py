"""Tests for the self-improvement audit log.

Verifies:
- Every write produces an audit event
- Events have correct fields (job_id, action, target, hashes, status)
- AuditWriter correctly stores and retrieves events via libSQL backend
- mark_committed and mark_rolled_back work correctly
- sha256_hex produces correct hashes
"""

from __future__ import annotations

import hashlib
import os
import sqlite3
import tempfile
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from agent.improvement_audit import (
    AuditWriter,
    EVENT_STATUS_COMMITTED,
    EVENT_STATUS_PENDING,
    EVENT_STATUS_ROLLED_BACK,
    SAFETY_VERDICT_PASS,
    SAFETY_VERDICT_FLAGGED,
    SAFETY_VERDICT_BLOCKED,
    SelfImprovementEvent,
    get_audit_writer,
    record_write_event,
    sha256_hex,
)


# ---------------------------------------------------------------------------
# sha256_hex
# ---------------------------------------------------------------------------


def test_sha256_hex_correct():
    content = "Hello, world!"
    expected = hashlib.sha256(content.encode("utf-8")).hexdigest()
    assert sha256_hex(content) == expected


def test_sha256_hex_empty_string():
    expected = hashlib.sha256(b"").hexdigest()
    assert sha256_hex("") == expected


def test_sha256_hex_unicode():
    content = "日本語テスト"
    expected = hashlib.sha256(content.encode("utf-8")).hexdigest()
    assert sha256_hex(content) == expected


# ---------------------------------------------------------------------------
# SelfImprovementEvent dataclass
# ---------------------------------------------------------------------------


def test_event_default_status_is_pending():
    event = SelfImprovementEvent(
        job_id="job-123",
        job_type="SKILL_REVIEW",
        action="skill_create",
        target="my_skill",
        after_hash=sha256_hex("content"),
    )
    assert event.status == EVENT_STATUS_PENDING


def test_event_has_unique_event_id():
    e1 = SelfImprovementEvent()
    e2 = SelfImprovementEvent()
    assert e1.event_id != e2.event_id


def test_event_to_dict_roundtrip():
    event = SelfImprovementEvent(
        job_id="job-456",
        job_type="MEMORY_REVIEW",
        action="memory_save",
        target="user_preference",
        before_hash=None,
        after_hash=sha256_hex("new content"),
        safety_verdict=SAFETY_VERDICT_PASS,
        hdc_score=0.85,
        llm_model="gemini-flash",
        container_id="container-abc",
    )
    d = event.to_dict()
    restored = SelfImprovementEvent.from_dict(d)

    assert restored.event_id == event.event_id
    assert restored.job_id == event.job_id
    assert restored.action == event.action
    assert restored.target == event.target
    assert restored.hdc_score == event.hdc_score
    assert restored.status == event.status


# ---------------------------------------------------------------------------
# AuditWriter with libSQL backend
# ---------------------------------------------------------------------------


@pytest.fixture
def libsql_writer(tmp_path):
    """Create an AuditWriter backed by a temporary libSQL database."""
    db_path = str(tmp_path / "test_audit.db")
    with patch.dict(os.environ, {
        "IRONCLAW_AUDIT_BACKEND": "libsql",
        "LIBSQL_DB_PATH": db_path,
    }):
        writer = AuditWriter()
        yield writer


def test_libsql_insert_event(libsql_writer):
    event = SelfImprovementEvent(
        job_id="job-001",
        job_type="SKILL_REVIEW",
        action="skill_create",
        target="test_skill",
        after_hash=sha256_hex("skill content"),
    )
    result = libsql_writer.insert_event(event)
    assert result is True


def test_libsql_get_events_for_job(libsql_writer):
    job_id = "job-002"
    event = SelfImprovementEvent(
        job_id=job_id,
        job_type="SKILL_REVIEW",
        action="skill_create",
        target="skill_a",
        after_hash=sha256_hex("content a"),
    )
    libsql_writer.insert_event(event)

    events = libsql_writer.get_events_for_job(job_id)
    assert len(events) == 1
    assert events[0].event_id == event.event_id
    assert events[0].status == EVENT_STATUS_PENDING


def test_libsql_mark_committed(libsql_writer):
    job_id = "job-003"
    event = SelfImprovementEvent(
        job_id=job_id,
        job_type="SKILL_REVIEW",
        action="skill_create",
        target="skill_b",
        after_hash=sha256_hex("content b"),
    )
    libsql_writer.insert_event(event)
    libsql_writer.mark_committed(job_id)

    events = libsql_writer.get_events_for_job(job_id)
    assert events[0].status == EVENT_STATUS_COMMITTED


def test_libsql_mark_rolled_back(libsql_writer):
    job_id = "job-004"
    event = SelfImprovementEvent(
        job_id=job_id,
        job_type="MEMORY_REVIEW",
        action="memory_save",
        target="key_c",
        after_hash=sha256_hex("memory content"),
    )
    libsql_writer.insert_event(event)
    libsql_writer.mark_rolled_back(job_id)

    events = libsql_writer.get_events_for_job(job_id)
    assert events[0].status == EVENT_STATUS_ROLLED_BACK


def test_libsql_committed_rows_not_rolled_back(libsql_writer):
    """Committed rows must not be changed by a subsequent rollback."""
    job_id = "job-005"
    event = SelfImprovementEvent(
        job_id=job_id,
        job_type="SKILL_REVIEW",
        action="skill_update",
        target="skill_d",
        after_hash=sha256_hex("updated content"),
    )
    libsql_writer.insert_event(event)
    libsql_writer.mark_committed(job_id)
    libsql_writer.mark_rolled_back(job_id)  # Must be a no-op.

    events = libsql_writer.get_events_for_job(job_id)
    assert events[0].status == EVENT_STATUS_COMMITTED


def test_libsql_insert_or_ignore_immutability(libsql_writer):
    """Duplicate inserts must be silently ignored."""
    job_id = "job-006"
    event = SelfImprovementEvent(
        job_id=job_id,
        job_type="SKILL_REVIEW",
        action="skill_create",
        target="skill_e",
        after_hash=sha256_hex("content e"),
    )
    libsql_writer.insert_event(event)
    libsql_writer.mark_committed(job_id)
    libsql_writer.insert_event(event)  # Duplicate — must be ignored.

    events = libsql_writer.get_events_for_job(job_id)
    assert len(events) == 1
    assert events[0].status == EVENT_STATUS_COMMITTED


# ---------------------------------------------------------------------------
# record_write_event helper
# ---------------------------------------------------------------------------


def test_record_write_event_produces_audit_event(tmp_path):
    db_path = str(tmp_path / "record_test.db")
    with patch.dict(os.environ, {
        "IRONCLAW_AUDIT_BACKEND": "libsql",
        "LIBSQL_DB_PATH": db_path,
    }):
        # Reset the singleton.
        import agent.improvement_audit as _mod
        _mod._audit_writer = None

        event = record_write_event(
            job_id="job-007",
            job_type="SKILL_REVIEW",
            action="skill_create",
            target="my_skill",
            content_before=None,
            content_after="# My Skill\n\nDoes something useful.",
            safety_verdict=SAFETY_VERDICT_PASS,
            hdc_score=0.9,
            llm_model="gemini-flash",
            container_id="",
        )

        assert event.job_id == "job-007"
        assert event.action == "skill_create"
        assert event.target == "my_skill"
        assert event.before_hash is None
        assert event.after_hash == sha256_hex("# My Skill\n\nDoes something useful.")
        assert event.safety_verdict == SAFETY_VERDICT_PASS
        assert event.hdc_score == 0.9
        assert event.status == EVENT_STATUS_PENDING

        # Reset singleton after test.
        _mod._audit_writer = None


def test_record_write_event_with_before_hash(tmp_path):
    db_path = str(tmp_path / "record_test2.db")
    with patch.dict(os.environ, {
        "IRONCLAW_AUDIT_BACKEND": "libsql",
        "LIBSQL_DB_PATH": db_path,
    }):
        import agent.improvement_audit as _mod
        _mod._audit_writer = None

        event = record_write_event(
            job_id="job-008",
            job_type="SKILL_REVIEW",
            action="skill_update",
            target="existing_skill",
            content_before="# Old content",
            content_after="# New content",
        )

        assert event.before_hash == sha256_hex("# Old content")
        assert event.after_hash == sha256_hex("# New content")

        _mod._audit_writer = None
