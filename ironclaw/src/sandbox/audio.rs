//! Audio sandbox manager — microphone input (STT) and speaker output (TTS).
//!
//! Provides safe audio I/O for the AI agent inside an isolated Docker container.
//! The container has access to a virtual audio device (PulseAudio/ALSA loopback)
//! and exposes two capabilities:
//!
//! - **Speech-to-Text (STT)**: Record audio from the host microphone (with explicit
//!   user consent), transcribe it via Whisper or a compatible API, and return the
//!   transcript as text.
//! - **Text-to-Speech (TTS)**: Synthesize speech from text using a TTS engine
//!   (Piper, espeak-ng, or an OpenAI-compatible TTS API) and play it back through
//!   the host speakers.
//!
//! # Architecture
//!
//! ```text
//! Host
//!   └── Docker container (ironclaw-audio:latest)
//!         ├── PulseAudio (virtual sink/source — no direct hardware access)
//!         ├── ffmpeg / sox  — audio capture, format conversion, normalization
//!         ├── whisper.cpp   — local STT (optional; falls back to API)
//!         └── piper / espeak-ng — local TTS (optional; falls back to API)
//!
//! Audio I/O path:
//!   Microphone → PulseAudio loopback → container capture → Whisper → transcript
//!   Text → TTS engine → PulseAudio loopback → host speakers
//! ```
//!
//! # Security properties
//!
//! - The container **cannot** access the host filesystem beyond `/audio-workspace`.
//! - Audio capture is **gated by explicit user consent** (`audio_session_start`).
//! - The container **cannot** access the host display or clipboard.
//! - Network access is proxied through the domain allowlist (same as `WorkspaceWrite`).
//! - Audio data is **never written to disk** inside the container unless the user
//!   explicitly requests a recording save.
//! - TTS output is played through a PulseAudio virtual sink; the container cannot
//!   directly access hardware audio devices.
//!
//! # Residual risks
//!
//! - The AI hears everything captured by the microphone during an active recording.
//!   Users must not speak sensitive information while a recording is in progress.
//! - TTS output is played through the host speakers; bystanders may hear it.
//! - Local STT/TTS models may have accuracy limitations.
//!
//! # Consent gate
//!
//! Every audio session requires explicit user consent before starting.
//! [`AudioSandboxManager::start_session`] returns [`AudioError::ConsentRequired`]
//! if consent has not been granted.

use std::sync::Arc;
use std::time::Duration;

use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, LogOutput, RemoveContainerOptions, StartContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::HostConfig;
use futures::StreamExt;
use tokio::sync::RwLock;

use crate::sandbox::config::SandboxConfig;
use crate::sandbox::container::connect_docker;
use crate::sandbox::proxy::{HttpProxy, NetworkProxyBuilder};

// ── Error types ───────────────────────────────────────────────────────────────

/// Errors specific to the audio sandbox.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// User has not granted consent for an audio session.
    ///
    /// Every audio session requires explicit user approval before starting,
    /// with a clear warning that the AI will be able to hear microphone input
    /// and play audio through the host speakers.
    #[error(
        "Audio session requires explicit user consent. \
         The AI will be able to hear microphone input and play audio through \
         the host speakers. Call audio_session_start(consent: true) to proceed."
    )]
    ConsentRequired,

    /// The audio container is not running.
    #[error("Audio container is not running. Call audio_session_start() first.")]
    NotRunning,

    /// Docker operation failed.
    #[error("Docker error: {0}")]
    Docker(String),

    /// Command execution inside the container failed.
    #[error("Exec failed (exit {exit_code}): {stderr}")]
    ExecFailed { exit_code: i64, stderr: String },

    /// Audio capture failed.
    #[error("Audio capture failed: {reason}")]
    CaptureFailed { reason: String },

    /// Transcription failed.
    #[error("Transcription failed: {reason}")]
    TranscriptionFailed { reason: String },

    /// TTS synthesis failed.
    #[error("TTS synthesis failed: {reason}")]
    TtsFailed { reason: String },

    /// Audio playback failed.
    #[error("Audio playback failed: {reason}")]
    PlaybackFailed { reason: String },

    /// Invalid input (e.g. text too long, unsupported format).
    #[error("Invalid input: {reason}")]
    InvalidInput { reason: String },

    /// Output exceeded the maximum allowed size.
    #[error("Output truncated at {max_bytes} bytes")]
    OutputTruncated { max_bytes: usize },
}

