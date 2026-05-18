/// Integration tests for the `ironclaw_self_improve_dispatcher` crate.
///
/// Tests:
/// - AES-256-GCM encryption (no plaintext fallback)
/// - LLM client resolution (typed enum)
/// - Snapshot serialization (serde_json, no pickle)
/// - Rollback manager (commit/rollback/idempotency)
/// - `should_use_ironclaw` decision logic

use ironclaw_self_improve_dispatcher::{
    config::DispatcherConfig,
    crypto::encrypt_snapshot,
    rollback::RollbackManager,
    snapshot::build_minimal_snapshot,
    types::{AgentInfo, DispatchResult, JobType, LlmClientMode, Message},
};

// ---------------------------------------------------------------------------
// AES-256-GCM encryption tests
// ---------------------------------------------------------------------------

#[test]
fn encrypt_snapshot_no_plaintext_fallback() {
    let payload = b"sensitive conversation data";
    let result = encrypt_snapshot(payload).expect("encryption must succeed");

    // key_id must never be "plaintext" — that was the Python fallback.
    assert_ne!(result.key_id, "plaintext", "plaintext fallback must not exist");
    assert_eq!(result.key_id.len(), 16, "key_id must be 16 hex chars");
    assert!(!result.ciphertext.is_empty(), "ciphertext must not be empty");
    assert!(!result.nonce.is_empty(), "nonce must not be empty");
}

#[test]
fn encrypt_snapshot_produces_different_ciphertext_each_call() {
    let payload = b"same payload";
    let r1 = encrypt_snapshot(payload).unwrap();
    let r2 = encrypt_snapshot(payload).unwrap();

    // Different ephemeral keys → different key_ids and ciphertexts.
    assert_ne!(r1.key_id, r2.key_id, "each call must use a fresh key");
    assert_ne!(r1.ciphertext, r2.ciphertext, "ciphertext must differ with different keys");
    assert_ne!(r1.nonce, r2.nonce, "nonce must differ each call");
}

#[test]
fn encrypt_snapshot_ciphertext_is_base64() {
    let payload = b"test";
    let result = encrypt_snapshot(payload).unwrap();

    // Verify ciphertext and nonce are valid base64.
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    BASE64.decode(&result.ciphertext).expect("ciphertext must be valid base64");
    BASE64.decode(&result.nonce).expect("nonce must be valid base64");
}

#[test]
fn encrypt_snapshot_ciphertext_longer_than_plaintext() {
    // AES-256-GCM adds a 16-byte auth tag, so ciphertext > plaintext.
    let payload = b"hello";
    let result = encrypt_snapshot(payload).unwrap();

    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    let ciphertext_bytes = BASE64.decode(&result.ciphertext).unwrap();
    // ciphertext = plaintext + 16-byte GCM auth tag.
    assert_eq!(ciphertext_bytes.len(), payload.len() + 16);
}

// ---------------------------------------------------------------------------
// LLM client resolution tests
// ---------------------------------------------------------------------------

#[test]
fn llm_client_mode_from_str() {
    assert_eq!(LlmClientMode::from_str("main"), LlmClientMode::Main);
    assert_eq!(LlmClientMode::from_str("local"), LlmClientMode::Local);
    assert_eq!(LlmClientMode::from_str("auxiliary"), LlmClientMode::Auxiliary);
    assert_eq!(LlmClientMode::from_str("MAIN"), LlmClientMode::Main);
    assert_eq!(LlmClientMode::from_str("unknown"), LlmClientMode::Auxiliary);
    assert_eq!(LlmClientMode::from_str(""), LlmClientMode::Auxiliary);
}

#[test]
fn job_type_from_str() {
    assert_eq!(JobType::from_str("MEMORY_REVIEW"), Some(JobType::MemoryReview));
    assert_eq!(JobType::from_str("SKILL_REVIEW"), Some(JobType::SkillReview));
    assert_eq!(JobType::from_str("CURATOR_RUN"), Some(JobType::CuratorRun));
    assert_eq!(JobType::from_str("SWE_TASK"), Some(JobType::SweTask));
    assert_eq!(JobType::from_str("UNKNOWN"), None);
}

