//! Audio I/O configuration — STT (speech-to-text) and TTS (text-to-speech).
//!
//! Settings are loaded with priority: **DB/TOML > env > default**.
//!
//! # Environment variables
//!
//! | Variable | Default | Description |
//! |----------|---------|-------------|
//! | `AUDIO_ENABLED` | `false` | Enable audio I/O tools |
//! | `AUDIO_STT_BACKEND` | `whisper_local` | STT backend: `whisper_local`, `whisper_api`, `chat_completions` |
//! | `AUDIO_TTS_BACKEND` | `piper` | TTS backend: `piper`, `espeak`, `openai_tts`, `chat_completions_tts` |
//! | `AUDIO_WHISPER_MODEL` | `base` | Whisper model size: `tiny`, `base`, `small`, `medium` |
//! | `AUDIO_PIPER_VOICE` | `en_US-lessac-medium` | Piper voice name |
//! | `AUDIO_STT_MODEL` | `whisper-1` | API STT model name |
//! | `AUDIO_TTS_MODEL` | `tts-1` | API TTS model name |
//! | `AUDIO_TTS_VOICE` | `alloy` | API TTS voice |
//! | `AUDIO_STT_BASE_URL` | — | Base URL for STT API (overrides `LLM_BASE_URL`) |
//! | `AUDIO_TTS_BASE_URL` | — | Base URL for TTS API (overrides `LLM_BASE_URL`) |
//! | `AUDIO_MAX_RECORD_SECS` | `120` | Maximum recording duration in seconds |
//! | `AUDIO_MAX_TTS_CHARS` | `4096` | Maximum TTS text length in characters |
//! | `AUDIO_MEMORY_LIMIT_MB` | `2048` | Container memory limit in MB |
//! | `AUDIO_IMAGE` | `ironclaw-audio:latest` | Docker image for audio sandbox |
//! | `AUDIO_REQUIRE_CONSENT` | `true` | Require explicit user consent before session start |

use crate::config::helpers::{optional_env, parse_bool_env, parse_optional_env, validate_base_url};
use crate::error::ConfigError;
use crate::settings::Settings;

/// Audio I/O configuration.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    /// Whether audio I/O tools are enabled.
    pub enabled: bool,

    /// STT backend: "whisper_local", "whisper_api", or "chat_completions".
    pub stt_backend: String,

    /// TTS backend: "piper", "espeak", "openai_tts", or "chat_completions_tts".
    pub tts_backend: String,

    /// Whisper model size for local STT (e.g. "tiny", "base", "small", "medium").
    pub whisper_model: String,

    /// Piper voice for local TTS (e.g. "en_US-lessac-medium").
    pub piper_voice: String,

    /// API STT model name (used when stt_backend is "whisper_api" or "chat_completions").
    pub stt_model: String,

    /// API TTS model name (used when tts_backend is "openai_tts" or "chat_completions_tts").
    pub tts_model: String,

    /// API TTS voice (used when tts_backend is "openai_tts" or "chat_completions_tts").
    pub tts_voice: String,

    /// Base URL for STT API (overrides LLM_BASE_URL for STT calls).
    pub stt_base_url: Option<String>,

    /// Base URL for TTS API (overrides LLM_BASE_URL for TTS calls).
    pub tts_base_url: Option<String>,

    /// Maximum recording duration in seconds (default: 120).
    pub max_record_secs: u64,

    /// Maximum TTS text length in characters (default: 4096).
    pub max_tts_chars: usize,

    /// Container memory limit in megabytes (default: 2048).
    pub memory_limit_mb: u64,

    /// Docker image for the audio sandbox (default: "ironclaw-audio:latest").
    pub image: String,

    /// Whether to require explicit user consent before session start (default: true).
    ///
    /// This should always be `true` in production. Setting it to `false` is only
    /// intended for automated testing environments where no human is present.
    pub require_consent: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            stt_backend: "whisper_local".to_string(),
            tts_backend: "piper".to_string(),
            whisper_model: "base".to_string(),
            piper_voice: "en_US-lessac-medium".to_string(),
            stt_model: "whisper-1".to_string(),
            tts_model: "tts-1".to_string(),
            tts_voice: "alloy".to_string(),
            stt_base_url: None,
            tts_base_url: None,
            max_record_secs: 120,
            max_tts_chars: 4096,
            memory_limit_mb: 2048,
            image: "ironclaw-audio:latest".to_string(),
            require_consent: true,
        }
    }
}