/// Result type for audio sandbox operations.
pub type AudioResult<T> = Result<T, AudioError>;

// ── Output types ──────────────────────────────────────────────────────────────

/// Output from a command executed inside the audio container.
#[derive(Debug, Clone)]
pub struct AudioExecOutput {
    /// Exit code.
    pub exit_code: i64,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// How long the command ran.
    pub duration: Duration,
}

/// Result of a speech-to-text transcription.
#[derive(Debug, Clone)]
pub struct TranscriptResult {
    /// The transcribed text.
    pub text: String,
    /// Detected language (ISO 639-1 code, e.g. "en"), if available.
    pub language: Option<String>,
    /// Confidence score in [0.0, 1.0], if available.
    pub confidence: Option<f32>,
    /// Duration of the audio that was transcribed.
    pub audio_duration: Duration,
    /// Which STT backend was used ("whisper_local", "whisper_api", "chat_completions").
    pub backend: String,
}

/// Result of a text-to-speech synthesis + playback.
#[derive(Debug, Clone)]
pub struct TtsResult {
    /// Number of characters synthesized.
    pub char_count: usize,
    /// Estimated playback duration.
    pub playback_duration: Duration,
    /// Which TTS backend was used ("piper", "espeak", "openai_tts", "chat_completions_tts").
    pub backend: String,
}

// ── Configuration ─────────────────────────────────────────────────────────────

/// Configuration for the audio sandbox.
#[derive(Debug, Clone)]
pub struct AudioSandboxConfig {
    /// Docker image to use (default: `ironclaw-audio:latest`).
    pub image: String,
    /// Memory limit in megabytes (default: 2048).
    pub memory_limit_mb: u64,
    /// CPU shares (default: 1024).
    pub cpu_shares: u32,
    /// Command timeout (default: 60 seconds).
    pub timeout: Duration,
    /// Maximum recording duration in seconds (default: 120).
    pub max_record_secs: u64,
    /// Maximum TTS text length in characters (default: 4096).
    pub max_tts_chars: usize,
    /// Maximum transcript output size in bytes (default: 64 KB).
    pub max_transcript_bytes: usize,
    /// Network allowlist (same as the main sandbox allowlist).
    pub network_allowlist: Vec<String>,
    /// Whether to auto-pull the image if not found.
    pub auto_pull_image: bool,
    /// Network proxy port (0 = auto-assign).
    pub proxy_port: u16,
    /// STT backend: "whisper_local", "whisper_api", or "chat_completions".
    pub stt_backend: String,
    /// TTS backend: "piper", "espeak", "openai_tts", or "chat_completions_tts".
    pub tts_backend: String,
    /// Whisper model size for local STT (e.g. "base", "small", "medium").
    pub whisper_model: String,
    /// Piper voice for local TTS (e.g. "en_US-lessac-medium").
    pub piper_voice: String,
    /// API base URL for remote STT/TTS (OpenAI-compatible).
    pub api_base_url: Option<String>,
    /// STT model name for API-based transcription.
    pub stt_model: String,
    /// TTS model name for API-based synthesis.
    pub tts_model: String,
    /// TTS voice name for API-based synthesis.
    pub tts_voice: String,
}

impl Default for AudioSandboxConfig {
    fn default() -> Self {
        Self {
            image: "ironclaw-audio:latest".to_string(),
            memory_limit_mb: 2048,
            cpu_shares: 1024,
            timeout: Duration::from_secs(60),
            max_record_secs: 120,
            max_tts_chars: 4096,
            max_transcript_bytes: 64 * 1024,
            network_allowlist: crate::sandbox::config::default_allowlist(),
            auto_pull_image: true,
            proxy_port: 0,
            stt_backend: "whisper_local".to_string(),
            tts_backend: "piper".to_string(),
            whisper_model: "base".to_string(),
            piper_voice: "en_US-lessac-medium".to_string(),
            api_base_url: None,
            stt_model: "whisper-1".to_string(),
            tts_model: "tts-1".to_string(),
            tts_voice: "alloy".to_string(),
        }
    }
}

// ── Manager ───────────────────────────────────────────────────────────────────

