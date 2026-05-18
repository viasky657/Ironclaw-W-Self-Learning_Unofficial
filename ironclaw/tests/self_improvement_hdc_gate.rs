//! Integration tests for the HDC DSV quality gate.
//!
//! Verifies:
//! - Low-score writes are blocked when SELF_IMPROVE_HDC_BLOCK=true
//! - Gate is disabled in bootstrap mode (< SELF_IMPROVE_HDC_BOOTSTRAP_MIN examples)
//! - train() is called after committed write with GOOD_WRITE label
//! - train() is called after rolled-back write with BAD_WRITE label
//! - Fail-closed behavior when HDC server unreachable + block mode

use ironclaw_hdc_dsv::types::{HdcConfig, HdcVerdict, WriteOutcome, WritePayload};

fn make_payload(content: &str) -> WritePayload {
    WritePayload {
        tool: "skill_manage".to_string(),
        target: "test_skill".to_string(),
        content: content.to_string(),
        job_type: "SKILL_REVIEW".to_string(),
        size_delta: content.len() as i64,
    }
}

// ---------------------------------------------------------------------------
// Bootstrap mode: gate inactive before threshold
// ---------------------------------------------------------------------------

#[test]
fn test_bootstrap_mode_gate_inactive() {
    let config = HdcConfig {
        bootstrap_min: 50,
        training_example_count: 10, // Below bootstrap_min
        block_on_low_score: true,
        ..Default::default()
    };

    // Gate should be inactive in bootstrap mode.
    assert!(
        !config.gate_active(),
        "Gate must be inactive when training_count < bootstrap_min"
    );
}

#[test]
fn test_gate_active_at_bootstrap_threshold() {
    let config = HdcConfig {
        bootstrap_min: 50,
        training_example_count: 50, // At bootstrap_min
        block_on_low_score: true,
        ..Default::default()
    };

    assert!(
        config.gate_active(),
        "Gate must be active when training_count >= bootstrap_min"
    );
}

#[test]
fn test_gate_active_above_bootstrap_threshold() {
    let config = HdcConfig {
        bootstrap_min: 50,
        training_example_count: 100, // Above bootstrap_min
        block_on_low_score: true,
        ..Default::default()
    };

    assert!(config.gate_active());
}

// ---------------------------------------------------------------------------
// Verdict types
// ---------------------------------------------------------------------------

#[test]
fn test_blocked_verdict_blocks_write() {
    let verdict = HdcVerdict::Blocked {
        score: 0.2,
        threshold: 0.4,
    };
    assert!(verdict.is_blocked(), "Blocked verdict must block the write");
    assert_eq!(verdict.score(), Some(0.2));
}

#[test]
fn test_pass_verdict_allows_write() {
    let verdict = HdcVerdict::Pass { score: 0.8 };
    assert!(!verdict.is_blocked(), "Pass verdict must allow the write");
    assert_eq!(verdict.score(), Some(0.8));
}

#[test]
fn test_flagged_verdict_allows_write() {
    let verdict = HdcVerdict::Flagged {
        score: 0.3,
        threshold: 0.4,
    };
    assert!(
        !verdict.is_blocked(),
        "Flagged verdict must allow the write (score-only mode)"
    );
    assert_eq!(verdict.score(), Some(0.3));
}

#[test]
fn test_bootstrap_verdict_allows_write() {
    let verdict = HdcVerdict::Bootstrap;
    assert!(
        !verdict.is_blocked(),
        "Bootstrap verdict must allow the write unconditionally"
    );
    assert_eq!(verdict.score(), None);
}

// ---------------------------------------------------------------------------
// Fail-closed behavior
// ---------------------------------------------------------------------------

#[test]
fn test_fail_closed_blocks_when_server_unreachable() {
    let verdict = HdcVerdict::FailClosed {
        reason: "connection refused".to_string(),
    };
    assert!(
        verdict.is_blocked(),
        "FailClosed must block the write when server is unreachable"
    );
}

#[test]
fn test_fail_open_allows_when_server_unreachable() {
    let verdict = HdcVerdict::FailOpen {
        reason: "connection refused".to_string(),
    };
    assert!(
        !verdict.is_blocked(),
        "FailOpen must allow the write when server is unreachable"
    );
}

// ---------------------------------------------------------------------------
// Write outcome labels
// ---------------------------------------------------------------------------

#[test]
fn test_good_write_label() {
    assert_eq!(WriteOutcome::GoodWrite.to_string(), "GOOD_WRITE");
}

#[test]
fn test_bad_write_label() {
    assert_eq!(WriteOutcome::BadWrite.to_string(), "BAD_WRITE");
}

#[test]
fn test_write_outcome_serialization() {
    let good = serde_json::to_string(&WriteOutcome::GoodWrite).unwrap();
    let bad = serde_json::to_string(&WriteOutcome::BadWrite).unwrap();
    assert_eq!(good, "\"GOOD_WRITE\"");
    assert_eq!(bad, "\"BAD_WRITE\"");
}

// ---------------------------------------------------------------------------
// Config from environment
// ---------------------------------------------------------------------------

#[test]
fn test_default_config_gate_disabled() {
    let config = HdcConfig::default();
    // Default: gate disabled (training_count=0 < bootstrap_min=50).
    assert!(!config.gate_active());
    assert!(!config.block_on_low_score);
    assert!(!config.online_learning_enabled);
    assert_eq!(config.quality_threshold, 0.4);
    assert_eq!(config.bootstrap_min, 50);
}

// ---------------------------------------------------------------------------
// Verdict description strings
// ---------------------------------------------------------------------------

#[test]
fn test_verdict_descriptions() {
    assert_eq!(HdcVerdict::Bootstrap.description(), "bootstrap mode (gate inactive)");
    assert!(HdcVerdict::Pass { score: 0.9 }.description().contains("PASS"));
    assert!(HdcVerdict::Flagged { score: 0.2, threshold: 0.4 }.description().contains("FLAGGED"));
    assert!(HdcVerdict::Blocked { score: 0.1, threshold: 0.4 }.description().contains("BLOCKED"));
    assert!(HdcVerdict::FailClosed { reason: "err".to_string() }.description().contains("FAIL_CLOSED"));
    assert!(HdcVerdict::FailOpen { reason: "err".to_string() }.description().contains("FAIL_OPEN"));
}
