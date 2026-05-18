use std::sync::{Arc, Mutex};
use serde_json::Value;

use crate::policy::is_sandboxed_tool;
use crate::types::ToolBridgeResult;

/// Internal session state — guarded by `Arc<Mutex<>>`.
pub struct SessionState {
    pub job_id: Option<String>,
    pub job_token: Option<String>,
    pub closed: bool,
}

/// A long-lived worker job on the IronClaw orchestrator that executes
/// sandboxed tool calls on behalf of one Hermes agent session.
///
/// The session is created lazily on the first tool call and reused for the
/// lifetime of the agent session. Thread-safe: `Arc<Mutex<SessionState>>`
/// guards job creation so concurrent tool calls don't race to create duplicate jobs.
pub struct BridgeSession {
    pub session_id: String,
    state: Arc<Mutex<SessionState>>,
}

impl BridgeSession {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            state: Arc::new(Mutex::new(SessionState {
                job_id: None,
                job_token: None,
                closed: false,
            })),
        }
    }

    /// Create a new worker job on the orchestrator.
    async fn create_job(&self) -> bool {
        let orchestrator_url = std::env::var("IRONCLAW_ORCHESTRATOR_URL")
            .unwrap_or_else(|_| "http://localhost:8080".to_string());
        let orchestrator_url = orchestrator_url.trim_end_matches('/');
        let orchestrator_token = std::env::var("IRONCLAW_ORCHESTRATOR_TOKEN")
            .unwrap_or_default();
        let sandbox_policy = std::env::var("IRONCLAW_TOOL_SANDBOX_POLICY")
            .unwrap_or_else(|_| "WorkspaceWrite".to_string());
        let max_wall_seconds: u64 = std::env::var("IRONCLAW_TOOL_SESSION_MAX_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600);

        let url = format!("{}/jobs/tool-session", orchestrator_url);
        let payload = serde_json::json!({
            "session_id": self.session_id,
            "sandbox_policy": sandbox_policy,
            "max_wall_seconds": max_wall_seconds,
        });

        let client = match reqwest::Client::builder().use_rustls_tls().build() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Tool bridge: failed to build HTTP client");
                return false;
            }
        };

        let resp = match client
            .post(&url)
            .bearer_auth(&orchestrator_token)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, url = %url, "Tool bridge: /jobs/tool-session request failed");
                return false;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "Tool bridge: /jobs/tool-session returned error");
            return false;
        }

        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "Tool bridge: failed to read /jobs/tool-session response");
                return false;
            }
        };

        let parsed: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, body = %body, "Tool bridge: failed to parse /jobs/tool-session response");
                return false;
            }
        };

        let job_id = match parsed["job_id"].as_str() {
            Some(id) => id.to_string(),
            None => {
                tracing::warn!(body = %body, "Tool bridge: /jobs/tool-session response missing job_id");
                return false;
            }
        };
        let job_token = match parsed["job_token"].as_str() {
            Some(t) => t.to_string(),
            None => {
                tracing::warn!(body = %body, "Tool bridge: /jobs/tool-session response missing job_token");
                return false;
            }
        };

        let mut state = self.state.lock().unwrap();
        state.job_id = Some(job_id.clone());
        state.job_token = Some(job_token);
        tracing::info!(
            job_id = %job_id,
            session_id = %self.session_id,
            "Tool bridge: created tool session job"
        );
        true
    }

    /// Ensure a worker job exists, creating one if needed. Thread-safe.
    async fn ensure_job(&self) -> bool {
        {
            let state = self.state.lock().unwrap();
            if state.job_id.is_some() {
                return true;
            }
        }
        // Double-checked locking: re-check after acquiring the lock.
        // We use a separate async call here — the Mutex is not held across await.
        self.create_job().await
    }

    /// Close the worker job and release resources.
    pub async fn close(&self) {
        let (job_id, job_token) = {
            let mut state = self.state.lock().unwrap();
            if state.closed || state.job_id.is_none() {
                return;
            }
            state.closed = true;
            (state.job_id.clone().unwrap(), state.job_token.clone().unwrap_or_default())
        };

        let orchestrator_url = std::env::var("IRONCLAW_ORCHESTRATOR_URL")
            .unwrap_or_else(|_| "http://localhost:8080".to_string());
        let orchestrator_url = orchestrator_url.trim_end_matches('/').to_string();
        let url = format!("{}/worker/{}/complete", orchestrator_url, job_id);
        let payload = serde_json::json!({
            "status": "success",
            "result": "tool-session-closed",
        });

        if let Ok(client) = reqwest::Client::builder().use_rustls_tls().build() {
            let _ = client
                .post(&url)
                .bearer_auth(&job_token)
                .json(&payload)
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await;
        }
        tracing::debug!(job_id = %job_id, "Tool bridge: closed tool session job");
    }

    /// Execute a tool inside the sandbox.
    ///
    /// Returns a `ToolBridgeResult`:
    /// - `Fallback` — tool is not sandboxed; caller may execute directly.
    /// - `Blocked` — sandboxed tool could not be executed; caller MUST NOT fall back.
    /// - `Ok(result)` — success.
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        tool_args: Value,
        tool_call_id: &str,
        timeout_secs: u64,
    ) -> ToolBridgeResult {
        // Check if session is closed.
        {
            let state = self.state.lock().unwrap();
            if state.closed {
                return ToolBridgeResult::fail_closed(
                    "[IronClaw sandbox] Tool session was closed — tool execution blocked. \
                     Restart the IronClaw orchestrator and retry."
                    .to_string(),
                );
            }
        }

        if !is_sandboxed_tool(tool_name) {
            return ToolBridgeResult::allow_fallback();
        }

        // Try to establish the session. If this fails, the orchestrator is
        // unreachable — fail-closed: do NOT execute on the host.
        if !self.ensure_job().await {
            let orchestrator_url = std::env::var("IRONCLAW_ORCHESTRATOR_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_string());
            return ToolBridgeResult::fail_closed(format!(
                "[IronClaw sandbox] Cannot execute '{}': the IronClaw orchestrator at {} \
                 is not reachable. Direct host execution is disabled for security. \
                 Start the IronClaw orchestrator and retry, or set \
                 HERMES_PREFER_LOCAL_SELF_IMPROVE=true to opt out of sandboxing.",
                tool_name, orchestrator_url
            ));
        }

        // Session is established. From this point on, all failures are fail-closed.
        let (job_id, job_token) = {
            let state = self.state.lock().unwrap();
            (
                state.job_id.clone().unwrap_or_default(),
                state.job_token.clone().unwrap_or_default(),
            )
        };

        let orchestrator_url = std::env::var("IRONCLAW_ORCHESTRATOR_URL")
            .unwrap_or_else(|_| "http://localhost:8080".to_string());
        let orchestrator_url = orchestrator_url.trim_end_matches('/').to_string();
        let url = format!("{}/worker/{}/tool", orchestrator_url, job_id);
        let payload = serde_json::json!({
            "tool_name": tool_name,
            "parameters": tool_args,
            "tool_call_id": tool_call_id,
        });

        let client = match reqwest::Client::builder().use_rustls_tls().build() {
            Ok(c) => c,
            Err(e) => {
                return ToolBridgeResult::fail_closed(format!(
                    "[IronClaw sandbox] Tool '{}' could not be executed: \
                     failed to build HTTP client: {}. The tool was NOT run on the host.",
                    tool_name, e
                ));
            }
        };

        let resp = match client
            .post(&url)
            .bearer_auth(&job_token)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    tool = %tool_name,
                    job = %job_id,
                    error = %e,
                    "Tool bridge: sandbox call failed — fail-closed"
                );
                return ToolBridgeResult::fail_closed(format!(
                    "[IronClaw sandbox] Tool '{}' could not be executed: \
                     sandbox communication failed: {}. The tool was NOT run on the host.",
                    tool_name, e
                ));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                tool = %tool_name,
                job = %job_id,
                status = %status,
                "Tool bridge: sandbox returned HTTP error — fail-closed"
            );
            return ToolBridgeResult::fail_closed(format!(
                "[IronClaw sandbox] Tool '{}' failed: HTTP {} from sandbox. \
                 The tool was NOT run on the host.",
                tool_name, status
            ));
        }

        let body = resp.text().await.unwrap_or_default();
        let parsed: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => {
                return ToolBridgeResult::fail_closed(format!(
                    "[IronClaw sandbox] Tool '{}' returned unparseable response. \
                     The tool was NOT run on the host.",
                    tool_name
                ));
            }
        };

        if !parsed["success"].as_bool().unwrap_or(false) {
            let error = parsed["error"]
                .as_str()
                .unwrap_or("unknown sandbox error")
                .to_string();
            tracing::warn!(
                tool = %tool_name,
                job = %job_id,
                error = %error,
                "Tool bridge: sandbox returned error — fail-closed"
            );
            return ToolBridgeResult::fail_closed(format!(
                "[IronClaw sandbox] Tool '{}' failed: {}",
                tool_name, error
            ));
        }

        let result = parsed["result"].as_str().unwrap_or("").to_string();
        tracing::debug!(
            tool = %tool_name,
            job = %job_id,
            "Tool bridge: tool completed via sandbox"
        );
        ToolBridgeResult::ok(result)
    }
}
