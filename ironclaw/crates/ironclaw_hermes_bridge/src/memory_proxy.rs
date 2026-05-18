//! `memory` tool proxy for the self-improvement sandbox.
//!
//! The sandbox container never directly touches the memory backend.
//! All memory writes are proxied to the orchestrator host via
//! `POST /orchestrator/memory-write`, which dispatches to the host-side
//! `MemoryManager` that has the correct provider wired up.
//!
//! ## Allowed operations
//! - `save`: Save a new memory entry
//! - `update`: Update an existing memory entry
//!
//! ## Denied operations
//! - `delete`: Never allowed in self-improvement mode
//! - `list_all`: Never allowed (exfiltration risk)
//! - `export`: Never allowed (exfiltration risk)

use serde::{Deserialize, Serialize};

use crate::policy::{ContentPolicy, PolicyVerdict};
use crate::rate_limiter::RateLimiter;
use crate::types::{BridgeConfig, BridgeError, ToolResult, WritePayload};

/// Allowed memory actions in self-improvement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAction {
    /// Save a new memory entry.
    Save,
    /// Update an existing memory entry.
    Update,
}

impl std::fmt::Display for MemoryAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Save => write!(f, "save"),
            Self::Update => write!(f, "update"),
        }
    }
}

/// Arguments for the `memory` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryArgs {
    /// The action to perform.
    pub action: MemoryAction,
    /// The memory key/identifier.
    pub key: String,
    /// The memory content.
    pub content: String,
    /// Optional tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Request body sent to `POST /orchestrator/memory-write`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrchestratorMemoryWriteRequest {
    pub job_id: String,
    pub action: String,
    pub key: String,
    pub content: String,
    pub tags: Vec<String>,
}

/// Response from `POST /orchestrator/memory-write`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrchestratorMemoryWriteResponse {
    pub success: bool,
    pub message: Option<String>,
}

/// The `memory` tool proxy for the self-improvement sandbox.
pub struct MemoryProxyTool {
    config: BridgeConfig,
    policy: ContentPolicy,
    rate_limiter: RateLimiter,
}

impl MemoryProxyTool {
    pub fn new(config: BridgeConfig, rate_limiter: RateLimiter) -> Self {
        // Memory writes use a slightly larger limit (256 KB) since memory
        // entries can be longer than individual skill files.
        let policy = ContentPolicy::new(config.max_total_bytes, true);
        Self {
            config,
            policy,
            rate_limiter,
        }
    }

    /// Execute a `memory` tool call.
    ///
    /// Validates the payload, checks the content policy, then proxies the
    /// write to the orchestrator host via HTTP.
    pub fn execute(&self, args: MemoryArgs) -> Result<ToolResult, BridgeError> {
        // 1. Validate key.
        self.validate_key(&args.key)?;

        // 2. Check rate limit.
        self.rate_limiter.consume_memory_write()?;

        // 3. Build write payload for policy check.
        let payload = WritePayload {
            tool: "memory".to_string(),
            target: args.key.clone(),
            content: args.content.clone(),
            job_type: "MEMORY_REVIEW".to_string(),
            size_delta: args.content.len() as i64,
        };

        // 4. Run content policy.
        let verdict = self.policy.check(&payload)?;
        if verdict.is_blocked() {
            return Ok(ToolResult::blocked(
                verdict.reason().unwrap_or("content policy violation"),
            ));
        }

        // 5. Proxy the write to the orchestrator host.
        let result = self.proxy_to_orchestrator(&args)?;

        let flagged_note = if matches!(verdict, PolicyVerdict::Flagged { .. }) {
            format!(
                " [FLAGGED: {}]",
                verdict.reason().unwrap_or("content policy")
            )
        } else {
            String::new()
        };

        if result.success {
            Ok(ToolResult::ok(format!(
                "Memory {} '{}' proxied to host MemoryManager successfully{}",
                args.action, args.key, flagged_note,
            )))
        } else {
            Ok(ToolResult::err(format!(
                "Memory write failed: {}",
                result.message.unwrap_or_else(|| "unknown error".to_string())
            )))
        }
    }

