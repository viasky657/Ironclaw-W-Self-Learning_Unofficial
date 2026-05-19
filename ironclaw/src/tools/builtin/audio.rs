//! Audio tools — microphone input (STT) and speaker output (TTS).
//!
//! These tools allow the AI to listen to voice commands via the host microphone
//! and respond with synthesized speech through the host speakers, all routed
//! through an isolated Docker container with a virtual audio device.
//!
//! # Tools provided
//!
//! | Tool | Description |
//! |------|-------------|
//! | [`AudioSessionStartTool`] | Start an audio session (requires user consent) |
//! | [`AudioSessionStopTool`] | Stop the audio session |
//! | [`AudioRecordTool`] | Record audio from the microphone |
//! | [`AudioTranscribeTool`] | Transcribe recorded audio to text |
//! | [`AudioListenTool`] | Record + transcribe in one step |
//! | [`AudioSpeakTool`] | Synthesize and play speech from text |
//! | [`AudioStatusTool`] | Query audio session status |
//!
//! # Security
//!
//! All tools require:
//! 1. A running [`AudioSandboxManager`] (injected at construction).
//! 2. Explicit user consent granted via [`AudioSessionStartTool`].
//!
//! The session start tool has `ApprovalRequirement::Always` — it will always
//! prompt the user for approval, even in auto-approved sessions. This is the
//! consent gate described in the architecture.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::context::JobContext;
use crate::sandbox::audio::{AudioError, AudioSandboxManager};
use crate::tools::tool::{
    ApprovalRequirement, RiskLevel, Tool, ToolDomain, ToolError, ToolOutput, ToolRateLimitConfig,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn audio_err(e: AudioError) -> ToolError {
    match e {
        AudioError::ConsentRequired => ToolError::NotAuthorized(e.to_string()),
        AudioError::NotRunning => ToolError::ExecutionFailed(e.to_string()),
        AudioError::InvalidInput { reason } => ToolError::InvalidParameters(reason),
        other => ToolError::ExecutionFailed(other.to_string()),
    }
}

fn opt_u64(params: &serde_json::Value, key: &str) -> Option<u64> {
    params.get(key).and_then(|v| v.as_u64())
}

fn opt_str<'a>(params: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(|v| v.as_str())
}

fn require_str<'a>(params: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidParameters(format!("missing required parameter '{key}'")))
}

// ── AudioSessionStartTool ─────────────────────────────────────────────────────

/// Start an audio session inside the virtual audio sandbox.
///
/// **This tool always requires explicit user approval** (consent gate).
/// The user must acknowledge that:
/// - The AI will be able to hear everything captured by the microphone.
/// - The AI can play audio through the host speakers.
/// - The user must not speak sensitive information while recording is active.
///
/// The audio container uses a PulseAudio virtual device; the container cannot
/// directly access hardware audio devices.
pub struct AudioSessionStartTool {
    manager: Arc<AudioSandboxManager>,
}

impl AudioSessionStartTool {
    pub fn new(manager: Arc<AudioSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for AudioSessionStartTool {
    fn name(&self) -> &str {
        "audio_session_start"
    }

    fn description(&self) -> &str {
        "Start an audio session inside an isolated virtual audio sandbox. \
         The AI will be able to hear microphone input and play audio through \
         the host speakers. Requires explicit user consent (set consent=true). \
         The container uses a PulseAudio virtual device — it cannot directly \
         access hardware audio devices. \
         WARNING: Do not speak sensitive information while recording is active."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "consent": {
                    "type": "boolean",
                    "description": "Must be true. Confirms the user understands the AI will \
                                    hear microphone input and play audio through speakers."
                }
            },
            "required": ["consent"]
        })
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::High
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        Some(ToolRateLimitConfig::new(5, 20))
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let consent = params
            .get("consent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let start = std::time::Instant::now();
        self.manager
            .start_session(consent)
            .await
            .map_err(audio_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "status": "started",
                "message": "Audio session started. You can now use audio_listen, audio_record, \
                            audio_transcribe, and audio_speak tools."
            }),
            start.elapsed(),
        ))
    }
}