/// Internal state of the audio sandbox.
#[derive(Debug, Default)]
struct AudioState {
    /// Whether the user has granted consent for this session.
    consent_granted: bool,
    /// Running container ID, if any.
    container_id: Option<String>,
    /// Running proxy, if any.
    proxy: Option<HttpProxy>,
}

/// Manages the audio sandbox container lifecycle and audio I/O operations.
///
/// All audio operations require:
/// 1. A running container (started via [`start_session`]).
/// 2. Explicit user consent granted via [`start_session`] with `consent: true`.
///
/// The session start method has `ApprovalRequirement::Always` on the tool side —
/// it will always prompt the user for approval, even in auto-approved sessions.
pub struct AudioSandboxManager {
    config: AudioSandboxConfig,
    state: Arc<RwLock<AudioState>>,
}

impl AudioSandboxManager {
    /// Create a new manager with the given configuration.
    pub fn new(config: AudioSandboxConfig) -> Self {
        Self {
            config,
            state: Arc::new(RwLock::new(AudioState::default())),
        }
    }

    /// Create a new manager from a [`SandboxConfig`].
    pub fn from_sandbox_config(sandbox_cfg: &SandboxConfig) -> Self {
        let cfg = AudioSandboxConfig {
            image: if sandbox_cfg.image.contains("audio") {
                sandbox_cfg.image.clone()
            } else {
                "ironclaw-audio:latest".to_string()
            },
            memory_limit_mb: sandbox_cfg.memory_limit_mb,
            cpu_shares: sandbox_cfg.cpu_shares,
            timeout: sandbox_cfg.timeout,
            network_allowlist: sandbox_cfg.network_allowlist.clone(),
            auto_pull_image: sandbox_cfg.auto_pull_image,
            proxy_port: sandbox_cfg.proxy_port,
            ..Default::default()
        };
        Self::new(cfg)
    }

    // ── Session lifecycle ─────────────────────────────────────────────────────

    /// Start an audio session.
    ///
    /// `consent` must be `true` — the caller (tool layer) is responsible for
    /// obtaining explicit user approval before calling this method.
    ///
    /// # Errors
    ///
    /// - [`AudioError::ConsentRequired`] if `consent` is `false`.
    /// - [`AudioError::Docker`] if the container cannot be started.
    pub async fn start_session(&self, consent: bool) -> AudioResult<()> {
        if !consent {
            return Err(AudioError::ConsentRequired);
        }

        let mut state = self.state.write().await;

        // Already running — idempotent.
        if state.container_id.is_some() {
            state.consent_granted = true;
            return Ok(());
        }

        let docker = connect_docker()
            .await
            .map_err(|e| AudioError::Docker(e.to_string()))?;

        // Start network proxy.
        let proxy = NetworkProxyBuilder::new()
            .allowlist(self.config.network_allowlist.clone())
            .port(self.config.proxy_port)
            .build()
            .await
            .map_err(|e| AudioError::Docker(format!("proxy start failed: {e}")))?;

        let proxy_addr = proxy.addr();

        // Build container config.
        let container_name = format!("ironclaw-audio-{}", uuid::Uuid::new_v4());
        let env = vec![
            format!("DISPLAY="),
            format!("PULSE_SERVER=unix:/run/pulse/native"),
            format!("HTTP_PROXY=http://{proxy_addr}"),
            format!("HTTPS_PROXY=http://{proxy_addr}"),
            format!("WHISPER_MODEL={}", self.config.whisper_model),
            format!("PIPER_VOICE={}", self.config.piper_voice),
            format!("STT_BACKEND={}", self.config.stt_backend),
            format!("TTS_BACKEND={}", self.config.tts_backend),
            format!("STT_MODEL={}", self.config.stt_model),
            format!("TTS_MODEL={}", self.config.tts_model),
            format!("TTS_VOICE={}", self.config.tts_voice),
        ];

        let host_config = HostConfig {
            memory: Some((self.config.memory_limit_mb * 1024 * 1024) as i64),
            cpu_shares: Some(self.config.cpu_shares as i64),
            // No host filesystem mounts — audio workspace is ephemeral.
            binds: Some(vec![]),
            // Drop all capabilities; add only what's needed for audio.
            cap_drop: Some(vec!["ALL".to_string()]),
            cap_add: Some(vec![
                // Required for PulseAudio socket access.
                "SYS_NICE".to_string(),
            ]),
            // No privileged mode.
            privileged: Some(false),
            // Read-only root filesystem.
            readonly_rootfs: Some(true),
            // Writable tmpfs for audio scratch space.
            tmpfs: Some({
                let mut m = std::collections::HashMap::new();
                m.insert("/tmp".to_string(), "size=256m,mode=1777".to_string());
                m.insert("/run/pulse".to_string(), "size=16m".to_string());
                m
            }),
            // Network mode: bridge (proxy handles allowlist).
            network_mode: Some("bridge".to_string()),
            ..Default::default()
        };

        let create_opts = CreateContainerOptions {
            name: container_name.clone(),
            platform: None,
        };

        let container_cfg: Config<String> = Config {
            image: Some(self.config.image.clone()),
            env: Some(env),
            host_config: Some(host_config),
            // Keep container alive for exec commands.
            tty: Some(true),
            attach_stdin: Some(false),
            attach_stdout: Some(false),
            attach_stderr: Some(false),
            ..Default::default()
        };

        docker
            .create_container(Some(create_opts), container_cfg)
            .await
            .map_err(|e| AudioError::Docker(format!("create container: {e}")))?;

        docker
            .start_container(&container_name, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| AudioError::Docker(format!("start container: {e}")))?;

        state.container_id = Some(container_name);
        state.consent_granted = true;
        state.proxy = Some(proxy);

        tracing::info!("Audio sandbox session started");
        Ok(())
    }