#[test]
fn job_type_as_str_roundtrip() {
    for jt in &[
        JobType::MemoryReview,
        JobType::SkillReview,
        JobType::CuratorRun,
        JobType::SweTask,
    ] {
        let s = jt.as_str();
        let parsed = JobType::from_str(s).expect("roundtrip must succeed");
        assert_eq!(&parsed, jt);
    }
}

// ---------------------------------------------------------------------------
// Snapshot serialization tests
// ---------------------------------------------------------------------------

#[test]
fn build_minimal_snapshot_contains_required_fields() {
    let agent = AgentInfo {
        session_id: "test-session-123".to_string(),
        provider: "anthropic".to_string(),
        model: "claude-3-5-sonnet".to_string(),
        base_url: None,
        recent_messages: vec![],
    };
    let msgs = vec![
        Message { role: "user".to_string(), content: "hello".to_string() },
        Message { role: "assistant".to_string(), content: "hi".to_string() },
    ];

    let snap = build_minimal_snapshot(&agent, &msgs);

    assert_eq!(snap["session_id"], "test-session-123");
    assert_eq!(snap["provider"], "anthropic");
    assert_eq!(snap["model"], "claude-3-5-sonnet");
    assert!(snap["timestamp"].as_str().unwrap().ends_with('Z'));
    assert_eq!(snap["recent_messages"].as_array().unwrap().len(), 2);
}

#[test]
fn build_minimal_snapshot_no_pickle_risk() {
    // Verify the snapshot is a serde_json::Value (not pickle-serializable).
    let agent = AgentInfo {
        session_id: "s".to_string(),
        provider: "p".to_string(),
        model: "m".to_string(),
        base_url: None,
        recent_messages: vec![],
    };
    let snap = build_minimal_snapshot(&agent, &[]);

    // Must be serializable to JSON without errors.
    let json_str = serde_json::to_string(&snap).expect("snapshot must serialize to JSON");
    assert!(!json_str.is_empty());

    // Must be deserializable back.
    let _: serde_json::Value = serde_json::from_str(&json_str).expect("must deserialize");
}

#[test]
fn build_minimal_snapshot_limits_messages() {
    let agent = AgentInfo {
        session_id: "s".to_string(),
        provider: "p".to_string(),
        model: "m".to_string(),
        base_url: None,
        recent_messages: vec![],
    };
    let msgs: Vec<Message> = (0..20)
        .map(|i| Message { role: "user".to_string(), content: format!("msg {}", i) })
        .collect();

    let snap = build_minimal_snapshot(&agent, &msgs);
    assert_eq!(snap["recent_messages"].as_array().unwrap().len(), 10);
}

// ---------------------------------------------------------------------------
// Rollback manager tests
// ---------------------------------------------------------------------------

#[test]
fn rollback_manager_commit_marks_committed() {
    let rm = RollbackManager::new("job-test-1".to_string(), Some("/tmp".to_string()));
    assert!(!rm.is_committed());
    assert!(rm.commit());
    assert!(rm.is_committed());
    assert!(!rm.is_rolled_back());
}

#[test]
fn rollback_manager_rollback_after_commit_fails() {
    let rm = RollbackManager::new("job-test-2".to_string(), Some("/tmp".to_string()));
    rm.commit();
    assert!(!rm.rollback("test"), "rollback after commit must fail");
}

#[test]
fn rollback_manager_commit_after_rollback_fails() {
    let rm = RollbackManager::new("job-test-3".to_string(), Some("/tmp".to_string()));
    rm.rollback("test");
    assert!(!rm.commit(), "commit after rollback must fail");
}

#[test]
fn rollback_manager_double_rollback_is_idempotent() {
    let rm = RollbackManager::new("job-test-4".to_string(), Some("/tmp".to_string()));
    assert!(rm.rollback("first"));
    assert!(rm.rollback("second"), "second rollback must return true (already rolled back)");
}

