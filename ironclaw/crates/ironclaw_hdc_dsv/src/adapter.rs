//! HDC DSV adapter implementation.
//!
//! Calls the local HDC DSV FastAPI server (`hdc_dsv_server.py`) to:
//! 1. Score proposed writes before they are committed (`score_write`)
//! 2. Send training updates after writes are committed or rolled back (`train`)

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::types::{HdcConfig, HdcError, HdcVerdict, WriteOutcome, WritePayload};

// ---------------------------------------------------------------------------
// OpenAI-compatible request/response types for the HDC DSV server
// ---------------------------------------------------------------------------

/// OpenAI-compatible chat completion request sent to the HDC DSV server.
#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

/// OpenAI-compatible chat completion response from the HDC DSV server.
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

/// HDC DSV score response (embedded in the completion content).
#[derive(Debug, Deserialize)]
struct HdcScoreResponse {
    /// Quality score in [0.0, 1.0].
    score: f32,
    /// Class label: "GOOD_WRITE" or "BAD_WRITE".
    label: String,
    /// Confidence in the classification.
    confidence: f32,
    /// Current training example count (for bootstrap mode detection).
    training_count: u32,
}

/// Training request sent to `POST /v1/train`.
#[derive(Debug, Serialize)]
struct TrainRequest {
    /// The write payload (encoded as text for hypervector encoding).
    content: String,
    /// The outcome label.
    label: String,
    /// Job type context.
    job_type: String,
    /// Target name (skill name or memory key).
    target: String,
}

/// Training response from `POST /v1/train`.
#[derive(Debug, Deserialize)]
struct TrainResponse {
    success: bool,
    training_count: u32,
    message: Option<String>,
}

// ---------------------------------------------------------------------------
// HdcDsvAdapter
// ---------------------------------------------------------------------------

/// Adapter for the local HDC DSV model server.
///
/// Calls `POST /v1/chat/completions` to score writes and
/// `POST /v1/train` to send online training updates.
pub struct HdcDsvAdapter {
    config: HdcConfig,
    #[cfg(not(target_arch = "wasm32"))]
    client: reqwest::Client,
}

impl HdcDsvAdapter {
    /// Create a new adapter with the given configuration.
    pub fn new(config: HdcConfig) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .expect("Failed to build reqwest client");

