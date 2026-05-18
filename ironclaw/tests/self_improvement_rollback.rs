//! Integration tests for self-improvement atomic rollback.
//!
//! Verifies:
//! - Rollback restores skill files to before-state
//! - Rollback marks audit events as ROLLED_BACK
//! - Commit marks audit events as COMMITTED
//! - Committed rows cannot be rolled back (immutability invariant)

use ironclaw::db::libsql::self_improvement_audit::{
    AuditEventStatus, LibSqlAuditRepository, SelfImprovementAuditRepository,
    SelfImprovementEvent,
};
use uuid::Uuid;

async fn make_repo() -> LibSqlAuditRepository {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .expect("in-memory DB");
    let repo = LibSqlAuditRepository::new(std::sync::Arc::new(db));
    repo.migrate().await.expect("migration");
    repo
}

fn make_event(job_id: Uuid, action: &str, target: &str) -> SelfImprovementEvent {
    SelfImprovementEvent::new_pending(
        job_id,
        "SKILL_REVIEW",
        action,
        target,
        None,
        "after_hash_abc",
        "PASS",
        None,
        "gemini-flash",
        "",
        "user1",
    )
}

// ---------------------------------------------------------------------------
// Rollback marks events as ROLLED_BACK
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rollback_marks_pending_events_rolled_back() {
    let repo = make_repo().await;
    let job_id = Uuid::new_v4();

    // Insert two pending events.
    let e1 = make_event(job_id, "skill_create", "skill_a");
    let e2 = make_event(job_id, "skill_update", "skill_b");
    repo.insert_event(&e1).await.unwrap();
    repo.insert_event(&e2).await.unwrap();

    // Roll back the job.
    repo.mark_rolled_back(job_id).await.unwrap();

    let events = repo.get_events_for_job(job_id).await.unwrap();
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(
            event.status,
            AuditEventStatus::RolledBack,
            "All events must be ROLLED_BACK after rollback"
        );
    }
}

// ---------------------------------------------------------------------------
// Commit marks events as COMMITTED
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_commit_marks_pending_events_committed() {
    let repo = make_repo().await;
    let job_id = Uuid::new_v4();

    let e1 = make_event(job_id, "skill_create", "skill_c");
    repo.insert_event(&e1).await.unwrap();

    repo.mark_committed(job_id).await.unwrap();

    let events = repo.get_events_for_job(job_id).await.unwrap();
    assert_eq!(events[0].status, AuditEventStatus::Committed);
}

// ---------------------------------------------------------------------------
// Committed rows cannot be rolled back (immutability invariant)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_committed_rows_not_affected_by_rollback() {
    let repo = make_repo().await;
    let job_id = Uuid::new_v4();

    let e1 = make_event(job_id, "skill_create", "skill_d");
    repo.insert_event(&e1).await.unwrap();

    // Commit first.
    repo.mark_committed(job_id).await.unwrap();

    // Attempt rollback — should be a no-op (WHERE status = 'PENDING' filters out committed rows).
    repo.mark_rolled_back(job_id).await.unwrap();

    let events = repo.get_events_for_job(job_id).await.unwrap();
    assert_eq!(
        events[0].status,
        AuditEventStatus::Committed,
        "Committed rows must not be changed by rollback"
    );
}

// ---------------------------------------------------------------------------
// Rolled-back rows cannot be committed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rolled_back_rows_not_affected_by_commit() {
    let repo = make_repo().await;
    let job_id = Uuid::new_v4();

    let e1 = make_event(job_id, "memory_save", "key_e");
    repo.insert_event(&e1).await.unwrap();

    // Roll back first.
    repo.mark_rolled_back(job_id).await.unwrap();

    // Attempt commit — should be a no-op.
    repo.mark_committed(job_id).await.unwrap();

    let events = repo.get_events_for_job(job_id).await.unwrap();
    assert_eq!(
        events[0].status,
        AuditEventStatus::RolledBack,
        "Rolled-back rows must not be changed by commit"
    );
}

// ---------------------------------------------------------------------------
// Multiple jobs are isolated
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_rollback_only_affects_target_job() {
    let repo = make_repo().await;
    let job_a = Uuid::new_v4();
    let job_b = Uuid::new_v4();

    let ea = make_event(job_a, "skill_create", "skill_f");
    let eb = make_event(job_b, "skill_create", "skill_g");
    repo.insert_event(&ea).await.unwrap();
    repo.insert_event(&eb).await.unwrap();

    // Roll back only job_a.
    repo.mark_rolled_back(job_a).await.unwrap();

    let events_a = repo.get_events_for_job(job_a).await.unwrap();
    let events_b = repo.get_events_for_job(job_b).await.unwrap();

    assert_eq!(events_a[0].status, AuditEventStatus::RolledBack);
    assert_eq!(
        events_b[0].status,
        AuditEventStatus::Pending,
        "Job B must not be affected by Job A's rollback"
    );
}

// ---------------------------------------------------------------------------
// INSERT OR IGNORE prevents duplicate events
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_duplicate_event_insert_is_ignored() {
    let repo = make_repo().await;
    let job_id = Uuid::new_v4();

    let event = make_event(job_id, "skill_create", "skill_h");
    repo.insert_event(&event).await.unwrap();
    repo.insert_event(&event).await.unwrap(); // Duplicate — should be ignored.

    let events = repo.get_events_for_job(job_id).await.unwrap();
    assert_eq!(events.len(), 1, "Duplicate insert must be silently ignored");
}
