//! libSQL implementation of `SelfImprovementAuditRepository`.
//!
//! Provides an INSERT-only audit log for self-improvement events using the
//! embedded libSQL (SQLite) database. Enforces the immutability invariant:
//!
//! - `INSERT OR IGNORE`: never overwrites committed rows (event_id is PRIMARY KEY)
//! - Status updates use `UPDATE ... WHERE status = 'PENDING'`
//!   (committed/rolled-back rows are never touched again)
//! - No DELETE statements are ever issued
//!
//! WAL mode is enabled at connection open (in `LibSqlBackend::new_local`).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use libsql::Connection;
use uuid::Uuid;

use crate::error::DatabaseError;

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Status of a self-improvement audit event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEventStatus {
    Pending,
    Committed,
    RolledBack,
}

impl AuditEventStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Committed => "COMMITTED",
            Self::RolledBack => "ROLLED_BACK",
        }
    }
}

impl std::str::FromStr for AuditEventStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "PENDING" => Ok(Self::Pending),
            "COMMITTED" => Ok(Self::Committed),
            "ROLLED_BACK" => Ok(Self::RolledBack),
            _ => Err(format!("invalid audit event status: {}", s)),
        }
    }
}

/// A self-improvement audit event.
#[derive(Debug, Clone)]
pub struct SelfImprovementEvent {
    pub event_id: Uuid,
    pub job_id: Uuid,
    pub job_type: String,
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub target: String,
    pub before_hash: Option<String>,
    pub after_hash: String,
    pub safety_verdict: String,
    pub hdc_score: Option<f64>,
    pub llm_model: String,
    pub container_id: String,
    pub status: AuditEventStatus,
    pub user_id: String,
}

