use crate::config::DispatcherConfig;
use crate::crypto::encrypt_snapshot;
use crate::llm_resolver::resolve_llm_client;
use crate::orchestrator_client::OrchestratorClient;
use crate::snapshot::build_minimal_snapshot;
use crate::types::{AgentInfo, DispatchResult, JobType, Message};

/// Trigger a sandboxed self-improvement job via the IronClaw orchestrator.
///
/// This is the main entry point. It:
/// 1. Checks whether IronClaw should be used.
/// 2. Resolves the LLM client (auxiliary / main / local).
/// 3. Serializes and encrypts the conversation snapshot.
/// 4. Submits the job to the IronClaw orchestrator.
/// 5. Returns the job_id (non-blocking — the orchestrator manages the container).
pub async fn trigger_self_improvement(
    config: &DispatcherConfig,
    agent_info: &AgentInfo,
    job_type: &str,
    messages: Option<Vec<Message>>,
) -> DispatchResult {
    if !should_use_ironclaw(config).await {
        tracing::debug!("Self-improve: IronClaw not available — signalling fallback");
        return DispatchResult::skipped();
    }

    // Resolve LLM client.
    let llm = match resolve_llm_client(config, agent_info) {
        Ok(llm) => llm,
        Err(e) => {
            tracing::warn!(error = %e, "Self-improve: LLM resolution failed — skipping cycle");
            return DispatchResult::skipped();
        }
    };

    // Build snapshot.
    let msgs = messages.unwrap_or_else(|| agent_info.recent_messages.clone());
    let snapshot_value = build_minimal_snapshot(agent_info, &msgs);
    let snapshot_bytes = match serde_json::to_vec(&snapshot_value) {
        Ok(b) => b,
        Err(e) => {
            return DispatchResult::failed(format!("Snapshot serialization failed: {}", e));
        }
    };

    // Encrypt snapshot — hard failure if encryption fails (no plaintext fallback).
    let encrypted = match encrypt_snapshot(&snapshot_bytes) {
        Ok(e) => e,
        Err(e) => {
            return DispatchResult::failed(format!("Snapshot encryption failed: {}", e));
        }
    };

    // Submit to orchestrator.
    let client = match OrchestratorClient::new(config.clone()) {
        Ok(c) => c,
        Err(e) => {
            return DispatchResult::failed(format!("Failed to create HTTP client: {}", e));
        }
    };

    match client
        .submit_self_improve_job(job_type, &encrypted, &llm, &config.llm_client_mode)
        .await
    {
        Ok(job_id) => {
            tracing::info!(
                job_id = %job_id,
                job_type = %job_type,
                provider = %llm.provider,
                model = %llm.model,
                "Self-improve job submitted"
            );
            DispatchResult::submitted(job_id)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Self-improve: job submission failed");
            DispatchResult::failed(e.to_string())
        }
    }
}

/// Trigger a sandboxed self-improvement job in a background task.
///
/// Non-blocking wrapper around `trigger_self_improvement`.
/// Errors are logged but do not propagate to the caller.
pub fn trigger_self_improvement_async(
    config: DispatcherConfig,
    agent_info: AgentInfo,
    job_type: String,
    messages: Option<Vec<Message>>,
) {
    tokio::spawn(async move {
        let result =
            trigger_self_improvement(&config, &agent_info, &job_type, messages).await;
        if let Some(err) = result.error {
            tracing::warn!(error = %err, "Self-improve: background trigger failed");
        }
    });
}

/// Return `true` when IronClaw should handle self-improvement work.
///
/// Decision logic (in priority order):
/// 1. `HERMES_PREFER_LOCAL_SELF_IMPROVE=true` → always use local Hermes fork.
/// 2. `HERMES_SECURE_SELF_IMPROVE=true` → always use IronClaw (skip probe).
/// 3. Otherwise → probe `GET /health`. Use IronClaw when reachable.
pub async fn should_use_ironclaw(config: &DispatcherConfig) -> bool {
    if config.prefer_local {
        tracing::debug!("Self-improve: HERMES_PREFER_LOCAL_SELF_IMPROVE=true — using local fork");
        return false;
    }
    if config.secure_self_improve {
        // Explicit opt-in: trust the configured URL, skip the probe.
        return true;
    }
    // Auto-detect: prefer IronClaw when the orchestrator is reachable.
    let client = match OrchestratorClient::new(config.clone()) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let reachable = client.health_check().await;
    if reachable {
        tracing::debug!(
            url = %config.orchestrator_url,
            "Self-improve: IronClaw orchestrator reachable — routing through sandbox"
        );
    } else {
        tracing::debug!(
            "Self-improve: IronClaw orchestrator not reachable — falling back to local fork"
        );
    }
    reachable
}