// ── AudioSessionStopTool ──────────────────────────────────────────────────────

/// Stop the audio session and remove the container.
pub struct AudioSessionStopTool {
    manager: Arc<AudioSandboxManager>,
}

impl AudioSessionStopTool {
    pub fn new(manager: Arc<AudioSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for AudioSessionStopTool {
    fn name(&self) -> &str {
        "audio_session_stop"
    }

    fn description(&self) -> &str {
        "Stop the audio session and remove the audio container. \
         Any in-progress recording or playback will be interrupted."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        self.manager.stop_session().await.map_err(audio_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "status": "stopped",
                "message": "Audio session stopped and container removed."
            }),
            start.elapsed(),
        ))
    }
}

// ── AudioRecordTool ───────────────────────────────────────────────────────────

/// Record audio from the microphone for a specified duration.
///
/// Returns the raw audio as a base64-encoded WAV string. Pass the result to
/// [`AudioTranscribeTool`] to convert it to text, or use [`AudioListenTool`]
/// to record and transcribe in one step.
pub struct AudioRecordTool {
    manager: Arc<AudioSandboxManager>,
}

impl AudioRecordTool {
    pub fn new(manager: Arc<AudioSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for AudioRecordTool {
    fn name(&self) -> &str {
        "audio_record"
    }

    fn description(&self) -> &str {
        "Record audio from the microphone for a specified duration. \
         Returns base64-encoded WAV audio data. \
         Use audio_transcribe to convert the audio to text, or use \
         audio_listen to record and transcribe in one step. \
         Requires an active audio session (call audio_session_start first)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "duration_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 120,
                    "description": "Recording duration in seconds (1–120). Default: 5."
                }
            }
        })
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        Some(ToolRateLimitConfig::new(10, 60))
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let duration_secs = opt_u64(&params, "duration_secs").unwrap_or(5);

        let start = std::time::Instant::now();
        let audio_b64 = self
            .manager
            .record_audio(duration_secs)
            .await
            .map_err(audio_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "audio_b64": audio_b64,
                "format": "wav",
                "duration_secs": duration_secs,
                "message": "Audio recorded. Pass audio_b64 to audio_transcribe to get text."
            }),
            start.elapsed(),
        ))
    }
}

// ── AudioTranscribeTool ───────────────────────────────────────────────────────

/// Transcribe base64-encoded WAV audio to text using the configured STT backend.
pub struct AudioTranscribeTool {
    manager: Arc<AudioSandboxManager>,
}

impl AudioTranscribeTool {
    pub fn new(manager: Arc<AudioSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for AudioTranscribeTool {
    fn name(&self) -> &str {
        "audio_transcribe"
    }

    fn description(&self) -> &str {
        "Transcribe base64-encoded WAV audio to text using the configured \
         speech-to-text backend (Whisper local, Whisper API, or chat_completions). \
         Pass the audio_b64 output from audio_record. \
         Requires an active audio session (call audio_session_start first)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "audio_b64": {
                    "type": "string",
                    "description": "Base64-encoded WAV audio data (from audio_record)."
                }
            },
            "required": ["audio_b64"]
        })
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::Never
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        Some(ToolRateLimitConfig::new(20, 120))
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let audio_b64 = require_str(&params, "audio_b64")?;

        let start = std::time::Instant::now();
        let result = self
            .manager
            .transcribe_audio(audio_b64)
            .await
            .map_err(audio_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "text": result.text,
                "language": result.language,
                "confidence": result.confidence,
                "backend": result.backend,
                "audio_duration_ms": result.audio_duration.as_millis(),
            }),
            start.elapsed(),
        ))
    }
}

// ── AudioListenTool ───────────────────────────────────────────────────────────

/// Record audio and immediately transcribe it in one step.
///
/// Convenience wrapper around [`AudioRecordTool`] + [`AudioTranscribeTool`].
/// Use this when you want to capture a voice command and get the text directly.
pub struct AudioListenTool {
    manager: Arc<AudioSandboxManager>,
}

