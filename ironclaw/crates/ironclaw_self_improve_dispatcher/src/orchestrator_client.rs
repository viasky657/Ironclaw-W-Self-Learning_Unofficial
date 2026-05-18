use reqwest::Client;
use serde_json::Value;

use crate::config::DispatcherConfig;
use crate::types::{DispatchResult, EncryptedSnapshot, ResolvedLlm};

/// Errors from the orchestrator HTTP client.
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Orchestrator returned HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("Response missing field '{field}': {body}")]
    MissingField { field: &'static str, body: String },
}

/// Typed HTTP client for the IronClaw orchestrator.
///
/// Uses `reqwest` with `rustls-tls-native-roots` — no OpenSSL dependency.
pub struct OrchestratorClient {
    client: Client,
    config: DispatcherConfig,
}

impl OrchestratorClient {
    pub fn new(config: DispatcherConfig) -> Result<Self, OrchestratorError> {
        let client = Client::builder()
            .use_rustls_tls()
            .build()?;
        Ok(Self { client, config })
    }

    /// Probe `GET /health` — returns true when the orchestrator is reachable.
    pub async fn health_check(&self) -> bool {
        let url = self.config.health_url();
        match self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Submit a self-improvement job to `POST /jobs/self-improve`.
    ///
    /// Returns the job_id on success.
    pub async fn submit_self_improve_job(
        &self,
        job_type: &str,
        encrypted: &EncryptedSnapshot,
        llm: &ResolvedLlm,
        llm_mode: &str,
    ) -> Result<String, OrchestratorError> {
        let url = self.config.self_improve_url();
        let payload = serde_json::json!({
            "job_type": job_type,
            "snapshot_encrypted": {
                "ciphertext": encrypted.ciphertext,
                "nonce": encrypted.nonce,
                "key_id": encrypted.key_id,
            },
            "llm_client_mode": llm_mode,
            "resolved_llm": {
                "provider": llm.provider,
                "model": llm.model,
                "base_url": llm.base_url,
            },
            "max_turns": self.config.max_turns,
            "max_wall_seconds": self.config.max_wall_seconds,
            "max_skill_writes": self.config.max_skill_writes,
            "max_memory_writes": self.config.max_memory_writes,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.orchestrator_token)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(OrchestratorError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: Value = serde_json::from_str(&body).map_err(|_| {
            OrchestratorError::MissingField {
                field: "job_id",
                body: body.clone(),
            }
        })?;

        parsed["job_id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or(OrchestratorError::MissingField {
                field: "job_id",
                body,
            })
    }

    /// Submit a tool-session job to `POST /jobs/tool-session`.
    pub async fn submit_tool_session(
        &self,
        session_id: &str,
    ) -> Result<(String, String), OrchestratorError> {
        let url = format!("{}/jobs/tool-session", self.config.orchestrator_url);
        let payload = serde_json::json!({
            "session_id": session_id,
            "sandbox_policy": self.config.sandbox_policy,
            "max_wall_seconds": 3600,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.orchestrator_token)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(OrchestratorError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: Value = serde_json::from_str(&body).map_err(|_| {
            OrchestratorError::MissingField {
                field: "job_id",
                body: body.clone(),
            }
        })?;

        let job_id = parsed["job_id"]
            .as_str()
            .ok_or(OrchestratorError::MissingField {
                field: "job_id",
                body: body.clone(),
            })?
            .to_string();

        let job_token = parsed["job_token"]
            .as_str()
            .ok_or(OrchestratorError::MissingField {
                field: "job_token",
                body,
            })?
            .to_string();

        Ok((job_id, job_token))
    }

    /// Execute a sandboxed tool via `POST /worker/{job_id}/tool`.
    pub async fn execute_sandboxed_tool(
        &self,
        job_id: &str,
        job_token: &str,
        tool_name: &str,
        tool_args: &Value,
        tool_call_id: &str,
        timeout_secs: u64,
    ) -> Result<Value, OrchestratorError> {
        let url = format!(
            "{}/worker/{}/tool",
            self.config.orchestrator_url, job_id
        );
        let payload = serde_json::json!({
            "tool_name": tool_name,
            "parameters": tool_args,
            "tool_call_id": tool_call_id,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(job_token)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(OrchestratorError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }

        serde_json::from_str(&body).map_err(|_| OrchestratorError::MissingField {
            field: "result",
            body,
        })
    }

    /// Mark a job as complete via `POST /worker/{job_id}/complete`.
    pub async fn complete_job(&self, job_id: &str, job_token: &str) -> Result<(), OrchestratorError> {
        let url = format!(
            "{}/worker/{}/complete",
            self.config.orchestrator_url, job_id
        );
        let payload = serde_json::json!({
            "status": "success",
            "result": "tool-session-closed",
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(job_token)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(OrchestratorError::HttpStatus { status, body });
        }

        Ok(())
    }
}
