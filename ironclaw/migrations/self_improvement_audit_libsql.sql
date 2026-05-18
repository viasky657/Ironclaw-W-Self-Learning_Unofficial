-- Self-improvement audit log table for libSQL (SQLite-compatible).
--
-- SQLite-compatible version of self_improvement_audit_postgres.sql.
-- Uses TEXT for UUIDs (no native UUID type in SQLite/libSQL).
-- Uses INTEGER for timestamps (Unix epoch milliseconds).
--
-- Immutability is enforced at the application layer (INSERT OR IGNORE semantics
-- in LibSqlAuditRepository) since SQLite triggers are more limited than PostgreSQL.
--
-- WAL mode is enabled at connection open (not in this DDL) for crash safety.

CREATE TABLE IF NOT EXISTS self_improvement_audit (
    -- Unique event identifier (UUID v4 as TEXT).
    event_id        TEXT PRIMARY KEY,

    -- Links to the orchestrator job that produced this write.
    job_id          TEXT NOT NULL,

    -- Job type: MEMORY_REVIEW | SKILL_REVIEW | CURATOR_RUN | SWE_TASK
    job_type        TEXT NOT NULL,

    -- When the write was proposed (ISO 8601 UTC string).
    timestamp       TEXT NOT NULL,

    -- What was done: skill_create | skill_update | memory_save | memory_update
    action          TEXT NOT NULL,

    -- The target: skill name or memory key.
    target          TEXT NOT NULL,

    -- SHA-256 of content before the write (NULL = new file/entry).
    before_hash     TEXT,

    -- SHA-256 of content after the write.
    after_hash      TEXT NOT NULL,

    -- Safety layer verdict: PASS | FLAGGED | BLOCKED
    safety_verdict  TEXT NOT NULL DEFAULT 'PASS',

    -- HDC DSV quality score [0.0, 1.0] (NULL = not scored / HDC disabled).
    hdc_score       REAL,

    -- Which LLM model produced this write.
    llm_model       TEXT NOT NULL DEFAULT '',

    -- Docker container ID that ran the job (empty = in-process WASM).
    container_id    TEXT NOT NULL DEFAULT '',

    -- Event status: PENDING | COMMITTED | ROLLED_BACK
    status          TEXT NOT NULL DEFAULT 'PENDING',

    -- User ID that owns this job (for multi-tenant scoping).
    user_id         TEXT NOT NULL DEFAULT ''
);

-- Index for querying all events for a job (used by rollback manager).
CREATE INDEX IF NOT EXISTS idx_self_improvement_audit_job_id
    ON self_improvement_audit (job_id);

-- Index for querying pending events (used by rollback trigger).
CREATE INDEX IF NOT EXISTS idx_self_improvement_audit_pending
    ON self_improvement_audit (job_id, status);

-- Index for time-range queries (used by audit dashboard).
CREATE INDEX IF NOT EXISTS idx_self_improvement_audit_timestamp
    ON self_improvement_audit (timestamp DESC);

-- Note: Immutability is enforced at the application layer:
--   - INSERT OR IGNORE: never overwrites committed rows (event_id is PRIMARY KEY)
--   - Status updates use: UPDATE ... WHERE status = 'PENDING'
--     (committed/rolled-back rows are never touched again)
--   - No DELETE statements are ever issued by LibSqlAuditRepository