    /// Stop the audio session and remove the container.
    pub async fn stop_session(&self) -> AudioResult<()> {
        let mut state = self.state.write().await;

        let container_id = match state.container_id.take() {
            Some(id) => id,
            None => return Ok(()), // Already stopped — idempotent.
        };

        // Stop proxy first.
        if let Some(proxy) = state.proxy.take() {
            proxy.shutdown().await;
        }

        let docker = connect_docker()
            .await
            .map_err(|e| AudioError::Docker(e.to_string()))?;

        docker
            .remove_container(
                &container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| AudioError::Docker(format!("remove container: {e}")))?;

        state.consent_granted = false;
        tracing::info!("Audio sandbox session stopped");
        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Execute a command inside the running container.
    async fn exec_in_container(
        &self,
        cmd: Vec<String>,
        timeout: Duration,
    ) -> AudioResult<AudioExecOutput> {
        let state = self.state.read().await;

        if !state.consent_granted {
            return Err(AudioError::ConsentRequired);
        }

        let container_id = state
            .container_id
            .as_ref()
            .ok_or(AudioError::NotRunning)?
            .clone();

        drop(state); // Release read lock before async Docker calls.

        let docker = connect_docker()
            .await
            .map_err(|e| AudioError::Docker(e.to_string()))?;

        let exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(cmd),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| AudioError::Docker(format!("create exec: {e}")))?;

        let start = std::time::Instant::now();

        let result = tokio::time::timeout(timeout, async {
            let output = docker
                .start_exec(&exec.id, None)
                .await
                .map_err(|e| AudioError::Docker(format!("start exec: {e}")))?;

            let mut stdout = String::new();
            let mut stderr = String::new();

            if let StartExecResults::Attached { mut output, .. } = output {
                while let Some(chunk) = output.next().await {
                    match chunk.map_err(|e| AudioError::Docker(e.to_string()))? {
                        LogOutput::StdOut { message } => {
                            stdout.push_str(&String::from_utf8_lossy(&message));
                        }
                        LogOutput::StdErr { message } => {
                            stderr.push_str(&String::from_utf8_lossy(&message));
                        }
                        _ => {}
                    }
                }
            }

            // Inspect exec to get exit code.
            let inspect = docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|e| AudioError::Docker(format!("inspect exec: {e}")))?;

            let exit_code = inspect.exit_code.unwrap_or(-1);

            Ok::<_, AudioError>(AudioExecOutput {
                exit_code,
                stdout,
                stderr,
                duration: start.elapsed(),
            })
        })
        .await
        .map_err(|_| AudioError::CaptureFailed {
            reason: format!("command timed out after {timeout:?}"),
        })??;

