/// Job type enum — replaces the Python JOB_TYPE_* string constants.
/// Using a typed enum prevents arbitrary string injection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobType {
    MemoryReview,
    SkillReview,
    CuratorRun,
    SweTask,
}

impl JobType {
    /// Parse from the Python string constants for backward compatibility.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "MEMORY_REVIEW" => Some(Self::MemoryReview),
            "SKILL_REVIEW" => Some(Self::SkillReview),
            "CURATOR_RUN" => Some(Self::CuratorRun),
            "SWE_TASK" => Some(Self::SweTask),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MemoryReview => "MEMORY_REVIEW",
            Self::SkillReview => "SKILL_REVIEW",
            Self::CuratorRun => "CURATOR_RUN",
            Self::SweTask => "SWE_TASK",
        }
    }
}

/// LLM client selection mode.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmClientMode {
    Auxiliary,
    Main,
    Local,
}

impl LlmClientMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "main" => Self::Main,
            "local" => Self::Local,
            _ => Self::Auxiliary,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auxiliary => "auxiliary",
            Self::Main => "main",
            Self::Local => "local",
        }
    }
}

/// Resolved LLM client triple (provider, model, optional base_url).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResolvedLlm {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
}

/// AES-256-GCM encrypted snapshot.
/// The key is ephemeral and transmitted out-of-band (or stored in the orchestrator's KMS).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EncryptedSnapshot {
    /// Base64-encoded AES-256-GCM ciphertext (includes 16-byte auth tag).
    pub ciphertext: String,
    /// Base64-encoded 96-bit (12-byte) nonce.
    pub nonce: String,
    /// First 16 hex chars of SHA-256(key) — used for key lookup in the orchestrator KMS.
    pub key_id: String,
}

/// Result of a self-improvement dispatch attempt.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DispatchResult {
    /// The orchestrator job ID, or None if the job was skipped.
    pub job_id: Option<String>,
    /// True when IronClaw is not available and the caller should fall back.
    pub skipped: bool,
    /// Human-readable error message when the dispatch failed.
    pub error: Option<String>,
}

impl DispatchResult {
    pub fn submitted(job_id: String) -> Self {
        Self { job_id: Some(job_id), skipped: false, error: None }
    }

    pub fn skipped() -> Self {
        Self { job_id: None, skipped: true, error: None }
    }

    pub fn failed(error: String) -> Self {
        Self { job_id: None, skipped: false, error: Some(error) }
    }
}

/// Minimal agent info extracted at the Python/Rust boundary.
/// Only the fields we actually need — no dynamic getattr inside Rust.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    /// Last N messages from the conversation history.
    pub recent_messages: Vec<Message>,
}

/// A single conversation message (role + content only — no tool call details).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}
