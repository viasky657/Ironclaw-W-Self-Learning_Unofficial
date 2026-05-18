//! Self-improvement job type for the IronClaw orchestrator.
//!
//! Defines the `SelfImprovementJob` struct and related types that allow
//! Hermes Agent's background review / curator loop to run inside a secure,
//! auditable sandbox container rather than in the main agent process.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Which LLM client the self-improvement review fork should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmClientMode {
    /// Use the auxiliary provider (cheap fast model, e.g. Gemini Flash via OpenRouter).
    /// This is the default — avoids surprise main-model token spend.
    #[default]
    Auxiliary,
    /// Use the same provider/model as the parent agent turn.
    /// Higher quality, higher cost. Opt-in only.
    Main,
    /// Use a local OpenAI-compatible server (e.g. HDC DSV server at localhost:8765).
    /// Zero cloud API calls. Requires the local server to be running.
    Local,
}

impl std::fmt::Display for LlmClientMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auxiliary => write!(f, "auxiliary"),
            Self::Main => write!(f, "main"),
            Self::Local => write!(f, "local"),
        }
    }
}

impl std::str::FromStr for LlmClientMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "auxiliary" | "aux" => Ok(Self::Auxiliary),
            "main" => Ok(Self::Main),
            "local" => Ok(Self::Local),
            _ => Err(format!(
                "invalid LLM client mode '{}', expected 'auxiliary', 'main', or 'local'",
                s
            )),
        }
    }
}

/// The type of self-improvement work to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SelfImprovementJobType {
    /// Post-turn memory review: extract and save relevant memories from the conversation.
    MemoryReview,
    /// Post-turn skill review: identify and create/update skills from the conversation.
    SkillReview,
    /// Periodic curator run: maintain the skill collection (archive stale, consolidate duplicates).
    CuratorRun,
    /// SWE task: run a software engineering task in the sandbox.
    SweTask,
}

impl std::fmt::Display for SelfImprovementJobType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MemoryReview => write!(f, "MEMORY_REVIEW"),
            Self::SkillReview => write!(f, "SKILL_REVIEW"),
            Self::CuratorRun => write!(f, "CURATOR_RUN"),
            Self::SweTask => write!(f, "SWE_TASK"),
        }
    }
}

/// An encrypted conversation snapshot passed to the sandbox container.
///
/// The snapshot is AES-256-GCM encrypted at the host boundary.
/// The sandbox daemon decrypts it using the per-job key injected via env.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedBlob {
    /// AES-256-GCM ciphertext (base64-encoded).
    pub ciphertext: String,
    /// Nonce (base64-encoded, 12 bytes for GCM).
    pub nonce: String,
    /// Key ID (references the per-job key in the orchestrator's secrets store).
    pub key_id: String,
}

/// A scoped credential grant for a self-improvement job.
///
/// Unlike the general `CredentialGrant`, this is intentionally narrow:
/// only `skill_manage` and `memory` tool credentials are allowed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedGrant {
    /// The tool this grant applies to.
    pub tool: AllowedSelfImproveTool,
    /// The secret name in the orchestrator's secrets store.
    pub secret_name: String,
    /// The env var name to inject into the container.
    pub env_var: String,
}

/// Tools that are allowed inside a self-improvement sandbox.
///
/// This is the complete allowlist — any other tool call is rejected by the
/// WASM bridge before it reaches the container's tool executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowedSelfImproveTool {
    /// Skill management: create/update agent-created skills only.
    SkillManage,
    /// Memory: save/update only (no delete, no list_all, no export).
    Memory,
}

impl std::fmt::Display for AllowedSelfImproveTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SkillManage => write!(f, "skill_manage"),
            Self::Memory => write!(f, "memory"),
        }
    }
}

/// Resolved LLM provider/model pair for the self-improvement review fork.
///
/// Resolved by the dispatcher at job submission time so the container never
/// needs to resolve credentials itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedLlmConfig {
    /// LLM provider identifier (e.g. "openrouter", "anthropic", "local").
    pub provider: String,
    /// Model name (e.g. "google/gemini-flash-1.5", "claude-sonnet-4-20250514").
    pub model: String,
    /// Base URL for the LLM proxy (orchestrator injects this into the container).
    pub base_url: Option<String>,
}

/// A self-improvement job submitted to the IronClaw orchestrator.
///
/// This is the canonical job descriptor stored in the orchestrator's job registry
/// and passed to the sandbox container at startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfImprovementJob {
    /// Unique job identifier.
    pub job_id: Uuid,
    /// The type of self-improvement work.
    pub job_type: SelfImprovementJobType,
    /// Encrypted conversation snapshot (decrypted inside the container by the daemon).
    pub conversation_snapshot: EncryptedBlob,
    /// Which tools the sandboxed agent is allowed to call.
    pub allowed_tools: Vec<AllowedSelfImproveTool>,
    /// Hard cap on agent turns (default: 10).
    pub max_turns: u32,
    /// Hard wall-clock timeout in seconds (default: 120).
    pub max_wall_seconds: u64,
    /// Maximum skill writes per job (default: 10).
    pub max_skill_writes: u32,
    /// Maximum memory writes per job (default: 5).
    pub max_memory_writes: u32,
    /// Scoped credential grants (only what the job needs).
    pub credential_grants: Vec<ScopedGrant>,
    /// Which LLM client mode to use for the review fork.
    pub llm_client_mode: LlmClientMode,
    /// Resolved LLM provider/model (set by dispatcher at submission time).
    pub resolved_llm: Option<ResolvedLlmConfig>,
    /// Whether to auto-rollback on safety violation.
    pub rollback_on_violation: bool,
    /// User ID that owns this job (for audit log scoping).
    pub user_id: String,
}

