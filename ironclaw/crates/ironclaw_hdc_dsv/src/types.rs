//! Types for the HDC DSV adapter.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Configuration for the HDC DSV adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdcConfig {
    /// Base URL of the local HDC DSV server (e.g. `http://localhost:8765/v1`).
    pub server_url: String,
    /// Minimum quality score [0.0, 1.0] to allow a write to proceed.
    pub quality_threshold: f32,
    /// Whether to block writes below the threshold (true) or just log (false).
    pub block_on_low_score: bool,
    /// Whether to send training updates after each committed write.
    pub online_learning_enabled: bool,
    /// Minimum training examples before the gate becomes active (bootstrap mode).
    pub bootstrap_min: u32,
    /// Current training example count (loaded from model state at startup).
    pub training_example_count: u32,
    /// Request timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for HdcConfig {
    fn default() -> Self {
        Self {
            server_url: "http://localhost:8765/v1".to_string(),
            quality_threshold: 0.4,
            block_on_low_score: false,
            online_learning_enabled: false,
            bootstrap_min: 50,
            training_example_count: 0,
            timeout_secs: 5,
        }
    }
}

impl HdcConfig {
    /// Returns true if the quality gate is active (past bootstrap threshold).
    pub fn gate_active(&self) -> bool {
        self.training_example_count >= self.bootstrap_min
    }
}

/// Verdict from the HDC DSV quality gate.
#[derive(Debug, Clone, PartialEq)]
pub enum HdcVerdict {
    /// Gate is in bootstrap mode — not enough training examples yet.
    /// Write proceeds unconditionally.
    Bootstrap,
    /// Write scored above the threshold — proceed.
    Pass { score: f32 },
    /// Write scored below the threshold — flagged but not blocked.
    Flagged { score: f32, threshold: f32 },
    /// Write scored below the threshold and blocking is enabled — reject.
    Blocked { score: f32, threshold: f32 },
    /// HDC server unreachable and fail-closed mode is enabled — reject.
    FailClosed { reason: String },
    /// HDC server unreachable but fail-open mode — proceed with warning.
    FailOpen { reason: String },
}

impl HdcVerdict {
    /// Returns true if the write should be blocked.
    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. } | Self::FailClosed { .. })
    }

    /// Returns the quality score if available.
    pub fn score(&self) -> Option<f32> {
        match self {
            Self::Pass { score } | Self::Flagged { score, .. } | Self::Blocked { score, .. } => {
                Some(*score)
            }
            _ => None,
        }
    }

    /// Returns a human-readable description.
    pub fn description(&self) -> String {
        match self {
            Self::Bootstrap => "bootstrap mode (gate inactive)".to_string(),
            Self::Pass { score } => format!("PASS (score={:.3})", score),
            Self::Flagged { score, threshold } => {
                format!("FLAGGED (score={:.3} < threshold={:.3})", score, threshold)
            }
            Self::Blocked { score, threshold } => {
                format!("BLOCKED (score={:.3} < threshold={:.3})", score, threshold)
            }
            Self::FailClosed { reason } => format!("FAIL_CLOSED: {}", reason),
            Self::FailOpen { reason } => format!("FAIL_OPEN: {}", reason),
        }
    }
}

/// The outcome of a write (used as the training label).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WriteOutcome {
    /// Write was committed successfully and passed all checks.
    GoodWrite,
    /// Write was blocked by safety layer, rolled back, or flagged by human review.
    BadWrite,
}

impl std::fmt::Display for WriteOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GoodWrite => write!(f, "GOOD_WRITE"),
            Self::BadWrite => write!(f, "BAD_WRITE"),
        }
    }
}

/// A write payload passed to the HDC DSV adapter for scoring and training.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WritePayload {
    /// The tool that produced this write.
    pub tool: String,
    /// The target (skill name or memory key).
    pub target: String,
    /// The content being written.
    pub content: String,
    /// The job type context.
    pub job_type: String,
    /// Size delta in bytes (positive = adding content).
    pub size_delta: i64,
}

/// Errors from the HDC DSV adapter.
#[derive(Debug, Error)]
pub enum HdcError {
    #[error("HDC server unreachable: {0}")]
    ServerUnreachable(String),

    #[error("HDC server returned error: {status} — {body}")]
    ServerError { status: u16, body: String },

    #[error("Failed to parse HDC response: {0}")]
    ParseError(String),

    #[error("Request timeout after {secs}s")]
    Timeout { secs: u64 },
}