        Ok(result)
    }

    // ── Public audio operations ───────────────────────────────────────────────

    /// Record audio from the microphone for up to `duration_secs` seconds.
    ///
    /// Returns the raw audio as base64-encoded WAV data. The caller should
    /// immediately pass this to [`transcribe_audio`] or save it explicitly.
    ///
    /// # Errors
    ///
    /// - [`AudioError::ConsentRequired`] if the session has not been started.
    /// - [`AudioError::NotRunning`] if the container is not running.
    /// - [`AudioError::CaptureFailed`] if audio capture fails.
    /// - [`AudioError::InvalidInput`] if `duration_secs` exceeds [`AudioSandboxConfig::max_record_secs`].
    pub async fn record_audio(&self, duration_secs: u64) -> AudioResult<String> {
        if duration_secs == 0 || duration_secs > self.config.max_record_secs {
            return Err(AudioError::InvalidInput {
                reason: format!(
                    "duration_secs must be between 1 and {} (got {duration_secs})",
                    self.config.max_record_secs
                ),
            });
        }

        // Record via sox/arecord into /tmp/recording.wav inside the container.
        let cmd = vec![
            "/usr/local/bin/audio-record.sh".to_string(),
            duration_secs.to_string(),
            "/tmp/recording.wav".to_string(),
        ];

        let out = self
            .exec_in_container(
                cmd,
                Duration::from_secs(duration_secs + 10), // Extra buffer for startup.
            )
            .await?;

        if out.exit_code != 0 {
            return Err(AudioError::CaptureFailed {
                reason: format!("recorder exited {}: {}", out.exit_code, out.stderr.trim()),
            });
        }

        // Read the WAV file as base64.
        let read_cmd = vec![
            "base64".to_string(),
            "-w".to_string(),
            "0".to_string(),
            "/tmp/recording.wav".to_string(),
        ];

        let read_out = self
            .exec_in_container(read_cmd, self.config.timeout)
            .await?;

        if read_out.exit_code != 0 {
            return Err(AudioError::CaptureFailed {
                reason: format!("failed to read recording: {}", read_out.stderr.trim()),
            });
        }

        let b64 = read_out.stdout.trim().to_string();
        if b64.len() > self.config.max_transcript_bytes * 10 {
            return Err(AudioError::OutputTruncated {
                max_bytes: self.config.max_transcript_bytes * 10,
            });
        }

        Ok(b64)
    }

    /// Transcribe audio (base64-encoded WAV) to text using the configured STT backend.
    ///
    /// # Errors
    ///
    /// - [`AudioError::ConsentRequired`] if the session has not been started.
    /// - [`AudioError::NotRunning`] if the container is not running.
    /// - [`AudioError::TranscriptionFailed`] if transcription fails.
    pub async fn transcribe_audio(&self, audio_b64: &str) -> AudioResult<TranscriptResult> {
        // Write the base64 audio into the container's /tmp.
        let write_cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "echo '{}' | base64 -d > /tmp/input.wav",
                audio_b64.replace('\'', "'\\''")
            ),
        ];

        let write_out = self
            .exec_in_container(write_cmd, self.config.timeout)
            .await?;

        if write_out.exit_code != 0 {
            return Err(AudioError::TranscriptionFailed {
                reason: format!("failed to write audio: {}", write_out.stderr.trim()),
            });
        }

        // Run the transcription script.
        let transcribe_cmd = vec![
            "/usr/local/bin/audio-transcribe.sh".to_string(),
            "/tmp/input.wav".to_string(),
            "/tmp/transcript.json".to_string(),
        ];

        let start = std::time::Instant::now();
        let trans_out = self
            .exec_in_container(transcribe_cmd, self.config.timeout)
            .await?;

        if trans_out.exit_code != 0 {
            return Err(AudioError::TranscriptionFailed {
                reason: format!(
                    "transcription exited {}: {}",
                    trans_out.exit_code,
                    trans_out.stderr.trim()
                ),
            });
        }

        // Read the JSON transcript.
        let read_cmd = vec!["cat".to_string(), "/tmp/transcript.json".to_string()];
        let read_out = self
            .exec_in_container(read_cmd, self.config.timeout)
            .await?;

        if read_out.exit_code != 0 {
            return Err(AudioError::TranscriptionFailed {
                reason: format!("failed to read transcript: {}", read_out.stderr.trim()),
            });
        }

        // Parse the JSON output from the transcription script.
        // Expected format: {"text": "...", "language": "en", "confidence": 0.95}
        let json: serde_json::Value =
            serde_json::from_str(read_out.stdout.trim()).map_err(|e| {
                AudioError::TranscriptionFailed {
                    reason: format!("invalid transcript JSON: {e}"),
                }
            })?;

        let text = json
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if text.len() > self.config.max_transcript_bytes {
            return Err(AudioError::OutputTruncated {
                max_bytes: self.config.max_transcript_bytes,
            });
        }

        Ok(TranscriptResult {
            text,
            language: json
                .get("language")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            confidence: json
                .get("confidence")
                .and_then(|v| v.as_f64())
                .map(|f| f as f32),
            audio_duration: start.elapsed(),
            backend: self.config.stt_backend.clone(),
        })
    }

    /// Record audio and immediately transcribe it in one step.
    ///
    /// Convenience wrapper around [`record_audio`] + [`transcribe_audio`].
    pub async fn record_and_transcribe(&self, duration_secs: u64) -> AudioResult<TranscriptResult> {
        let audio_b64 = self.record_audio(duration_secs).await?;
        self.transcribe_audio(&audio_b64).await
    }

    /// Synthesize speech from text and play it through the host speakers.
    ///
    /// `text` must not exceed [`AudioSandboxConfig::max_tts_chars`] characters.
    ///
    /// # Errors
    ///
    /// - [`AudioError::ConsentRequired`] if the session has not been started.
    /// - [`AudioError::NotRunning`] if the container is not running.
    /// - [`AudioError::InvalidInput`] if `text` is empty or too long.
    /// - [`AudioError::TtsFailed`] if synthesis fails.
    /// - [`AudioError::PlaybackFailed`] if playback fails.
    pub async fn speak(&self, text: &str, voice: Option<&str>) -> AudioResult<TtsResult> {
        if text.is_empty() {
            return Err(AudioError::InvalidInput {
                reason: "text must not be empty".to_string(),
            });
        }
        if text.len() > self.config.max_tts_chars {
            return Err(AudioError::InvalidInput {
                reason: format!(
                    "text length {} exceeds maximum {} characters",
                    text.len(),
                    self.config.max_tts_chars
                ),
            });
        }

        let effective_voice = voice.unwrap_or(&self.config.tts_voice);

        // Write text to /tmp/tts_input.txt inside the container.
        let escaped = text.replace('\'', "'\\''");
        let write_cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("printf '%s' '{escaped}' > /tmp/tts_input.txt"),
        ];

        let write_out = self
            .exec_in_container(write_cmd, self.config.timeout)
            .await?;

        if write_out.exit_code != 0 {
            return Err(AudioError::TtsFailed {
                reason: format!("failed to write TTS input: {}", write_out.stderr.trim()),
            });
        }

        // Run TTS synthesis + playback script.
        let tts_cmd = vec![
            "/usr/local/bin/audio-speak.sh".to_string(),
            "/tmp/tts_input.txt".to_string(),
            effective_voice.to_string(),
        ];

        let start = std::time::Instant::now();
        let tts_out = self
            .exec_in_container(
                tts_cmd,
                Duration::from_secs(self.config.max_tts_chars as u64 / 10 + 30),
            )
            .await?;

        if tts_out.exit_code != 0 {
            return Err(AudioError::TtsFailed {
                reason: format!(
                    "TTS exited {}: {}",
                    tts_out.exit_code,
                    tts_out.stderr.trim()
                ),
            });
        }

        Ok(TtsResult {
            char_count: text.len(),
            playback_duration: start.elapsed(),
            backend: self.config.tts_backend.clone(),
        })
    }

    /// Return the current session status as a JSON value.
    pub async fn status(&self) -> serde_json::Value {
        let state = self.state.read().await;
        serde_json::json!({
            "running": state.container_id.is_some(),
            "consent_granted": state.consent_granted,
            "container_id": state.container_id.as_deref().unwrap_or(""),
            "stt_backend": self.config.stt_backend,
            "tts_backend": self.config.tts_backend,
            "whisper_model": self.config.whisper_model,
            "piper_voice": self.config.piper_voice,
            "max_record_secs": self.config.max_record_secs,
            "max_tts_chars": self.config.max_tts_chars,
        })
    }
}