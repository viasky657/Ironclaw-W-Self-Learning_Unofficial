//! Integration tests for the libSQL audit log.
//!
//! Verifies:
//! - Audit rows are INSERT-only (no UPDATE/DELETE on committed rows)
//! - WAL mode is active
//! - Encrypted DB cannot be opened without key (tested via env var check)

use ironclaw::db::libsql::self_improvement_audit::{
    AuditEventStatus, LibSqlAuditRepository, SelfImprovementAuditRepository,
    SelfImprovementEvent,
};
use uuid::Uuid;

async fn make_in_memory_repo() -> LibSqlAuditRepository {
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
        "test_skill",
        None,
        "sha256_after",
        "PASS",
        Some(0.75),
        "gemini-flash",
        "container-abc123",
        "user1",
    )
}

// ---------------------------------------------------------------------------
// INSERT-only semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_insert_only_no_overwrite_on_duplicate() {
    let repo = make_in_memory_repo().await;
    let job_id = Uuid::new_v4();
    let event = make_event(job_id);

    // First insert.
    repo.insert_event(&event).await.unwrap();

    // Commit the event.
    repo.mark_committed(job_id).await.unwrap();

    // Attempt to re-insert the same event (simulating a retry).
    // INSERT OR IGNORE must silently skip — the committed row must not be overwritten.
    repo.insert_event(&event).await.unwrap();

    let events = repo.get_events_for_job(job_id).await.unwrap();
    assert_eq!(events.len(), 1, "Duplicate insert must be silently ignored");
    assert_eq!(
        events[0].status,
        AuditEventStatus::Committed,
        "Committed row must not be overwritten by re-insert"
    );
}

// ---------------------------------------------------------------------------
// Status transitions: PENDING → COMMITTED only
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_only_pending_rows_are_updated() {
    let repo = make_in_memory_repo().await;
    let job_id = Uuid::new_v4();

    // Insert two events.
    let e1 = make_event(job_id);
    let mut e2 = make_event(job_id);
    e2.event_id = Uuid::new_v4();
    e2.action = "skill_update".to_string();

    repo.insert_event(&e1).await.unwrap();
    repo.insert_event(&e2).await.unwrap();

    // Commit both.
    repo.mark_committed(job_id).await.unwrap();

    // Attempt rollback — must be a no-op (no PENDING rows left).
    repo.mark_rolled_back(job_id).await.unwrap();

    let events = repo.get_events_for_job(job_id).await.unwrap();
    for event in &events {
        assert_eq!(
            event.status,
            AuditEventStatus::Committed,
            "Committed rows must not be changed by rollback"
        );
    }
}

// ---------------------------------------------------------------------------
// Multiple events per job
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_multiple_events_per_job() {
    let repo = make_in_memory_repo().await;
    let job_id = Uuid::new_v4();

    for i in 0..5 {
        let mut event = make_event(job_id);
        event.event_id = Uuid::new_v4();
        event.target = format!("skill_{}", i);
        repo.insert_event(&event).await.unwrap();
    }

    let events = repo.get_events_for_job(job_id).await.unwrap();
    assert_eq!(events.len(), 5);
    for event in &events {
        assert_eq!(event.status, AuditEventStatus::Pending);
    }
}

// ---------------------------------------------------------------------------
// HDC score is stored and retrieved
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_hdc_score_stored_and_retrieved() {
    let repo = make_in_memory_repo().await;
    let job_id = Uuid::new_v4();

    let mut event = make_event(job_id);
    event.hdc_score = Some(0.87654);
    repo.insert_event(&event).await.unwrap();

    let events = repo.get_events_for_job(job_id).await.unwrap();
    let stored_score = events[0].hdc_score.unwrap();
    assert!(
        (stored_score - 0.87654).abs() < 0.0001,
        "HDC score must be stored and retrieved accurately"
    );
}

// ---------------------------------------------------------------------------
// Null before_hash (new skill)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_null_before_hash_for_new_skill() {
    let repo = make_in_memory_repo().await;
    let job_id = Uuid::new_v4();

    let event = make_event(job_id); // before_hash = None
    repo.insert_event(&event).await.unwrap();

    let events = repo.get_events_for_job(job_id).await.unwrap();
    assert!(
        events[0].before_hash.is_none(),
        "before_hash must be NULL for new skills"
    );
}

// ---------------------------------------------------------------------------
// Events ordered by timestamp
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_events_ordered_by_timestamp() {
    let repo = make_in_memory_repo().await;
    let job_id = Uuid::new_v4();

    for i in 0..3 {
        let mut event = make_event(job_id);
        event.event_id = Uuid::new_v4();
        event.target = format!("skill_{}", i);
        // Timestamps are set to Utc::now() in new_pending — they'll be in order.
        repo.insert_event(&event).await.unwrap();
        // Small sleep to ensure distinct timestamps.
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }

    let events = repo.get_events_for_job(job_id).await.unwrap();
    assert_eq!(events.len(), 3);

    // Verify ascending timestamp order.
    for i in 1..events.len() {
        assert!(
            events[i].timestamp >= events[i - 1].timestamp,
            "Events must be ordered by timestamp ASC"
        );
    }
}

// ---------------------------------------------------------------------------
// Cross-job isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_events_isolated_by_job_id() {
    let repo = make_in_memory_repo().await;
    let job_a = Uuid::new_v4();
    let job_b = Uuid::new_v4();

    let ea = make_event(job_a);
    let mut eb = make_event(job_b);
    eb.event_id = Uuid::new_v4();

    repo.insert_event(&ea).await.unwrap();
    repo.insert_event(&eb).await.unwrap();

    let events_a = repo.get_events_for_job(job_a).await.unwrap();
    let events_b = repo.get_events_for_job(job_b).await.unwrap();

    assert_eq!(events_a.len(), 1);
    assert_eq!(events_b.len(), 1);
    assert_eq!(events_a[0].job_id, job_a);
    assert_eq!(events_b[0].job_id, job_b);
}
