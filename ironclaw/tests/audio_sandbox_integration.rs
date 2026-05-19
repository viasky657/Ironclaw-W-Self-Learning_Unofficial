//! Integration tests for the audio sandbox.
//!
//! These tests verify:
//! - [`AudioSandboxConfig`] defaults and construction
//! - [`AudioSandboxManager`] consent gate enforcement
//! - [`AudioError`] variant coverage
//! - [`SandboxPolicy::AudioAccess`] properties
//! - [`AudioConfig`] resolution and validation
//! - Tool allowlist: audio tools are allowed, shell is blocked
//! - [`build_audio_tools`] returns the expected 7 tools

use ironclaw::sandbox::{
    AudioError, AudioSandboxConfig, AudioSandboxManager, SandboxPolicy,
};

// ── SandboxPolicy::AudioAccess ────────────────────────────────────────────────

#[test]
fn audio_access_policy_is_audio_access() {
    assert!(SandboxPolicy::AudioAccess.is_audio_access());
}

#[test]
fn audio_access_policy_is_not_desktop_access() {
    assert!(!SandboxPolicy::AudioAccess.is_desktop_access());
}

#[test]
fn audio_access_policy_is_not_self_improvement() {
    assert!(!SandboxPolicy::AudioAccess.is_self_improvement());
}

#[test]
fn audio_access_policy_allows_writes() {
    assert!(SandboxPolicy::AudioAccess.allows_writes());
}

#[test]
fn audio_access_policy_is_sandboxed() {
    assert!(SandboxPolicy::AudioAccess.is_sandboxed());
}

#[test]
fn audio_access_policy_has_no_full_network() {
    assert!(!SandboxPolicy::AudioAccess.has_full_network());
}

#[test]
fn audio_access_policy_writable_path() {
    assert_eq!(
        SandboxPolicy::AudioAccess.writable_path(),
        Some("/audio-workspace")
    );
}

#[test]
fn audio_access_policy_parses_from_str() {
    assert_eq!(
        "audio_access".parse::<SandboxPolicy>().unwrap(),
        SandboxPolicy::AudioAccess
    );
    assert_eq!(
        "audioaccess".parse::<SandboxPolicy>().unwrap(),
        SandboxPolicy::AudioAccess
    );
    assert_eq!(
        "audio".parse::<SandboxPolicy>().unwrap(),
        SandboxPolicy::AudioAccess
    );
    assert_eq!(
        "AUDIO".parse::<SandboxPolicy>().unwrap(),
        SandboxPolicy::AudioAccess
    );
}

#[test]
fn audio_access_policy_error_message_mentions_audio_access() {
    let err = "bogus_policy".parse::<SandboxPolicy>().unwrap_err();
    assert!(
        err.contains("audio_access"),
        "error message should mention audio_access, got: {err}"
    );
}

// ── AudioSandboxConfig defaults ───────────────────────────────────────────────

#[test]
fn audio_sandbox_config_defaults() {
    let cfg = AudioSandboxConfig::default();
    assert_eq!(cfg.image, "ironclaw-audio:latest");
    assert_eq!(cfg.memory_limit_mb, 2048);
    assert_eq!(cfg.cpu_shares, 1024);
    assert_eq!(cfg.max_record_secs, 120);
    assert_eq!(cfg.max_tts_chars, 4096);
    assert_eq!(cfg.stt_backend, "whisper_local");
    assert_eq!(cfg.tts_backend, "piper");
    assert_eq!(cfg.whisper_model, "base");
    assert_eq!(cfg.piper_voice, "en_US-lessac-medium");
    assert_eq!(cfg.stt_model, "whisper-1");
    assert_eq!(cfg.tts_model, "tts-1");
    assert_eq!(cfg.tts_voice, "alloy");
    assert!(cfg.auto_pull_image);
    assert!(cfg.api_base_url.is_none());
}

// ── AudioSandboxManager consent gate ─────────────────────────────────────────

