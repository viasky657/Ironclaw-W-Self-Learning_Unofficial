/// Dispatcher configuration — reads all env vars with typed defaults.
/// No `getattr` on Python objects; all configuration is resolved here.
#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    /// IronClaw orchestrator base URL (e.g. `http://localhost:8080`).
    pub orchestrator_url: String,
    /// Bearer token for the orchestrator API.
    pub orchestrator_token: String,
    /// LLM client mode: auxiliary | main | local.
    pub llm_client_mode: String,
    /// Optional base URL for local LLM server (only used when mode=local).
    pub llm_base_url: Option<String>,
    /// Optional model name for local LLM server.
    pub llm_model: Option<String>,
    /// Maximum number of turns the review fork may take.
    pub max_turns: u32,
    /// Maximum wall-clock seconds for the job.
    pub max_wall_seconds: u32,
    /// Maximum number of skill writes per job.
    pub max_skill_writes: u32,
    /// Maximum number of memory writes per job.
    pub max_memory_writes: u32,
    /// Sandbox policy name passed to the orchestrator.
    pub sandbox_policy: String,
    /// Whether the explicit opt-in flag is set.
    pub secure_self_improve: bool,
    /// Whether the explicit opt-out flag is set.
    pub prefer_local: bool,
}

impl DispatcherConfig {
    /// Read configuration from environment variables.
    pub fn from_env() -> Self {
        Self {
            orchestrator_url: std::env::var("IRONCLAW_ORCHESTRATOR_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_string())
                .trim_end_matches('/')
                .to_string(),
            orchestrator_token: std::env::var("IRONCLAW_ORCHESTRATOR_TOKEN")
                .unwrap_or_default(),
            llm_client_mode: std::env::var("SELF_IMPROVE_LLM_CLIENT")
                .unwrap_or_else(|_| "auxiliary".to_string())
                .to_lowercase(),
            llm_base_url: std::env::var("SELF_IMPROVE_LLM_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty()),
            llm_model: std::env::var("SELF_IMPROVE_LLM_MODEL")
                .ok()
                .filter(|s| !s.is_empty()),
            max_turns: parse_env_u32("SELF_IMPROVE_MAX_TURNS", 10),
            max_wall_seconds: parse_env_u32("SELF_IMPROVE_MAX_WALL_SECS", 120),
            max_skill_writes: parse_env_u32("SELF_IMPROVE_MAX_SKILL_WRITES", 10),
            max_memory_writes: parse_env_u32("SELF_IMPROVE_MAX_MEMORY_WRITES", 5),
            sandbox_policy: std::env::var("IRONCLAW_TOOL_SANDBOX_POLICY")
                .unwrap_or_else(|_| "WorkspaceWrite".to_string()),
            secure_self_improve: env_bool("HERMES_SECURE_SELF_IMPROVE", false),
            prefer_local: env_bool("HERMES_PREFER_LOCAL_SELF_IMPROVE", false),
        }
    }

    /// Return the health-check URL for the orchestrator.
    pub fn health_url(&self) -> String {
        format!("{}/health", self.orchestrator_url)
    }

    /// Return the self-improve job submission URL.
    pub fn self_improve_url(&self) -> String {
        format!("{}/jobs/self-improve", self.orchestrator_url)
    }
}

fn parse_env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    match std::env::var(name)
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" => true,
        "0" | "false" | "no" => false,
        _ => default,
    }
}
