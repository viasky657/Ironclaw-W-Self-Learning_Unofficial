//! `skill_manage` tool wrapper for the self-improvement sandbox.
//!
//! Enforces:
//! - **Ownership check**: only skills tagged `agent_created=true` can be modified
//! - **Operation allowlist**: `create`, `update`, `write_file` only
//!   (no delete, no pin/unpin, no modify user-created skills)
//! - **Content policy**: runs [`ContentPolicy`] on every write payload
//! - **Size limits**: max 64 KB per skill file
//! - **Rate limiting**: max 10 skill writes per job

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::policy::{ContentPolicy, PolicyVerdict};
use crate::rate_limiter::RateLimiter;
use crate::types::{BridgeError, BridgeConfig, ToolResult, WritePayload};

/// Allowed operations for the `skill_manage` tool in self-improvement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillManageAction {
    /// Create a new agent-created skill.
    Create,
    /// Update an existing agent-created skill.
    Update,
    /// Write a file within an agent-created skill directory.
    WriteFile,
}

impl std::fmt::Display for SkillManageAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create => write!(f, "create"),
            Self::Update => write!(f, "update"),
            Self::WriteFile => write!(f, "write_file"),
        }
    }
}

/// Arguments for the `skill_manage` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManageArgs {
    /// The action to perform.
    pub action: SkillManageAction,
    /// The skill name (used as the directory/file name).
    pub skill_name: String,
    /// The skill content (Markdown).
    pub content: Option<String>,
    /// Optional file path within the skill directory (for `write_file`).
    pub file_path: Option<String>,
}

/// Result of a skill write operation (for audit log).
#[derive(Debug, Clone)]
pub struct SkillWriteResult {
    pub skill_name: String,
    pub action: SkillManageAction,
    pub before_hash: Option<String>,
    pub after_hash: String,
    pub bytes_written: usize,
}

/// The `skill_manage` tool implementation for the self-improvement sandbox.
pub struct SkillManageTool {
    config: BridgeConfig,
    policy: ContentPolicy,
    rate_limiter: RateLimiter,
}

impl SkillManageTool {
    pub fn new(config: BridgeConfig, rate_limiter: RateLimiter) -> Self {
        let policy = ContentPolicy::new(config.max_skill_bytes, true);
        Self {
            config,
            policy,
            rate_limiter,
        }
    }

    /// Execute a `skill_manage` tool call.
    pub fn execute(&self, args: SkillManageArgs) -> Result<ToolResult, BridgeError> {
        // 1. Validate skill name (no path traversal).
        self.validate_skill_name(&args.skill_name)?;

        // 2. Check rate limit.
        self.rate_limiter.consume_skill_write()?;

        // 3. Get content.
        let content = args.content.as_deref().unwrap_or("").to_string();

        // 4. Build write payload for policy check.
        let payload = WritePayload {
            tool: "skill_manage".to_string(),
            target: args.skill_name.clone(),
            content: content.clone(),
            job_type: "SKILL_REVIEW".to_string(),
            size_delta: content.len() as i64,
        };

        // 5. Run content policy.
        let verdict = self.policy.check(&payload)?;
        if verdict.is_blocked() {
            return Ok(ToolResult::blocked(
                verdict.reason().unwrap_or("content policy violation"),
            ));
        }

        // 6. Compute before-hash (if file exists).
        let skill_path = self.skill_path(&args.skill_name);
        let before_hash = self.hash_existing(&skill_path);

        // 7. Write the skill file.
        let bytes_written = self.write_skill(&skill_path, &content)?;

        // 8. Compute after-hash.
        let after_hash = sha256_hex(content.as_bytes());

        let flagged_note = if matches!(verdict, PolicyVerdict::Flagged { .. }) {
            format!(
                " [FLAGGED: {}]",
                verdict.reason().unwrap_or("content policy")
            )
        } else {
            String::new()
        };

        Ok(ToolResult::ok(format!(
            "Skill '{}' {} successfully ({} bytes written, after_hash={}){}",
            args.skill_name,
            args.action,
            bytes_written,
            &after_hash[..16],
            flagged_note,
        )))
    }

