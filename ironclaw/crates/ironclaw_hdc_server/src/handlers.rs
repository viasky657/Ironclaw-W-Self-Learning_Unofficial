use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::SharedModel;
use crate::types::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatResponseMessage, ChatUsage,
    HealthResponse, ModelInfo, ModelsResponse, TrainRequest, TrainResponse,
};

/// Application state shared across all handlers.
#[derive(Clone)]
pub struct AppState {
    pub model: SharedModel,
    pub model_path: Option<std::path::PathBuf>,
}

// ---------------------------------------------------------------------------
// POST /v1/chat/completions — score handler (bearer token required)
// ---------------------------------------------------------------------------

pub async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    // Extract the last user message as the content to score.
    let content = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .unwrap_or("");

    let score = {
        let model = state.model.read().unwrap();
        model.score(content)
    };

    // Return the score as a chat completion response.
    // The score is embedded in the assistant message content as JSON.
    let score_json = serde_json::json!({
        "score": score,
        "verdict": if score >= 0.0 { "good_write" } else { "bad_write" },
    });

    let response = ChatCompletionResponse {
        id: format!("hdc-{}", uuid::Uuid::new_v4()),
        object: "chat.completion",
        model: req.model.unwrap_or_else(|| "hdc-dsv-local".to_string()),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatResponseMessage {
                role: "assistant",
                content: score_json.to_string(),
            },
            finish_reason: "stop",
        }],
        usage: ChatUsage {
            prompt_tokens: content.len() as u32 / 4,
            completion_tokens: 10,
            total_tokens: content.len() as u32 / 4 + 10,
        },
    };

    tracing::debug!(score = %score, "HDC: scored content");
    (StatusCode::OK, Json(response))
}

// ---------------------------------------------------------------------------
// POST /v1/train — online learning handler (bearer token required)
// ---------------------------------------------------------------------------

pub async fn train(
    State(state): State<AppState>,
    Json(req): Json<TrainRequest>,
) -> impl IntoResponse {
    {
        let mut model = state.model.write().unwrap();
        model.train(&req.content, req.outcome);
        tracing::info!(
            outcome = ?req.outcome,
            train_count = model.train_count(),
            "HDC: trained on new sample"
        );
    }

    // Persist the updated model if a path is configured.
    if let Some(path) = &state.model_path {
        let model = state.model.read().unwrap();
        if let Err(e) = model.save(path) {
            tracing::warn!(error = %e, "HDC: failed to save model after training");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(TrainResponse {
                    success: false,
                    message: format!("Training succeeded but model save failed: {}", e),
                }),
            );
        }
    }

    (
        StatusCode::OK,
        Json(TrainResponse {
            success: true,
            message: "Model updated successfully".to_string(),
        }),
    )
}

// ---------------------------------------------------------------------------
// GET /v1/models — model discovery (public)
// ---------------------------------------------------------------------------

pub async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let train_count = {
        let model = state.model.read().unwrap();
        model.train_count()
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let response = ModelsResponse {
        object: "list",
        data: vec![ModelInfo {
            id: format!("hdc-dsv-local-v{}", train_count),
            object: "model",
            created: now,
            owned_by: "ironclaw",
        }],
    };

    (StatusCode::OK, Json(response))
}

// ---------------------------------------------------------------------------
// GET /health — liveness (public)
// ---------------------------------------------------------------------------

pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let model_loaded = {
        let model = state.model.read().unwrap();
        model.train_count() > 0 || state.model_path.is_some()
    };

    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok",
            model_loaded,
        }),
    )
}