impl AudioListenTool {
    pub fn new(manager: Arc<AudioSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for AudioListenTool {
    fn name(&self) -> &str {
        "audio_listen"
    }

    fn description(&self) -> &str {
        "Record audio from the microphone and immediately transcribe it to text. \
         Combines audio_record and audio_transcribe in one step. \
         Use this to capture voice commands or dictation. \
         Requires an active audio session (call audio_session_start first)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "duration_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 120,
                    "description": "Recording duration in seconds (1–120). Default: 5."
                }
            }
        })
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        Some(ToolRateLimitConfig::new(10, 60))
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let duration_secs = opt_u64(&params, "duration_secs").unwrap_or(5);

        let start = std::time::Instant::now();
        let result = self
            .manager
            .record_and_transcribe(duration_secs)
            .await
            .map_err(audio_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "text": result.text,
                "language": result.language,
                "confidence": result.confidence,
                "backend": result.backend,
                "duration_secs": duration_secs,
            }),
            start.elapsed(),
        ))
    }
}

// ── AudioSpeakTool ────────────────────────────────────────────────────────────

/// Synthesize speech from text and play it through the host speakers.
///
/// Uses the configured TTS backend (Piper local, espeak-ng, or OpenAI TTS API).
/// The text is synthesized inside the container and played through the
/// PulseAudio virtual sink, which routes to the host speakers.
pub struct AudioSpeakTool {
    manager: Arc<AudioSandboxManager>,
}

impl AudioSpeakTool {
    pub fn new(manager: Arc<AudioSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for AudioSpeakTool {
    fn name(&self) -> &str {
        "audio_speak"
    }

    fn description(&self) -> &str {
        "Synthesize speech from text and play it through the host speakers. \
         Uses the configured TTS backend (Piper local, espeak-ng, or OpenAI TTS API). \
         Text is limited to 4096 characters. \
         Requires an active audio session (call audio_session_start first)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "maxLength": 4096,
                    "description": "Text to synthesize and speak aloud (max 4096 characters)."
                },
                "voice": {
                    "type": "string",
                    "description": "Voice to use. For Piper: e.g. 'en_US-lessac-medium'. \
                                    For OpenAI TTS: 'alloy', 'echo', 'fable', 'onyx', 'nova', 'shimmer'. \
                                    Defaults to the configured voice."
                }
            },
            "required": ["text"]
        })
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        Some(ToolRateLimitConfig::new(20, 120))
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let text = require_str(&params, "text")?;
        let voice = opt_str(&params, "voice");

        let start = std::time::Instant::now();
        let result = self
            .manager
            .speak(text, voice)
            .await
            .map_err(audio_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "status": "played",
                "char_count": result.char_count,
                "playback_duration_ms": result.playback_duration.as_millis(),
                "backend": result.backend,
            }),
            start.elapsed(),
        ))
    }
}

// ── AudioStatusTool ───────────────────────────────────────────────────────────

/// Query the current audio session status.
pub struct AudioStatusTool {
    manager: Arc<AudioSandboxManager>,
}

impl AudioStatusTool {
    pub fn new(manager: Arc<AudioSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for AudioStatusTool {
    fn name(&self) -> &str {
        "audio_status"
    }

    fn description(&self) -> &str {
        "Query the current audio session status, including whether a session is \
         running, which STT/TTS backends are configured, and the current limits."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::Never
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let status = self.manager.status().await;
        Ok(ToolOutput::success(status, start.elapsed()))
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Build all audio tools sharing a single [`AudioSandboxManager`].
///
/// Returns a `Vec<Box<dyn Tool>>` containing all seven audio tools.
pub fn build_audio_tools(manager: Arc<AudioSandboxManager>) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(AudioSessionStartTool::new(Arc::clone(&manager))),
        Box::new(AudioSessionStopTool::new(Arc::clone(&manager))),
        Box::new(AudioRecordTool::new(Arc::clone(&manager))),
        Box::new(AudioTranscribeTool::new(Arc::clone(&manager))),
        Box::new(AudioListenTool::new(Arc::clone(&manager))),
        Box::new(AudioSpeakTool::new(Arc::clone(&manager))),
        Box::new(AudioStatusTool::new(Arc::clone(&manager))),
    ]
}