impl SelfImprovementJob {
    /// Create a new self-improvement job with safe defaults.
    pub fn new(
        job_type: SelfImprovementJobType,
        conversation_snapshot: EncryptedBlob,
        user_id: String,
    ) -> Self {
        Self {
            job_id: Uuid::new_v4(),
            job_type,
            conversation_snapshot,
            allowed_tools: vec![AllowedSelfImproveTool::SkillManage, AllowedSelfImproveTool::Memory],
            max_turns: 10,
            max_wall_seconds: 120,
            max_skill_writes: 10,
            max_memory_writes: 5,
            credential_grants: vec![],
            llm_client_mode: LlmClientMode::Auxiliary,
            resolved_llm: None,
            rollback_on_violation: true,
            user_id,
        }
    }

    /// Returns true if the given tool name is in the allowed list.
    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        self.allowed_tools.iter().any(|t| t.to_string() == tool_name)
    }
}

/// Request body for `POST /jobs/self-improve`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfImproveJobRequest {
    /// The type of self-improvement work.
    pub job_type: SelfImprovementJobType,
    /// Encrypted conversation snapshot.
    pub snapshot_encrypted: EncryptedBlob,
    /// Which LLM client mode to use.
    #[serde(default)]
    pub llm_client_mode: LlmClientMode,
    /// Resolved LLM provider/model (set by dispatcher).
    pub resolved_llm: Option<ResolvedLlmConfig>,
    /// Optional override for max turns.
    pub max_turns: Option<u32>,
    /// Optional override for max wall seconds.
    pub max_wall_seconds: Option<u64>,
    /// Optional override for max skill writes.
    pub max_skill_writes: Option<u32>,
    /// Optional override for max memory writes.
    pub max_memory_writes: Option<u32>,
}

/// Response body for `POST /jobs/self-improve`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfImproveJobResponse {
    /// The assigned job ID.
    pub job_id: Uuid,
    /// The per-job bearer token (scoped to skill_manage + memory only).
    pub token: String,
    /// Status message.
    pub status: String,
}

/// Request body for `POST /orchestrator/memory-write`.
///
/// The sandbox container calls this endpoint to proxy memory writes to the
/// host-side MemoryManager. The container never directly touches the memory
/// backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryWriteRequest {
    /// The job that is requesting the write.
    pub job_id: Uuid,
    /// The memory action (save or update only).
    pub action: MemoryWriteAction,
    /// The memory key/identifier.
    pub key: String,
    /// The memory content to write.
    pub content: String,
    /// Optional metadata tags.
    pub tags: Vec<String>,
}

/// Allowed memory write actions (subset of the full memory tool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWriteAction {
    /// Save a new memory entry.
    Save,
    /// Update an existing memory entry.
    Update,
}

/// Response body for `POST /orchestrator/memory-write`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryWriteResponse {
    /// Whether the write succeeded.
    pub success: bool,
    /// Optional message (error details or confirmation).
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_client_mode_roundtrip() {
        for mode in [LlmClientMode::Auxiliary, LlmClientMode::Main, LlmClientMode::Local] {
            let s = mode.to_string();
            let parsed: LlmClientMode = s.parse().unwrap();
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn test_job_tool_allowlist() {
        let job = SelfImprovementJob::new(
            SelfImprovementJobType::SkillReview,
            EncryptedBlob {
                ciphertext: "abc".to_string(),
                nonce: "def".to_string(),
                key_id: "key1".to_string(),
            },
            "user1".to_string(),
        );

        assert!(job.is_tool_allowed("skill_manage"));
        assert!(job.is_tool_allowed("memory"));
        assert!(!job.is_tool_allowed("terminal"));
        assert!(!job.is_tool_allowed("http"));
        assert!(!job.is_tool_allowed("file"));
    }

    #[test]
    fn test_job_defaults() {
        let job = SelfImprovementJob::new(
            SelfImprovementJobType::MemoryReview,
            EncryptedBlob {
                ciphertext: "x".to_string(),
                nonce: "y".to_string(),
                key_id: "z".to_string(),
            },
            "user2".to_string(),
        );

        assert_eq!(job.max_turns, 10);
        assert_eq!(job.max_wall_seconds, 120);
        assert_eq!(job.max_skill_writes, 10);
        assert_eq!(job.max_memory_writes, 5);
        assert_eq!(job.llm_client_mode, LlmClientMode::Auxiliary);
        assert!(job.rollback_on_violation);
    }

    #[test]
    fn test_job_type_display() {
        assert_eq!(SelfImprovementJobType::MemoryReview.to_string(), "MEMORY_REVIEW");
        assert_eq!(SelfImprovementJobType::SkillReview.to_string(), "SKILL_REVIEW");
        assert_eq!(SelfImprovementJobType::CuratorRun.to_string(), "CURATOR_RUN");
        assert_eq!(SelfImprovementJobType::SweTask.to_string(), "SWE_TASK");
    }
}
