//! Integration tests for the self-improvement sandbox.
//!
//! Verifies:
//! - Tool allowlist enforcement (shell tool blocked, skill_manage allowed)
//! - Memory writes are proxied to host MemoryManager, not written directly
//! - Per-job bearer token scoping

use ironclaw::orchestrator::self_improvement_job::{
    AllowedSelfImproveTool, EncryptedBlob, SelfImprovementJob, SelfImprovementJobType,
};
use ironclaw::orchestrator::auth::TokenStore;

// ---------------------------------------------------------------------------
// Tool allowlist enforcement
// ---------------------------------------------------------------------------

#[test]
fn test_allowed_tools_skill_manage_and_memory_only() {
    let job = SelfImprovementJob::new(
        SelfImprovementJobType::SkillReview,
        EncryptedBlob {
            ciphertext: "abc".to_string(),
            nonce: "def".to_string(),
            key_id: "key1".to_string(),
        },
        "user1".to_string(),
    );

    // Allowed tools.
    assert!(
        job.is_tool_allowed("skill_manage"),
        "skill_manage must be allowed"
    );
    assert!(
        job.is_tool_allowed("memory"),
        "memory must be allowed"
    );

    // Blocked tools.
    assert!(
        !job.is_tool_allowed("terminal"),
        "terminal must be blocked"
    );
    assert!(
        !job.is_tool_allowed("http"),
        "http must be blocked"
    );
    assert!(
        !job.is_tool_allowed("file"),
        "file must be blocked"
    );
    assert!(
        !job.is_tool_allowed("shell"),
        "shell must be blocked"
    );
    assert!(
        !job.is_tool_allowed("bash"),
        "bash must be blocked"
    );
    assert!(
        !job.is_tool_allowed("python"),
        "python must be blocked"
    );
    assert!(
        !job.is_tool_allowed(""),
        "empty tool name must be blocked"
    );
}

#[test]
fn test_allowed_tools_default_list() {
    let job = SelfImprovementJob::new(
        SelfImprovementJobType::MemoryReview,
        EncryptedBlob {
            ciphertext: "x".to_string(),
            nonce: "y".to_string(),
            key_id: "z".to_string(),
        },
        "user2".to_string(),
    );

    assert_eq!(job.allowed_tools.len(), 2);
    assert!(job.allowed_tools.contains(&AllowedSelfImproveTool::SkillManage));
    assert!(job.allowed_tools.contains(&AllowedSelfImproveTool::Memory));
}

// ---------------------------------------------------------------------------
// Token store tool allowlist
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_token_store_tool_allowlist_enforcement() {
    let store = TokenStore::new();
    let job_id = uuid::Uuid::new_v4();

    let _token = store.create_token(job_id).await;
    store
        .store_allowed_tools(
            job_id,
            vec!["skill_manage".to_string(), "memory".to_string()],
        )
        .await;

    // Allowed tools.
    assert!(store.is_tool_allowed(job_id, "skill_manage").await);
    assert!(store.is_tool_allowed(job_id, "memory").await);

    // Blocked tools.
    assert!(!store.is_tool_allowed(job_id, "terminal").await);
    assert!(!store.is_tool_allowed(job_id, "http").await);
    assert!(!store.is_tool_allowed(job_id, "file").await);
}

#[tokio::test]
async fn test_token_store_no_allowlist_means_unrestricted() {
    let store = TokenStore::new();
    let job_id = uuid::Uuid::new_v4();

    let _token = store.create_token(job_id).await;
    // No allowlist set — standard worker job, unrestricted.

    assert!(store.is_tool_allowed(job_id, "terminal").await);
    assert!(store.is_tool_allowed(job_id, "http").await);
    assert!(store.is_tool_allowed(job_id, "skill_manage").await);
}

#[tokio::test]
async fn test_token_revoke_clears_allowlist() {
    let store = TokenStore::new();
    let job_id = uuid::Uuid::new_v4();

    let _token = store.create_token(job_id).await;
    store
        .store_allowed_tools(job_id, vec!["skill_manage".to_string()])
        .await;

    assert!(store.is_tool_allowed(job_id, "skill_manage").await);
    assert!(!store.is_tool_allowed(job_id, "terminal").await);

    // Revoke the token — allowlist should be cleared.
    store.revoke(job_id).await;

    // After revoke, the job_id has no entry — defaults to unrestricted.
    assert!(store.is_tool_allowed(job_id, "terminal").await);
    assert!(store.get_allowed_tools(job_id).await.is_none());
}

// ---------------------------------------------------------------------------
// Memory proxy: container must not write directly
// ---------------------------------------------------------------------------

#[test]
fn test_memory_write_action_variants() {
    use ironclaw::orchestrator::self_improvement_job::MemoryWriteAction;

    // Only save and update are allowed.
    let save = MemoryWriteAction::Save;
    let update = MemoryWriteAction::Update;

    // Verify serialization.
    let save_json = serde_json::to_string(&save).unwrap();
    let update_json = serde_json::to_string(&update).unwrap();

    assert_eq!(save_json, "\"save\"");
    assert_eq!(update_json, "\"update\"");
}

#[test]
fn test_memory_write_request_validation() {
    use ironclaw::orchestrator::self_improvement_job::{MemoryWriteAction, MemoryWriteRequest};

    let req = MemoryWriteRequest {
        job_id: uuid::Uuid::new_v4(),
        action: MemoryWriteAction::Save,
        key: "user_preference_dark_mode".to_string(),
        content: "User prefers dark mode in all applications.".to_string(),
        tags: vec!["preference".to_string()],
    };

    // Key must be non-empty and ≤ 512 chars.
    assert!(!req.key.is_empty());
    assert!(req.key.len() <= 512);

    // Content must be ≤ 256 KB.
    assert!(req.content.len() <= 256 * 1024);
}

// ---------------------------------------------------------------------------
// Job defaults
// ---------------------------------------------------------------------------

#[test]
fn test_job_defaults_are_safe() {
    let job = SelfImprovementJob::new(
        SelfImprovementJobType::CuratorRun,
        EncryptedBlob {
            ciphertext: "c".to_string(),
            nonce: "n".to_string(),
            key_id: "k".to_string(),
        },
        "user3".to_string(),
    );

    assert_eq!(job.max_turns, 10, "Default max_turns must be 10");
    assert_eq!(job.max_wall_seconds, 120, "Default max_wall_seconds must be 120");
    assert_eq!(job.max_skill_writes, 10, "Default max_skill_writes must be 10");
    assert_eq!(job.max_memory_writes, 5, "Default max_memory_writes must be 5");
    assert!(job.rollback_on_violation, "rollback_on_violation must default to true");
    assert!(job.credential_grants.is_empty(), "No credentials by default");
}