impl AudioConfig {
    /// Resolve audio configuration from settings and environment variables.
    pub(crate) fn resolve(settings: &Settings) -> Result<Self, ConfigError> {
        // Tri-state: Some(true/false) = explicit DB value, None = unset (fall back to env).
        let enabled = match settings.audio.as_ref().map(|a| a.enabled) {
            Some(db_enabled) => db_enabled,
            None => parse_bool_env("AUDIO_ENABLED", false)?,
        };

        let stt_backend = optional_env("AUDIO_STT_BACKEND")?
            .unwrap_or_else(|| "whisper_local".to_string());

        let tts_backend = optional_env("AUDIO_TTS_BACKEND")?
            .unwrap_or_else(|| "piper".to_string());

        // Validate STT backend.
        match stt_backend.as_str() {
            "whisper_local" | "whisper_api" | "chat_completions" => {}
            other => {
                return Err(ConfigError::InvalidValue {
                    key: "AUDIO_STT_BACKEND".to_string(),
                    message: format!(
                        "must be 'whisper_local', 'whisper_api', or 'chat_completions', got '{other}'"
                    ),
                });
            }
        }

        // Validate TTS backend.
        match tts_backend.as_str() {
            "piper" | "espeak" | "openai_tts" | "chat_completions_tts" => {}
            other => {
                return Err(ConfigError::InvalidValue {
                    key: "AUDIO_TTS_BACKEND".to_string(),
                    message: format!(
                        "must be 'piper', 'espeak', 'openai_tts', or 'chat_completions_tts', got '{other}'"
                    ),
                });
            }
        }

        let whisper_model = optional_env("AUDIO_WHISPER_MODEL")?
            .unwrap_or_else(|| "base".to_string());

        // Validate Whisper model size.
        match whisper_model.as_str() {
            "tiny" | "base" | "small" | "medium" | "large" | "large-v2" | "large-v3" => {}
            other => {
                return Err(ConfigError::InvalidValue {
                    key: "AUDIO_WHISPER_MODEL".to_string(),
                    message: format!(
                        "must be 'tiny', 'base', 'small', 'medium', 'large', 'large-v2', or 'large-v3', got '{other}'"
                    ),
                });
            }
        }

        let piper_voice = optional_env("AUDIO_PIPER_VOICE")?
            .unwrap_or_else(|| "en_US-lessac-medium".to_string());

        let stt_model = optional_env("AUDIO_STT_MODEL")?
            .unwrap_or_else(|| "whisper-1".to_string());

        let tts_model = optional_env("AUDIO_TTS_MODEL")?
            .unwrap_or_else(|| "tts-1".to_string());

        let tts_voice = optional_env("AUDIO_TTS_VOICE")?
            .unwrap_or_else(|| "alloy".to_string());

        let stt_base_url = optional_env("AUDIO_STT_BASE_URL")?;
        let tts_base_url = optional_env("AUDIO_TTS_BASE_URL")?;

        // Validate base URLs to prevent SSRF.
        if let Some(ref url) = stt_base_url {
            validate_base_url(url, "AUDIO_STT_BASE_URL")?;
        }
        if let Some(ref url) = tts_base_url {
            validate_base_url(url, "AUDIO_TTS_BASE_URL")?;
        }

        let max_record_secs: u64 = parse_optional_env("AUDIO_MAX_RECORD_SECS", 120u64)?;
        if max_record_secs == 0 || max_record_secs > 600 {
            return Err(ConfigError::InvalidValue {
                key: "AUDIO_MAX_RECORD_SECS".to_string(),
                message: format!("must be between 1 and 600, got '{max_record_secs}'"),
            });
        }

        let max_tts_chars_raw: u64 = parse_optional_env("AUDIO_MAX_TTS_CHARS", 4096u64)?;
        if max_tts_chars_raw == 0 || max_tts_chars_raw > 32768 {
            return Err(ConfigError::InvalidValue {
                key: "AUDIO_MAX_TTS_CHARS".to_string(),
                message: format!("must be between 1 and 32768, got '{max_tts_chars_raw}'"),
            });
        }
        let max_tts_chars = max_tts_chars_raw as usize;

        let memory_limit_mb: u64 = parse_optional_env("AUDIO_MEMORY_LIMIT_MB", 2048u64)?;

        let image = optional_env("AUDIO_IMAGE")?
            .unwrap_or_else(|| "ironclaw-audio:latest".to_string());

        // `require_consent` is security-sensitive: env-only, never from DB.
        // Default is `true`; only override to `false` in test environments.
        let require_consent = parse_bool_env("AUDIO_REQUIRE_CONSENT", true)?;

        Ok(Self {
            enabled,
            stt_backend,
            tts_backend,
            whisper_model,
            piper_voice,
            stt_model,
            tts_model,
            tts_voice,
            stt_base_url,
            tts_base_url,
            max_record_secs,
            max_tts_chars,
            memory_limit_mb,
            image,
            require_consent,
        })
    }

    /// Build an [`AudioSandboxConfig`] from this config.
    pub fn to_sandbox_config(&self) -> crate::sandbox::AudioSandboxConfig {
        crate::sandbox::AudioSandboxConfig {
            image: self.image.clone(),
            memory_limit_mb: self.memory_limit_mb,
            max_record_secs: self.max_record_secs,
            max_tts_chars: self.max_tts_chars,
            stt_backend: self.stt_backend.clone(),
            tts_backend: self.tts_backend.clone(),
            whisper_model: self.whisper_model.clone(),
            piper_voice: self.piper_voice.clone(),
            stt_model: self.stt_model.clone(),
            tts_model: self.tts_model.clone(),
            tts_voice: self.tts_voice.clone(),
            api_base_url: self.stt_base_url.clone().or_else(|| self.tts_base_url.clone()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_audio_config() {
        let cfg = AudioConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.stt_backend, "whisper_local");
        assert_eq!(cfg.tts_backend, "piper");
        assert_eq!(cfg.whisper_model, "base");
        assert_eq!(cfg.piper_voice, "en_US-lessac-medium");
        assert_eq!(cfg.stt_model, "whisper-1");
        assert_eq!(cfg.tts_model, "tts-1");
        assert_eq!(cfg.tts_voice, "alloy");
        assert_eq!(cfg.max_record_secs, 120);
        assert_eq!(cfg.max_tts_chars, 4096);
        assert_eq!(cfg.memory_limit_mb, 2048);
        assert_eq!(cfg.image, "ironclaw-audio:latest");
        assert!(cfg.require_consent);
    }

    #[test]
    fn test_to_sandbox_config() {
        let cfg = AudioConfig::default();
        let sandbox_cfg = cfg.to_sandbox_config();
        assert_eq!(sandbox_cfg.image, "ironclaw-audio:latest");
        assert_eq!(sandbox_cfg.stt_backend, "whisper_local");
        assert_eq!(sandbox_cfg.tts_backend, "piper");
        assert_eq!(sandbox_cfg.max_record_secs, 120);
        assert_eq!(sandbox_cfg.max_tts_chars, 4096);
    }
}