    /// Validate the memory key.
    fn validate_key(&self, key: &str) -> Result<(), BridgeError> {
        if key.is_empty() {
            return Err(BridgeError::OperationNotAllowed(
                "Memory key cannot be empty".to_string(),
            ));
        }
        if key.len() > 512 {
            return Err(BridgeError::OperationNotAllowed(
                "Memory key too long (max 512 chars)".to_string(),
            ));
        }
        Ok(())
    }

    /// Proxy the memory write to the orchestrator host.
    ///
    /// In WASM mode (wasm32-wasip1), this uses WASI sockets.
    /// In native mode (Docker container), this uses reqwest.
    fn proxy_to_orchestrator(
        &self,
        args: &MemoryArgs,
    ) -> Result<OrchestratorMemoryWriteResponse, BridgeError> {
        let url = format!("{}/orchestrator/memory-write", self.config.orchestrator_url);

        let req_body = OrchestratorMemoryWriteRequest {
            job_id: self.config.job_id.clone(),
            action: args.action.to_string(),
            key: args.key.clone(),
            content: args.content.clone(),
            tags: args.tags.clone(),
        };

        let body_json = serde_json::to_string(&req_body).map_err(|e| {
            BridgeError::SerializationError(format!("Failed to serialize memory write: {}", e))
        })?;

        // Use a simple blocking HTTP call via the standard library's TCP stack.
        // This works in both native and WASM (WASI) contexts.
        self.http_post_blocking(&url, &body_json)
    }

    /// Blocking HTTP POST using raw TCP (works in WASM/WASI and native).
    fn http_post_blocking(
        &self,
        url: &str,
        body: &str,
    ) -> Result<OrchestratorMemoryWriteResponse, BridgeError> {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        // Parse the URL to extract host, port, and path.
        let url_stripped = url
            .strip_prefix("http://")
            .ok_or_else(|| BridgeError::OrchestratorProxyError("Only http:// URLs supported".to_string()))?;

        let (host_port, path) = if let Some(slash) = url_stripped.find('/') {
            (&url_stripped[..slash], &url_stripped[slash..])
        } else {
            (url_stripped, "/")
        };

        let mut stream = TcpStream::connect(host_port).map_err(|e| {
            BridgeError::OrchestratorProxyError(format!("Failed to connect to orchestrator: {}", e))
        })?;

        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n{}",
            path,
            host_port,
            body.len(),
            self.config.job_token,
            body
        );

        stream.write_all(request.as_bytes()).map_err(|e| {
            BridgeError::OrchestratorProxyError(format!("Failed to send request: {}", e))
        })?;

        let mut response = String::new();
        stream.read_to_string(&mut response).map_err(|e| {
            BridgeError::OrchestratorProxyError(format!("Failed to read response: {}", e))
        })?;

        // Extract the JSON body from the HTTP response.
        let body_start = response.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
        let json_body = &response[body_start..];

        serde_json::from_str(json_body).map_err(|e| {
            BridgeError::OrchestratorProxyError(format!(
                "Failed to parse orchestrator response: {} (body: {})",
                e, json_body
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config() -> BridgeConfig {
        BridgeConfig {
            job_id: "test-job".to_string(),
            orchestrator_url: "http://localhost:50051".to_string(),
            job_token: "test-token".to_string(),
            skills_path: "/tmp/skills".to_string(),
            max_skill_bytes: 64 * 1024,
            max_total_bytes: 256 * 1024,
            max_skill_writes: 10,
            max_memory_writes: 5,
        }
    }

    #[test]
    fn test_empty_key_rejected() {
        let config = make_config();
        let tool = MemoryProxyTool::new(config, RateLimiter::default());
        let result = tool.validate_key("");
        assert!(result.is_err());
    }

    #[test]
    fn test_long_key_rejected() {
        let config = make_config();
        let tool = MemoryProxyTool::new(config, RateLimiter::default());
        let result = tool.validate_key(&"x".repeat(600));
        assert!(result.is_err());
    }

    #[test]
    fn test_rate_limit_enforced() {
        let config = make_config();
        let tool = MemoryProxyTool::new(config, RateLimiter::new(10, 2));

        // Consume both memory write slots via rate limiter directly.
        tool.rate_limiter.consume_memory_write().unwrap();
        tool.rate_limiter.consume_memory_write().unwrap();

        // 3rd should fail.
        assert!(tool.rate_limiter.consume_memory_write().is_err());
    }
}