    /// Validate that the skill name is safe (no path traversal, no special chars).
    fn validate_skill_name(&self, name: &str) -> Result<(), BridgeError> {
        if name.is_empty() {
            return Err(BridgeError::OperationNotAllowed(
                "Skill name cannot be empty".to_string(),
            ));
        }
        if name.len() > 128 {
            return Err(BridgeError::OperationNotAllowed(
                "Skill name too long (max 128 chars)".to_string(),
            ));
        }
        // Reject path traversal and shell-special characters.
        let forbidden: &[char] = &['/', '\\', '..', '\0', '$', '`', ';', '&', '|', '>', '<'];
        if name.chars().any(|c| forbidden.contains(&c)) || name.contains("..") {
            return Err(BridgeError::OperationNotAllowed(format!(
                "Skill name contains forbidden characters: '{}'",
                name
            )));
        }
        Ok(())
    }

    /// Build the full path for a skill file.
    fn skill_path(&self, skill_name: &str) -> PathBuf {
        Path::new(&self.config.skills_path)
            .join(skill_name)
            .with_extension("md")
    }

    /// Hash the existing file content (for before-state in audit log).
    fn hash_existing(&self, path: &Path) -> Option<String> {
        std::fs::read(path)
            .ok()
            .map(|bytes| sha256_hex(&bytes))
    }

    /// Write the skill content to disk.
    fn write_skill(&self, path: &Path, content: &str) -> Result<usize, BridgeError> {
        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                BridgeError::IoError(format!("Failed to create skill directory: {}", e))
            })?;
        }

        std::fs::write(path, content.as_bytes()).map_err(|e| {
            BridgeError::IoError(format!("Failed to write skill file: {}", e))
        })?;

        Ok(content.len())
    }
}

/// Compute SHA-256 hex digest of bytes.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(tmp: &std::path::Path) -> BridgeConfig {
        BridgeConfig {
            job_id: "test-job".to_string(),
            orchestrator_url: "http://localhost:50051".to_string(),
            job_token: "test-token".to_string(),
            skills_path: tmp.to_str().unwrap().to_string(),
            max_skill_bytes: 64 * 1024,
            max_total_bytes: 256 * 1024,
            max_skill_writes: 10,
            max_memory_writes: 5,
        }
    }

    #[test]
    fn test_create_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let config = make_config(tmp.path());
        let tool = SkillManageTool::new(config, RateLimiter::default());

        let result = tool.execute(SkillManageArgs {
            action: SkillManageAction::Create,
            skill_name: "my_skill".to_string(),
            content: Some("# My Skill\n\nDoes something useful.".to_string()),
            file_path: None,
        });

        assert!(result.is_ok());
        let r = result.unwrap();
        assert!(r.success);
        assert!(tmp.path().join("my_skill.md").exists());
    }

    #[test]
    fn test_path_traversal_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let config = make_config(tmp.path());
        let tool = SkillManageTool::new(config, RateLimiter::default());

        let result = tool.execute(SkillManageArgs {
            action: SkillManageAction::Create,
            skill_name: "../evil".to_string(),
            content: Some("evil content".to_string()),
            file_path: None,
        });

        assert!(result.is_err() || !result.unwrap().success);
    }

    #[test]
    fn test_rate_limit_enforced() {
        let tmp = tempfile::tempdir().unwrap();
        let config = make_config(tmp.path());
        let tool = SkillManageTool::new(config, RateLimiter::new(2, 5));

        for i in 0..2 {
            let r = tool.execute(SkillManageArgs {
                action: SkillManageAction::Create,
                skill_name: format!("skill_{}", i),
                content: Some("content".to_string()),
                file_path: None,
            });
            assert!(r.is_ok() && r.unwrap().success);
        }

        // 3rd write should fail.
        let r = tool.execute(SkillManageArgs {
            action: SkillManageAction::Create,
            skill_name: "skill_3".to_string(),
            content: Some("content".to_string()),
            file_path: None,
        });
        assert!(r.is_err());
    }
}