#[tokio::test]
async fn audio_manager_requires_consent_to_start() {
    let cfg = AudioSandboxConfig::default();
    let manager = AudioSandboxManager::new(cfg);

    // consent=false must return ConsentRequired.
    let err = manager.start_session(false).await.unwrap_err();
    assert!(
        matches!(err, AudioError::ConsentRequired),
        "expected ConsentRequired, got: {err:?}"
    );
}

#[tokio::test]
async fn audio_manager_not_running_without_session() {
    let cfg = AudioSandboxConfig::default();
    let manager = AudioSandboxManager::new(cfg);

    // Status should show not running before any session.
    let status = manager.status().await;
    assert_eq!(status["running"], false);
    assert_eq!(status["consent_granted"], false);
}

#[tokio::test]
async fn audio_manager_record_requires_session() {
    let cfg = AudioSandboxConfig::default();
    let manager = AudioSandboxManager::new(cfg);

    // record_audio without a session must return ConsentRequired or NotRunning.
    let err = manager.record_audio(5).await.unwrap_err();
    assert!(
        matches!(err, AudioError::ConsentRequired | AudioError::NotRunning),
        "expected ConsentRequired or NotRunning, got: {err:?}"
    );
}

#[tokio::test]
async fn audio_manager_transcribe_requires_session() {
    let cfg = AudioSandboxConfig::default();
    let manager = AudioSandboxManager::new(cfg);

    let err = manager.transcribe_audio("dGVzdA==").await.unwrap_err();
    assert!(
        matches!(err, AudioError::ConsentRequired | AudioError::NotRunning),
        "expected ConsentRequired or NotRunning, got: {err:?}"
    );
}

#[tokio::test]
async fn audio_manager_speak_requires_session() {
    let cfg = AudioSandboxConfig::default();
    let manager = AudioSandboxManager::new(cfg);

    let err = manager.speak("Hello, world!", None).await.unwrap_err();
    assert!(
        matches!(err, AudioError::ConsentRequired | AudioError::NotRunning),
        "expected ConsentRequired or NotRunning, got: {err:?}"
    );
}

// ── AudioSandboxManager input validation ──────────────────────────────────────

#[tokio::test]
async fn audio_manager_record_rejects_zero_duration() {
    let cfg = AudioSandboxConfig::default();
    let manager = AudioSandboxManager::new(cfg);

    let err = manager.record_audio(0).await.unwrap_err();
    assert!(
        matches!(err, AudioError::InvalidInput { .. }),
        "expected InvalidInput for duration=0, got: {err:?}"
    );
}

#[tokio::test]
async fn audio_manager_record_rejects_excessive_duration() {
    let cfg = AudioSandboxConfig::default();
    let manager = AudioSandboxManager::new(cfg);

    // Default max is 120 seconds.
    let err = manager.record_audio(121).await.unwrap_err();
    assert!(
        matches!(err, AudioError::InvalidInput { .. }),
        "expected InvalidInput for duration=121, got: {err:?}"
    );
}

#[tokio::test]
async fn audio_manager_speak_rejects_empty_text() {
    let cfg = AudioSandboxConfig::default();
    let manager = AudioSandboxManager::new(cfg);

    let err = manager.speak("", None).await.unwrap_err();
    assert!(
        matches!(err, AudioError::InvalidInput { .. }),
        "expected InvalidInput for empty text, got: {err:?}"
    );
}

#[tokio::test]
async fn audio_manager_speak_rejects_text_exceeding_max_chars() {
    let cfg = AudioSandboxConfig::default();
    let manager = AudioSandboxManager::new(cfg);

    // Default max is 4096 characters.
    let long_text = "a".repeat(4097);
    let err = manager.speak(&long_text, None).await.unwrap_err();
    assert!(
        matches!(err, AudioError::InvalidInput { .. }),
        "expected InvalidInput for text > 4096 chars, got: {err:?}"
    );
}

// ── AudioSandboxManager stop is idempotent ────────────────────────────────────