impl SelfImprovementEvent {
    /// Create a new PENDING event.
    pub fn new_pending(
        job_id: Uuid,
        job_type: impl Into<String>,
        action: impl Into<String>,
        target: impl Into<String>,
        before_hash: Option<String>,
        after_hash: impl Into<String>,
        safety_verdict: impl Into<String>,
        hdc_score: Option<f64>,
        llm_model: impl Into<String>,
        container_id: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            job_id,
            job_type: job_type.into(),
            timestamp: Utc::now(),
            action: action.into(),
            target: target.into(),
            before_hash,
            after_hash: after_hash.into(),
            safety_verdict: safety_verdict.into(),
            hdc_score,
            llm_model: llm_model.into(),
            container_id: container_id.into(),
            status: AuditEventStatus::Pending,
            user_id: user_id.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Repository trait
// ---------------------------------------------------------------------------

/// Repository trait for self-improvement audit events.
///
/// Two implementations exist:
/// - `LibSqlAuditRepository`: for local/embedded deployments
/// - `PostgresAuditRepository`: for cloud/multi-tenant deployments (in postgres.rs)
#[async_trait]
pub trait SelfImprovementAuditRepository: Send + Sync {
    /// Insert a new audit event (INSERT OR IGNORE — never overwrites).
    async fn insert_event(&self, event: &SelfImprovementEvent) -> Result<(), DatabaseError>;

    /// Get all events for a job, ordered by timestamp.
    async fn get_events_for_job(
        &self,
        job_id: Uuid,
    ) -> Result<Vec<SelfImprovementEvent>, DatabaseError>;

    /// Mark all PENDING events for a job as COMMITTED.
    async fn mark_committed(&self, job_id: Uuid) -> Result<(), DatabaseError>;

    /// Mark all PENDING events for a job as ROLLED_BACK.
    async fn mark_rolled_back(&self, job_id: Uuid) -> Result<(), DatabaseError>;
}

// ---------------------------------------------------------------------------
// libSQL implementation
// ---------------------------------------------------------------------------

/// libSQL implementation of `SelfImprovementAuditRepository`.
pub struct LibSqlAuditRepository {
    db: std::sync::Arc<libsql::Database>,
}

impl LibSqlAuditRepository {
    pub fn new(db: std::sync::Arc<libsql::Database>) -> Self {
        Self { db }
    }

    async fn conn(&self) -> Result<Connection, DatabaseError> {
        self.db
            .connect()
            .map_err(|e| DatabaseError::Pool(format!("libSQL connect failed: {}", e)))
    }

    /// Run the DDL migration to create the audit table if it doesn't exist.
    pub async fn migrate(&self) -> Result<(), DatabaseError> {
        let conn = self.conn().await?;
        conn.execute_batch(include_str!(
            "../../../../migrations/self_improvement_audit_libsql.sql"
        ))
        .await
        .map_err(|e| DatabaseError::Pool(format!("libSQL migration failed: {}", e)))?;
        Ok(())
    }
}

#[async_trait]
impl SelfImprovementAuditRepository for LibSqlAuditRepository {
    async fn insert_event(&self, event: &SelfImprovementEvent) -> Result<(), DatabaseError> {
        let conn = self.conn().await?;

        // INSERT OR IGNORE: if event_id already exists, silently skip.
        // This enforces the immutability invariant — committed rows can never
        // be overwritten by a re-insert.
        conn.execute(
            r#"
            INSERT OR IGNORE INTO self_improvement_audit
                (event_id, job_id, job_type, timestamp, action, target,
                 before_hash, after_hash, safety_verdict, hdc_score,
                 llm_model, container_id, status, user_id)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#,
            libsql::params![
                event.event_id.to_string(),
                event.job_id.to_string(),
                event.job_type.as_str(),
                event.timestamp.to_rfc3339(),
                event.action.as_str(),
                event.target.as_str(),
                event.before_hash.as_deref(),
                event.after_hash.as_str(),
                event.safety_verdict.as_str(),
                event.hdc_score,
                event.llm_model.as_str(),
                event.container_id.as_str(),
                event.status.as_str(),
                event.user_id.as_str(),
            ],
        )
        .await
        .map_err(|e| DatabaseError::Pool(format!("libSQL insert_event failed: {}", e)))?;

        Ok(())
    }

    async fn get_events_for_job(
        &self,
        job_id: Uuid,
    ) -> Result<Vec<SelfImprovementEvent>, DatabaseError> {
        let conn = self.conn().await?;

        let mut rows = conn
            .query(
                r#"
                SELECT event_id, job_id, job_type, timestamp, action, target,
                       before_hash, after_hash, safety_verdict, hdc_score,
                       llm_model, container_id, status, user_id
                FROM self_improvement_audit
                WHERE job_id = ?1
                ORDER BY timestamp ASC
                "#,
                libsql::params![job_id.to_string()],
            )
            .await
            .map_err(|e| DatabaseError::Pool(format!("libSQL get_events_for_job failed: {}", e)))?;

        let mut events = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| DatabaseError::Pool(format!("libSQL row iteration failed: {}", e)))?
        {
            let event = row_to_event(&row)?;
            events.push(event);
        }

        Ok(events)
    }

    async fn mark_committed(&self, job_id: Uuid) -> Result<(), DatabaseError> {
        let conn = self.conn().await?;

        // Only update PENDING rows — committed/rolled-back rows are immutable.
        conn.execute(
            r#"
            UPDATE self_improvement_audit
            SET status = 'COMMITTED'
            WHERE job_id = ?1 AND status = 'PENDING'
            "#,
            libsql::params![job_id.to_string()],
        )
        .await
        .map_err(|e| DatabaseError::Pool(format!("libSQL mark_committed failed: {}", e)))?;

        Ok(())
    }

    async fn mark_rolled_back(&self, job_id: Uuid) -> Result<(), DatabaseError> {
        let conn = self.conn().await?;

        // Only update PENDING rows — committed/rolled-back rows are immutable.
        conn.execute(
            r#"
            UPDATE self_improvement_audit
            SET status = 'ROLLED_BACK'
            WHERE job_id = ?1 AND status = 'PENDING'
            "#,
            libsql::params![job_id.to_string()],
        )
        .await
        .map_err(|e| DatabaseError::Pool(format!("libSQL mark_rolled_back failed: {}", e)))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Row deserialization
// ---------------------------------------------------------------------------

fn row_to_event(row: &libsql::Row) -> Result<SelfImprovementEvent, DatabaseError> {
    let event_id_str: String = row
        .get(0)
        .map_err(|e| DatabaseError::Pool(format!("event_id: {}", e)))?;
    let job_id_str: String = row
        .get(1)
        .map_err(|e| DatabaseError::Pool(format!("job_id: {}", e)))?;
    let job_type: String = row
        .get(2)
        .map_err(|e| DatabaseError::Pool(format!("job_type: {}", e)))?;
    let timestamp_str: String = row
        .get(3)
        .map_err(|e| DatabaseError::Pool(format!("timestamp: {}", e)))?;
    let action: String = row
        .get(4)
        .map_err(|e| DatabaseError::Pool(format!("action: {}", e)))?;
    let target: String = row
        .get(5)
        .map_err(|e| DatabaseError::Pool(format!("target: {}", e)))?;
    let before_hash: Option<String> = row
        .get(6)
        .map_err(|e| DatabaseError::Pool(format!("before_hash: {}", e)))?;
    let after_hash: String = row
        .get(7)
        .map_err(|e| DatabaseError::Pool(format!("after_hash: {}", e)))?;
    let safety_verdict: String = row
        .get(8)
        .map_err(|e| DatabaseError::Pool(format!("safety_verdict: {}", e)))?;
    let hdc_score: Option<f64> = row
        .get(9)
        .map_err(|e| DatabaseError::Pool(format!("hdc_score: {}", e)))?;
    let llm_model: String = row
        .get(10)
        .map_err(|e| DatabaseError::Pool(format!("llm_model: {}", e)))?;
    let container_id: String = row
        .get(11)
        .map_err(|e| DatabaseError::Pool(format!("container_id: {}", e)))?;
    let status_str: String = row
        .get(12)
        .map_err(|e| DatabaseError::Pool(format!("status: {}", e)))?;
    let user_id: String = row
        .get(13)
        .map_err(|e| DatabaseError::Pool(format!("user_id: {}", e)))?;

    let event_id = Uuid::parse_str(&event_id_str)
        .map_err(|e| DatabaseError::Pool(format!("invalid event_id UUID: {}", e)))?;
    let job_id = Uuid::parse_str(&job_id_str)
        .map_err(|e| DatabaseError::Pool(format!("invalid job_id UUID: {}", e)))?;
    let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| DatabaseError::Pool(format!("invalid timestamp: {}", e)))?;
    let status = status_str
        .parse::<AuditEventStatus>()
        .map_err(|e| DatabaseError::Pool(e))?;

    Ok(SelfImprovementEvent {
        event_id,
        job_id,
        job_type,
        timestamp,
        action,
        target,
        before_hash,
        after_hash,
        safety_verdict,
        hdc_score,
        llm_model,
        container_id,
        status,
        user_id,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_repo() -> LibSqlAuditRepository {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .expect("in-memory DB");
        let repo = LibSqlAuditRepository::new(std::sync::Arc::new(db));
        repo.migrate().await.expect("migration");
        repo
    }

    fn make_event(job_id: Uuid) -> SelfImprovementEvent {
        SelfImprovementEvent::new_pending(
            job_id,
            "SKILL_REVIEW",
            "skill_create",
            "my_skill",
            None,
            "abc123",
            "PASS",
            Some(0.85),
            "gemini-flash",
            "",
            "user1",
        )
    }

    #[tokio::test]
    async fn test_insert_and_query() {
        let repo = make_repo().await;
        let job_id = Uuid::new_v4();
        let event = make_event(job_id);

        repo.insert_event(&event).await.unwrap();

        let events = repo.get_events_for_job(job_id).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_id, event.event_id);
        assert_eq!(events[0].status, AuditEventStatus::Pending);
    }

    #[tokio::test]
    async fn test_insert_or_ignore_immutability() {
        let repo = make_repo().await;
        let job_id = Uuid::new_v4();
        let event = make_event(job_id);

        // First insert succeeds.
        repo.insert_event(&event).await.unwrap();

        // Second insert with same event_id is silently ignored (INSERT OR IGNORE).
        repo.insert_event(&event).await.unwrap();

        let events = repo.get_events_for_job(job_id).await.unwrap();
        assert_eq!(events.len(), 1, "Duplicate insert should be ignored");
    }

    #[tokio::test]
    async fn test_mark_committed() {
        let repo = make_repo().await;
        let job_id = Uuid::new_v4();
        let event = make_event(job_id);

        repo.insert_event(&event).await.unwrap();
        repo.mark_committed(job_id).await.unwrap();

        let events = repo.get_events_for_job(job_id).await.unwrap();
        assert_eq!(events[0].status, AuditEventStatus::Committed);
    }

    #[tokio::test]
    async fn test_mark_rolled_back() {
        let repo = make_repo().await;
        let job_id = Uuid::new_v4();
        let event = make_event(job_id);

        repo.insert_event(&event).await.unwrap();
        repo.mark_rolled_back(job_id).await.unwrap();

        let events = repo.get_events_for_job(job_id).await.unwrap();
        assert_eq!(events[0].status, AuditEventStatus::RolledBack);
    }

    #[tokio::test]
    async fn test_committed_rows_not_updated_by_rollback() {
        let repo = make_repo().await;
        let job_id = Uuid::new_v4();
        let event = make_event(job_id);

        repo.insert_event(&event).await.unwrap();
        repo.mark_committed(job_id).await.unwrap();

        // Attempting to roll back a committed job should be a no-op
        // (WHERE status = 'PENDING' filters out committed rows).
        repo.mark_rolled_back(job_id).await.unwrap();

        let events = repo.get_events_for_job(job_id).await.unwrap();
        assert_eq!(
            events[0].status,
            AuditEventStatus::Committed,
            "Committed rows must not be changed by rollback"
        );
    }
}
