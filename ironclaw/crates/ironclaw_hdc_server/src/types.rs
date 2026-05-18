/// Request/response types for the HDC DSV server.
/// All types derive `Serialize`/`Deserialize` — no pickle, no arbitrary code execution.

/// Outcome label for online training.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, bincode::Encode, bincode::Decode)]
#[serde(rename_all = "snake_case")]
pub enum WriteOutcome {
    GoodWrite,
    BadWrite,
}

// ---------------------------------------------------------------------------
// Chat completions API (OpenAI-compatible)
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct ChatCompletionRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: &'static str,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: ChatUsage,
}

#[derive(Debug, serde::Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatResponseMessage,
    pub finish_reason: &'static str,
}

#[derive(Debug, serde::Serialize)]
pub struct ChatResponseMessage {
    pub role: &'static str,
    pub content: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ChatUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ---------------------------------------------------------------------------
// Training API
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct TrainRequest {
    pub content: String,
    pub outcome: WriteOutcome,
}

#[derive(Debug, serde::Serialize)]
pub struct TrainResponse {
    pub success: bool,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Models discovery API
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct ModelsResponse {
    pub object: &'static str,
    pub data: Vec<ModelInfo>,
}

#[derive(Debug, serde::Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub owned_by: &'static str,
}

// ---------------------------------------------------------------------------
// Health API
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub model_loaded: bool,
}