#[tokio::test]
async fn audio_manager_stop_is_idempotent_when_not_running() {
    let cfg = AudioSandboxConfig::default();
    let manager = AudioSandboxManager::new(cfg);

    // Stopping when not running should succeed (idempotent).
    manager.stop_session().await.expect("stop_session should be idempotent");
}

// ── AudioError display ────────────────────────────────────────────────────────

#[test]
fn audio_error_consent_required_display() {
    let err = AudioError::ConsentRequired;
    let msg = err.to_string();
    assert!(
        msg.contains("consent"),
        "ConsentRequired message should mention consent, got: {msg}"
    );
}

#[test]
fn audio_error_not_running_display() {
    let err = AudioError::NotRunning;
    let msg = err.to_string();
    assert!(
        msg.contains("audio_session_start"),
        "NotRunning message should mention audio_session_start, got: {msg}"
    );
}

#[test]
fn audio_error_invalid_input_display() {
    let err = AudioError::InvalidInput {
        reason: "test reason".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("test reason"), "got: {msg}");
}

// ── build_audio_tools ─────────────────────────────────────────────────────────

#[test]
fn build_audio_tools_returns_seven_tools() {
    use std::sync::Arc;
    use ironclaw::tools::builtin::build_audio_tools;

    let cfg = AudioSandboxConfig::default();
    let manager = Arc::new(AudioSandboxManager::new(cfg));
    let tools = build_audio_tools(manager);

    assert_eq!(tools.len(), 7, "expected 7 audio tools, got {}", tools.len());
}

#[test]
fn build_audio_tools_has_expected_names() {
    use std::sync::Arc;
    use ironclaw::tools::builtin::build_audio_tools;
    use ironclaw::tools::tool::Tool;

    let cfg = AudioSandboxConfig::default();
    let manager = Arc::new(AudioSandboxManager::new(cfg));
    let tools = build_audio_tools(manager);

    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();

    let expected = [
        "audio_session_start",
        "audio_session_stop",
        "audio_record",
        "audio_transcribe",
        "audio_listen",
        "audio_speak",
        "audio_status",
    ];

    for name in &expected {
        assert!(
            names.contains(name),
            "expected tool '{name}' in audio tools, got: {names:?}"
        );
    }
}

#[test]
fn audio_session_start_requires_always_approval() {
    use std::sync::Arc;
    use ironclaw::tools::builtin::AudioSessionStartTool;
    use ironclaw::tools::tool::{ApprovalRequirement, Tool};

    let cfg = AudioSandboxConfig::default();
    let manager = Arc::new(AudioSandboxManager::new(cfg));
    let tool = AudioSessionStartTool::new(manager);

    assert_eq!(
        tool.requires_approval(&serde_json::json!({})),
        ApprovalRequirement::Always,
        "audio_session_start must always require approval (consent gate)"
    );
}

#[test]
fn audio_status_requires_no_approval() {
    use std::sync::Arc;
    use ironclaw::tools::builtin::AudioStatusTool;
    use ironclaw::tools::tool::{ApprovalRequirement, Tool};

    let cfg = AudioSandboxConfig::default();
    let manager = Arc::new(AudioSandboxManager::new(cfg));
    let tool = AudioStatusTool::new(manager);

    assert_eq!(
        tool.requires_approval(&serde_json::json!({})),
        ApprovalRequirement::Never,
        "audio_status should never require approval"
    );
}

#[test]
fn audio_transcribe_requires_no_approval() {
    use std::sync::Arc;
    use ironclaw::tools::builtin::AudioTranscribeTool;
    use ironclaw::tools::tool::{ApprovalRequirement, Tool};

    let cfg = AudioSandboxConfig::default();
    let manager = Arc::new(AudioSandboxManager::new(cfg));
    let tool = AudioTranscribeTool::new(manager);

    assert_eq!(
        tool.requires_approval(&serde_json::json!({})),
        ApprovalRequirement::Never,
        "audio_transcribe should never require approval (read-only)"
    );
}