        Self {
            config,
            #[cfg(not(target_arch = "wasm32"))]
            client,
        }
    }

    /// Create from environment variables.
    ///
    /// Reads:
    /// - `IRONCLAW_HDC_SERVER_URL` (default: `http://localhost:8765/v1`)
    /// - `SELF_IMPROVE_HDC_THRESHOLD` (default: `0.4`)
    /// - `SELF_IMPROVE_HDC_BLOCK` (default: `false`)
    /// - `SELF_IMPROVE_HDC_TRAIN` (default: `false`)
    /// - `SELF_IMPROVE_HDC_BOOTSTRAP_MIN` (default: `50`)
    pub fn from_env() -> Self {
        let config = HdcConfig {
            server_url: std::env::var("IRONCLAW_HDC_SERVER_URL")
                .unwrap_or_else(|_| "http://localhost:8765/v1".to_string()),
            quality_threshold: std::env::var("SELF_IMPROVE_HDC_THRESHOLD")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.4),
            block_on_low_score: std::env::var("SELF_IMPROVE_HDC_BLOCK")
                .map(|s| s.to_lowercase() == "true")
                .unwrap_or(false),
            online_learning_enabled: std::env::var("SELF_IMPROVE_HDC_TRAIN")
                .map(|s| s.to_lowercase() == "true")
                .unwrap_or(false),
            bootstrap_min: std::env::var("SELF_IMPROVE_HDC_BOOTSTRAP_MIN")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(50),
            training_example_count: 0,
            timeout_secs: 5,
        };
        Self::new(config)
    }

    /// Score a proposed write payload.
    ///
    /// Returns `(score, verdict)`. The verdict determines whether the write
    /// should proceed, be flagged, or be blocked.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn score_write(&self, payload: &WritePayload) -> (f32, HdcVerdict) {
        // In bootstrap mode, gate is inactive — all writes pass.
        if !self.config.gate_active() {
            debug!(
                training_count = self.config.training_example_count,
                bootstrap_min = self.config.bootstrap_min,
                "HDC DSV gate in bootstrap mode — write passes unconditionally"
            );
            return (1.0, HdcVerdict::Bootstrap);
        }

        let prompt = self.build_score_prompt(payload);
        let req = ChatCompletionRequest {
            model: "hdc-dsv-local".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: prompt,
            }],
            max_tokens: 256,
        };

        let url = format!("{}/chat/completions", self.config.server_url);
        let result = self.client.post(&url).json(&req).send().await;

        match result {
            Err(e) => {
                let reason = format!("HDC server unreachable: {}", e);
                warn!("{}", reason);
                if self.config.block_on_low_score {
                    (0.0, HdcVerdict::FailClosed { reason })
                } else {
                    (1.0, HdcVerdict::FailOpen { reason })
                }
            }
            Ok(resp) => {
                let status = resp.status().as_u16();
                if !resp.status().is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    let reason = format!("HDC server error {}: {}", status, body);
                    warn!("{}", reason);
                    if self.config.block_on_low_score {
                        return (0.0, HdcVerdict::FailClosed { reason });
                    } else {
                        return (1.0, HdcVerdict::FailOpen { reason });
                    }
                }

                match resp.json::<ChatCompletionResponse>().await {
                    Err(e) => {
                        let reason = format!("Failed to parse HDC response: {}", e);
                        warn!("{}", reason);
                        if self.config.block_on_low_score {
                            (0.0, HdcVerdict::FailClosed { reason })
                        } else {
                            (1.0, HdcVerdict::FailOpen { reason })
                        }
                    }
                    Ok(completion) => {
                        let content = completion
                            .choices
                            .first()
                            .map(|c| c.message.content.as_str())
                            .unwrap_or("{}");

                        match serde_json::from_str::<HdcScoreResponse>(content) {
                            Err(e) => {
                                let reason = format!("Failed to parse HDC score JSON: {}", e);
                                warn!("{}", reason);
                                if self.config.block_on_low_score {
                                    (0.0, HdcVerdict::FailClosed { reason })
                                } else {
                                    (1.0, HdcVerdict::FailOpen { reason })
                                }
                            }
                            Ok(score_resp) => {
                                let score = score_resp.score.clamp(0.0, 1.0);
                                let verdict = if score >= self.config.quality_threshold {
                                    HdcVerdict::Pass { score }
                                } else if self.config.block_on_low_score {
                                    HdcVerdict::Blocked {
                                        score,
                                        threshold: self.config.quality_threshold,
                                    }
                                } else {
                                    HdcVerdict::Flagged {
                                        score,
                                        threshold: self.config.quality_threshold,
                                    }
                                };
                                debug!(
                                    score = score,
                                    label = score_resp.label,
                                    confidence = score_resp.confidence,
                                    verdict = verdict.description(),
                                    "HDC DSV score"
                                );
                                (score, verdict)
                            }
                        }
                    }
                }
            }
        }
    }

    /// Send a training update after a write is committed or rolled back.
    ///
    /// This is a fire-and-forget call — errors are logged but do not affect
    /// the write outcome (the write has already been committed or rolled back).
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn train(&self, payload: &WritePayload, outcome: WriteOutcome) {
        if !self.config.online_learning_enabled {
            return;
        }

        let req = TrainRequest {
            content: payload.content.clone(),
            label: outcome.to_string(),
            job_type: payload.job_type.clone(),
            target: payload.target.clone(),
        };

        let url = format!("{}/train", self.config.server_url);
        match self.client.post(&url).json(&req).send().await {
            Err(e) => {
                warn!("HDC DSV training update failed (fire-and-forget): {}", e);
            }
            Ok(resp) => {
                if resp.status().is_success() {
                    if let Ok(train_resp) = resp.json::<TrainResponse>().await {
                        debug!(
                            outcome = %outcome,
                            training_count = train_resp.training_count,
                            "HDC DSV training update sent"
                        );
                    }
                } else {
                    warn!(
                        "HDC DSV training update returned {}: {}",
                        resp.status(),
                        resp.text().await.unwrap_or_default()
                    );
                }
            }
        }
    }

    /// Build the scoring prompt for the HDC DSV server.
    fn build_score_prompt(&self, payload: &WritePayload) -> String {
        serde_json::json!({
            "tool": payload.tool,
            "target": payload.target,
            "job_type": payload.job_type,
            "size_delta": payload.size_delta,
            "content_preview": &payload.content[..payload.content.len().min(512)],
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bootstrap_mode_passes_all() {
        let config = HdcConfig {
            bootstrap_min: 50,
            training_example_count: 10, // Below bootstrap_min
            ..Default::default()
        };
        assert!(!config.gate_active());
    }

    #[test]
    fn test_gate_active_after_bootstrap() {
        let config = HdcConfig {
            bootstrap_min: 50,
            training_example_count: 50, // At bootstrap_min
            ..Default::default()
        };
        assert!(config.gate_active());
    }

    #[test]
    fn test_verdict_blocked() {
        let verdict = HdcVerdict::Blocked {
            score: 0.2,
            threshold: 0.4,
        };
        assert!(verdict.is_blocked());
        assert_eq!(verdict.score(), Some(0.2));
    }

    #[test]
    fn test_verdict_pass() {
        let verdict = HdcVerdict::Pass { score: 0.8 };
        assert!(!verdict.is_blocked());
        assert_eq!(verdict.score(), Some(0.8));
    }

    #[test]
    fn test_verdict_fail_closed_blocks() {
        let verdict = HdcVerdict::FailClosed {
            reason: "server down".to_string(),
        };
        assert!(verdict.is_blocked());
    }

    #[test]
    fn test_verdict_fail_open_passes() {
        let verdict = HdcVerdict::FailOpen {
            reason: "server down".to_string(),
        };
        assert!(!verdict.is_blocked());
    }

    #[test]
    fn test_write_outcome_display() {
        assert_eq!(WriteOutcome::GoodWrite.to_string(), "GOOD_WRITE");
        assert_eq!(WriteOutcome::BadWrite.to_string(), "BAD_WRITE");
    }
}