#[test]
fn rollback_manager_double_commit_is_idempotent() {
    let rm = RollbackManager::new("job-test-5".to_string(), Some("/tmp".to_string()));
    assert!(rm.commit());
    assert!(rm.commit(), "second commit must return true (already committed)");
}

#[test]
fn rollback_manager_snapshot_count() {
    let rm = RollbackManager::new("job-test-6".to_string(), Some("/tmp".to_string()));
    assert_eq!(rm.snapshot_count(), 0);
    rm.snapshot_skill("skill1", Some("content1".to_string()), "evt-1");
    assert_eq!(rm.snapshot_count(), 1);
    rm.snapshot_skill("skill2", None, "evt-2");
    assert_eq!(rm.snapshot_count(), 2);
}

#[test]
fn rollback_manager_restores_file() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let skills_path = dir.path().to_str().unwrap().to_string();
    let skill_file = dir.path().join("my_skill.md");

    // Write initial content.
    fs::write(&skill_file, "original content").unwrap();

    let rm = RollbackManager::new("job-test-7".to_string(), Some(skills_path));
    rm.snapshot_skill("my_skill", Some("original content".to_string()), "evt-1");

    // Simulate a write.
    fs::write(&skill_file, "new content").unwrap();

    // Rollback should restore original.
    assert!(rm.rollback("test failure"));
    let restored = fs::read_to_string(&skill_file).unwrap();
    assert_eq!(restored, "original content");
}

#[test]
fn rollback_manager_deletes_new_file() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let skills_path = dir.path().to_str().unwrap().to_string();
    let skill_file = dir.path().join("new_skill.md");

    let rm = RollbackManager::new("job-test-8".to_string(), Some(skills_path));
    rm.snapshot_skill("new_skill", None, "evt-2");

    // Simulate creating the file.
    fs::write(&skill_file, "new skill content").unwrap();

    // Rollback should delete it.
    assert!(rm.rollback("test failure"));
    assert!(!skill_file.exists(), "new skill file must be deleted on rollback");
}

// ---------------------------------------------------------------------------
// DispatchResult tests
// ---------------------------------------------------------------------------

#[test]
fn dispatch_result_submitted() {
    let r = DispatchResult::submitted("job-123".to_string());
    assert_eq!(r.job_id, Some("job-123".to_string()));
    assert!(!r.skipped);
    assert!(r.error.is_none());
}

#[test]
fn dispatch_result_skipped() {
    let r = DispatchResult::skipped();
    assert!(r.job_id.is_none());
    assert!(r.skipped);
    assert!(r.error.is_none());
}

#[test]
fn dispatch_result_failed() {
    let r = DispatchResult::failed("connection refused".to_string());
    assert!(r.job_id.is_none());
    assert!(!r.skipped);
    assert_eq!(r.error, Some("connection refused".to_string()));
}

// ---------------------------------------------------------------------------
// Config tests
// ---------------------------------------------------------------------------

#[test]
fn dispatcher_config_defaults() {
    // Clear relevant env vars to test defaults.
    std::env::remove_var("IRONCLAW_ORCHESTRATOR_URL");
    std::env::remove_var("SELF_IMPROVE_LLM_CLIENT");
    std::env::remove_var("SELF_IMPROVE_MAX_TURNS");

    let config = DispatcherConfig::from_env();
    assert_eq!(config.orchestrator_url, "http://localhost:8080");
    assert_eq!(config.llm_client_mode, "auxiliary");
    assert_eq!(config.max_turns, 10);
    assert_eq!(config.max_wall_seconds, 120);
}

#[test]
fn dispatcher_config_health_url() {
    std::env::set_var("IRONCLAW_ORCHESTRATOR_URL", "http://example.com:9090/");
    let config = DispatcherConfig::from_env();
    assert_eq!(config.health_url(), "http://example.com:9090/health");
    std::env::remove_var("IRONCLAW_ORCHESTRATOR_URL");
}
