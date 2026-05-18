use crate::config::DispatcherConfig;
use crate::types::{AgentInfo, LlmClientMode, ResolvedLlm};

/// Errors from LLM client resolution.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("No auxiliary LLM configured (SELF_IMPROVE_LLM_CLIENT=auxiliary). Set SELF_IMPROVE_LLM_CLIENT=main to use the main model, or configure an auxiliary provider.")]
    NoAuxiliaryClient,
    #[error("SELF_IMPROVE_LLM_CLIENT=local but SELF_IMPROVE_LLM_BASE_URL is not set")]
    LocalBaseUrlMissing,
    #[error("Local LLM server at {url} is unreachable: {reason}")]
    LocalServerUnreachable { url: String, reason: String },
    #[error("Unknown LLM client mode: {0}")]
    UnknownMode(String),
}

/// Resolve the (provider, model, base_url) triple for the review fork.
///
/// `agent_info` is a plain Rust struct populated from Python via PyO3 —
/// no dynamic `getattr` at resolution time.
///
/// Returns `Err(LlmError)` if no client is available (caller should skip the cycle).
pub fn resolve_llm_client(
    config: &DispatcherConfig,
    agent_info: &AgentInfo,
) -> Result<ResolvedLlm, LlmError> {
    let mode = LlmClientMode::from_str(&config.llm_client_mode);

    match mode {
        LlmClientMode::Main => {
            // Use the same provider/model as the parent agent turn.
            tracing::debug!(
                provider = %agent_info.provider,
                model = %agent_info.model,
                "Self-improve LLM: main mode"
            );
            Ok(ResolvedLlm {
                provider: agent_info.provider.clone(),
                model: agent_info.model.clone(),
                base_url: agent_info.base_url.clone(),
            })
        }

        LlmClientMode::Local => {
            let base_url = config
                .llm_base_url
                .clone()
                .ok_or(LlmError::LocalBaseUrlMissing)?;
            let model = config
                .llm_model
                .clone()
                .unwrap_or_else(|| "hdc-dsv-local".to_string());

            // Verify the local server is reachable (synchronous probe via blocking reqwest).
            let models_url = format!(
                "{}/v1/models",
                base_url.trim_end_matches('/').replace("/v1", "")
            );
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .map_err(|e| LlmError::LocalServerUnreachable {
                    url: base_url.clone(),
                    reason: e.to_string(),
                })?;
            client
                .get(&models_url)
                .send()
                .map_err(|e| LlmError::LocalServerUnreachable {
                    url: base_url.clone(),
                    reason: e.to_string(),
                })?;

            tracing::debug!(base_url = %base_url, model = %model, "Self-improve LLM: local mode");
            Ok(ResolvedLlm {
                provider: "openai_compatible".to_string(),
                model,
                base_url: Some(base_url),
            })
        }

        LlmClientMode::Auxiliary => {
            // Auxiliary mode: the orchestrator resolves the auxiliary client
            // server-side. We signal this by setting provider="auxiliary".
            // If no auxiliary provider is configured, the orchestrator will
            // reject the job — we don't silently fall back to the main model.
            tracing::debug!("Self-improve LLM: auxiliary mode");
            Ok(ResolvedLlm {
                provider: "auxiliary".to_string(),
                model: "auxiliary".to_string(),
                base_url: None,
            })
        }
    }
}
