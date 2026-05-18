-- Self-improvement audit log table for PostgreSQL.
--
-- Records every self-modification (skill write, memory write) as an immutable
-- audit event. Rows are INSERT-only — never UPDATE or DELETE on committed rows.
--
-- Per IronClaw's "LLM data is never deleted" invariant, this table is append-only.
-- The only allowed status transitions are:
--   PENDING → COMMITTED
--   PENDING → ROLLED_BACK
--
-- Migration: V{next}__self_improvement_audit.sql

CREATE TABLE IF NOT EXISTS self_improvement_audit (
    -- Unique event identifier (UUID v4).
    event_id        UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Links to the orchestrator job that produced this write.
    job_id          UUID NOT NULL,

    -- Job type: MEMORY_REVIEW | SKILL_REVIEW | CURATOR_RUN | SWE_TASK
    job_type        TEXT NOT NULL CHECK (job_type IN ('MEMORY_REVIEW', 'SKILL_REVIEW', 'CURATOR_RUN', 'SWE_TASK')),

    -- When the write was proposed.
    timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- What was done: skill_create | skill_update | memory_save | memory_update
    action          TEXT NOT NULL,

    -- The target: skill name or memory key.
    target          TEXT NOT NULL,

    -- SHA-256 of content before the write (NULL = new file/entry).
    before_hash     TEXT,

    -- SHA-256 of content after the write.
    after_hash      TEXT NOT NULL,

    -- Safety layer verdict: PASS | FLAGGED | BLOCKED
    safety_verdict  TEXT NOT NULL DEFAULT 'PASS'
                    CHECK (safety_verdict IN ('PASS', 'FLAGGED', 'BLOCKED')),

    -- HDC DSV quality score [0.0, 1.0] (NULL = not scored / HDC disabled).
    hdc_score       REAL,

    -- Which LLM model produced this write.
    llm_model       TEXT NOT NULL DEFAULT '',

    -- Docker container ID that ran the job (empty = in-process WASM).
    container_id    TEXT NOT NULL DEFAULT '',

    -- Event status: PENDING | COMMITTED | ROLLED_BACK
    status          TEXT NOT NULL DEFAULT 'PENDING'
                    CHECK (status IN ('PENDING', 'COMMITTED', 'ROLLED_BACK')),

    -- User ID that owns this job (for multi-tenant scoping).
    user_id         TEXT NOT NULL DEFAULT ''
);

-- Index for querying all events for a job (used by rollback manager).
CREATE INDEX IF NOT EXISTS idx_self_improvement_audit_job_id
    ON self_improvement_audit (job_id);

-- Index for querying pending events (used by rollback trigger).
CREATE INDEX IF NOT EXISTS idx_self_improvement_audit_pending
    ON self_improvement_audit (job_id, status)
    WHERE status = 'PENDING';

-- Index for time-range queries (used by audit dashboard).
CREATE INDEX IF NOT EXISTS idx_self_improvement_audit_timestamp
    ON self_improvement_audit (timestamp DESC);

-- Prevent UPDATE/DELETE on committed rows (immutable audit invariant).
-- Only PENDING → COMMITTED and PENDING → ROLLED_BACK transitions are allowed.
CREATE OR REPLACE FUNCTION self_improvement_audit_immutability_check()
RETURNS TRIGGER AS $$
BEGIN
    -- Allow status transitions from PENDING only.
    IF OLD.status != 'PENDING' THEN
        RAISE EXCEPTION
            'self_improvement_audit: cannot modify committed/rolled-back event %',
            OLD.event_id;
    END IF;
    -- Only allow status field to change (no other field mutations).
    IF OLD.event_id != NEW.event_id
        OR OLD.job_id != NEW.job_id
        OR OLD.job_type != NEW.job_type
        OR OLD.action != NEW.action
        OR OLD.target != NEW.target
        OR OLD.after_hash != NEW.after_hash
    THEN
        RAISE EXCEPTION
            'self_improvement_audit: only status field may be updated on event %',
            OLD.event_id;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER self_improvement_audit_immutability
    BEFORE UPDATE ON self_improvement_audit
    FOR EACH ROW
    EXECUTE FUNCTION self_improvement_audit_immutability_check();

-- Prevent DELETE entirely.
CREATE RULE self_improvement_audit_no_delete AS
    ON DELETE TO self_improvement_audit
    DO INSTEAD NOTHING;

COMMENT ON TABLE self_improvement_audit IS
    'Immutable audit log for Hermes self-improvement writes. '
    'Rows are INSERT-only; status transitions PENDING→COMMITTED/ROLLED_BACK only.';
