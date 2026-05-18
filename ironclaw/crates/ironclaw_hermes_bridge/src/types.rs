//! Shared types for the Hermes bridge tool interface.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Configuration for the WASM tool bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    /// Job ID (used for rate limiting and audit).
    pub job_id: String,
    /// Orchestrator base URL for proxying memory writes.
    /// e.g. `http://172.17.0.1:50051`
    pub orchestrator_url: String,
    /// Per-job bearer token for authenticating with the orchestrator.
    pub job_token: String,
    /// Path to the skills directory (writable surface).
    pub skills_path: String,
    /// Maximum skill file size in bytes (default: 64 KB).
    pub max_skill_bytes: usize,
    /// Maximum total bytes written per job (default: 256 KB).
    pub max_total_bytes: usize,
    /// Maximum skill writes per job (default: 10).
    pub max_skill_writes: u32,
    /// Maximum memory writes per job (default: 5).
    pub max_memory_writes: u32,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            job_id: String::new(),
            orchestrator_url: "http://172.17.0.1:50051".to_string(),
            job_token: String::new(),
            skills_path: "/hermes-skills".to_string(),
            max_skill_bytes: 64 * 1024,
            max_total_bytes: 256 * 1024,
            max_skill_writes: 10,
            max_memory_writes: 5,
        }
    }
}

/// A tool call from the sandboxed agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Tool name (must be `skill_manage` or `memory`).
    pub tool: String,
    /// Tool arguments as a JSON object.
    pub arguments: serde_json::Value,
}

/// Result of a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Whether the tool call succeeded.
    pub success: bool,
    /// Output content (shown to the agent).
    pub content: String,
    /// Whether this write was committed to the audit log.
    pub committed: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            success: true,
            content: content.into(),
            committed: true,
        }
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            success: false,
            content: content.into(),
            committed: false,
        }
    }

    pub fn blocked(reason: impl Into<String>) -> Self {
        Self {
            success: false,
            content: format!("BLOCKED: {}", reason.into()),
            committed: false,
        }
    }
}

/// A write payload passed to the content policy and HDC DSV adapter.
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

/// Errors from the WASM tool bridge.
#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("Tool not allowed: {0}")]
    ToolNotAllowed(String),

    #[error("Operation not allowed: {0}")]
    OperationNotAllowed(String),

    #[error("Ownership check failed: {0}")]
    OwnershipCheckFailed(String),

    #[error("Content policy violation: {0}")]
    ContentPolicyViolation(String),

    #[error("Size limit exceeded: {0}")]
    SizeLimitExceeded(String),

    #[error("Rate limit exceeded: {0}")]
    RateLimitExceeded(String),

    #[error("Orchestrator proxy error: {0}")]
    OrchestratorProxyError(String),

    #[error("IO error: {0}")]
    IoError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),
}
