//! Configuration for the Docker execution sandbox.

use std::time::Duration;

/// Configuration for the sandbox system.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Whether the sandbox is enabled.
    pub enabled: bool,
    /// Security policy for sandbox execution.
    pub policy: SandboxPolicy,
    /// Whether `FullAccess` policy is explicitly allowed.
    ///
    /// When `policy` is `FullAccess` but this field is `false`, the manager
    /// will return `SandboxError::Config` and refuse to execute. This is an
    /// intentional double opt-in to prevent accidental host execution.
    /// Set via `SANDBOX_ALLOW_FULL_ACCESS=true` env var.
    pub allow_full_access: bool,
    /// Default timeout for command execution.
    pub timeout: Duration,
    /// Memory limit in megabytes.
    pub memory_limit_mb: u64,
    /// CPU shares (relative weight, default 1024).
    pub cpu_shares: u32,
    /// Network allowlist for proxied requests.
    pub network_allowlist: Vec<String>,
    /// Docker image to use for the sandbox.
    pub image: String,
    /// Whether to auto-pull the image if not found.
    pub auto_pull_image: bool,
    /// Port for the HTTP proxy (0 = auto-assign).
    pub proxy_port: u16,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: true, // Startup check disables gracefully if Docker unavailable
            policy: SandboxPolicy::ReadOnly,
            allow_full_access: false,
            timeout: Duration::from_secs(120),
            memory_limit_mb: 2048,
            cpu_shares: 1024,
            network_allowlist: default_allowlist(),
            image: "ironclaw-worker:latest".to_string(),
            auto_pull_image: true,
            proxy_port: 0,
        }
    }
}

/// Security policy for sandbox execution.
///
/// ```text
/// ┌──────────────────────────┬──────────────────────────────┬────────────────────────────────┐
/// │ Policy                   │ Filesystem                   │ Network                        │
/// ├──────────────────────────┼──────────────────────────────┼────────────────────────────────┤
/// │ ReadOnly                 │ /workspace (ro)              │ Proxied (allowlist only)       │
/// │ WorkspaceWrite           │ /workspace (rw)              │ Proxied (allowlist only)       │
/// │ SelfImprovementWrite     │ /hermes-skills (rw only)     │ Orchestrator bridge only       │
/// │ DesktopAccess            │ /workspace (rw), /tmp (rw)   │ Proxied (allowlist only)       │
/// │ FullAccess               │ Full host                    │ Full network (DANGER)          │
/// └──────────────────────────┴──────────────────────────────┴────────────────────────────────┘
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxPolicy {
    /// Read-only access to workspace, proxied network.
    /// Use for: exploring code, fetching docs, read-only operations.
    #[default]
    ReadOnly,

    /// Read/write access to workspace, proxied network.
    /// Use for: building software, running tests, generating files.
    WorkspaceWrite,

    /// Restricted write access for self-improvement jobs.
    ///
    /// **Security properties:**
    /// - Writable surface: `/hermes-skills/` only (agent-created skills)
    /// - No memory volume mount — memory writes are proxied via `POST /orchestrator/memory-write`
    /// - Network: orchestrator internal bridge only (no internet, no LLM API keys in container)
    /// - Tool allowlist enforced by WASM bridge: only `skill_manage` + `memory`
    /// - Non-root UID 65534 (nobody)
    /// - Read-only root filesystem with explicit tmpfs for `/tmp`
    /// - seccomp: deny ptrace, mount, clone-newuser
    /// - AppArmor: deny writes outside `/hermes-skills/`
    ///
    /// Use for: Hermes background review, curator runs, SWE tasks.
    SelfImprovementWrite,

    /// Desktop app access via virtual display (Xvfb).
    ///
    /// **Security properties:**
    /// - Virtual display: Xvfb `:99` — **no connection to host `DISPLAY`**; the AI
    ///   cannot see the user's actual screen.
    /// - Writable surfaces: `/workspace` (rw) and `/tmp` (rw via tmpfs).
    /// - Network: proxied through the domain allowlist (same as `WorkspaceWrite`).
    /// - Clipboard: isolated — no host clipboard sharing.
    /// - Container filesystem: isolated from host (no host mounts beyond `/workspace`).
    /// - Input injection via `xdotool` inside the container only.
    /// - Screenshot captures the Xvfb framebuffer, not the host screen.
    ///
    /// **Residual risks (user must acknowledge):**
    /// - The AI sees everything rendered in the virtual display. Do not open
    ///   documents containing secrets inside the desktop session.
    /// - Input injection means the AI can type into any field, including
    ///   password fields rendered in the virtual display.
    /// - Prompt injection via app UI content is possible; accessibility tree
    ///   output is sanitised but not fully immune.
    ///
    /// Requires explicit user consent before a session starts (consent gate).
    /// Use for: GUI automation, desktop app testing, browser-based workflows.
    DesktopAccess,

    /// Full access (no sandbox). Use with extreme caution.
    ///
    /// **BLAST RADIUS**: This bypasses Docker entirely and executes commands
    /// via `sh -c` directly on the host with the agent process's full
    /// privileges. If prompt injection bypasses tool approval, arbitrary
    /// host shell commands can run. File system, network, and environment
    /// are completely unrestricted.
    ///
    /// Requires `SANDBOX_ALLOW_FULL_ACCESS=true` as a second opt-in.
    /// Without it, the sandbox manager will return `SandboxError::Config`
    /// and refuse to execute.
    FullAccess,
}

impl SandboxPolicy {
    /// Returns true if filesystem writes are allowed.
    pub fn allows_writes(&self) -> bool {
        matches!(
            self,
            SandboxPolicy::WorkspaceWrite
                | SandboxPolicy::SelfImprovementWrite
                | SandboxPolicy::DesktopAccess
                | SandboxPolicy::FullAccess
        )
    }

    /// Returns true if network requests bypass the proxy.
    pub fn has_full_network(&self) -> bool {
        matches!(self, SandboxPolicy::FullAccess)
    }

    /// Returns true if running in a container.
    pub fn is_sandboxed(&self) -> bool {
        !matches!(self, SandboxPolicy::FullAccess)
    }

    /// Returns true if this is a self-improvement policy (restricted tool allowlist).
    pub fn is_self_improvement(&self) -> bool {
        matches!(self, SandboxPolicy::SelfImprovementWrite)
    }

    /// Returns true if this policy enables desktop app access via virtual display.
    ///
    /// Desktop sessions run inside a container with Xvfb (virtual framebuffer).
    /// The AI can see and interact with everything rendered in the virtual display,
    /// but has **no** access to the host display server.
    pub fn is_desktop_access(&self) -> bool {
        matches!(self, SandboxPolicy::DesktopAccess)
    }

    /// Returns the writable path for this policy, if any.
    pub fn writable_path(&self) -> Option<&'static str> {
        match self {
            SandboxPolicy::WorkspaceWrite => Some("/workspace"),
            SandboxPolicy::SelfImprovementWrite => Some("/hermes-skills"),
            SandboxPolicy::DesktopAccess => Some("/workspace"),
            SandboxPolicy::FullAccess => Some("/"),
            SandboxPolicy::ReadOnly => None,
        }
    }
}

impl std::str::FromStr for SandboxPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "readonly" | "read_only" | "ro" => Ok(SandboxPolicy::ReadOnly),
            "workspacewrite" | "workspace_write" | "rw" => Ok(SandboxPolicy::WorkspaceWrite),
            "selfimprovementwrite" | "self_improvement_write" | "self_improve" => {
                Ok(SandboxPolicy::SelfImprovementWrite)
            }
            "desktopaccess" | "desktop_access" | "desktop" => Ok(SandboxPolicy::DesktopAccess),
            "fullaccess" | "full_access" | "full" | "none" => Ok(SandboxPolicy::FullAccess),
            _ => Err(format!(
                "invalid sandbox policy '{}', expected 'readonly', 'workspace_write', \
                 'self_improvement_write', 'desktop_access', or 'full_access'",
                s
            )),
        }
    }
}

/// Resource limits for container execution.
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Maximum memory in bytes.
    pub memory_bytes: u64,
    /// CPU shares (relative weight).
    pub cpu_shares: u32,
    /// Maximum execution time.
    pub timeout: Duration,
    /// Maximum output size in bytes.
    pub max_output_bytes: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 2 * 1024 * 1024 * 1024, // 2 GB
            cpu_shares: 1024,
            timeout: Duration::from_secs(120),
            max_output_bytes: 64 * 1024, // 64 KB
        }
    }
}

/// Default network allowlist for common development operations.
pub fn default_allowlist() -> Vec<String> {
    vec![
        // Package registries
        "crates.io".to_string(),
        "static.crates.io".to_string(),
        "index.crates.io".to_string(),
        "registry.npmjs.org".to_string(),
        "proxy.golang.org".to_string(),
        "pypi.org".to_string(),
        "files.pythonhosted.org".to_string(),
        // Documentation
        "docs.rs".to_string(),
        "doc.rust-lang.org".to_string(),
        "nodejs.org".to_string(),
        "go.dev".to_string(),
        "docs.python.org".to_string(),
        // Version control (read-only)
        "github.com".to_string(),
        "raw.githubusercontent.com".to_string(),
        "api.github.com".to_string(),
        "codeload.github.com".to_string(),
        // Common APIs (credentials will be injected by proxy)
        "api.openai.com".to_string(),
        "api.anthropic.com".to_string(),
        "api.near.ai".to_string(),
    ]
}

/// Default credential mappings for common APIs.
pub fn default_credential_mappings() -> Vec<crate::secrets::CredentialMapping> {
    use crate::secrets::CredentialMapping;

    vec![
        CredentialMapping::bearer("OPENAI_API_KEY", "api.openai.com"),
        CredentialMapping::header("ANTHROPIC_API_KEY", "x-api-key", "api.anthropic.com"),
        CredentialMapping::bearer("NEARAI_API_KEY", "api.near.ai"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_parsing() {
        assert_eq!(
            "readonly".parse::<SandboxPolicy>().unwrap(),
            SandboxPolicy::ReadOnly
        );
        assert_eq!(
            "workspace_write".parse::<SandboxPolicy>().unwrap(),
            SandboxPolicy::WorkspaceWrite
        );
        assert_eq!(
            "full_access".parse::<SandboxPolicy>().unwrap(),
            SandboxPolicy::FullAccess
        );
        assert_eq!(
            "desktop_access".parse::<SandboxPolicy>().unwrap(),
            SandboxPolicy::DesktopAccess
        );
        assert_eq!(
            "desktopaccess".parse::<SandboxPolicy>().unwrap(),
            SandboxPolicy::DesktopAccess
        );
        assert_eq!(
            "desktop".parse::<SandboxPolicy>().unwrap(),
            SandboxPolicy::DesktopAccess
        );
        assert!("invalid".parse::<SandboxPolicy>().is_err());
    }

    #[test]
    fn test_policy_properties() {
        assert!(!SandboxPolicy::ReadOnly.allows_writes());
        assert!(SandboxPolicy::WorkspaceWrite.allows_writes());
        assert!(SandboxPolicy::DesktopAccess.allows_writes());
        assert!(SandboxPolicy::FullAccess.allows_writes());

        assert!(!SandboxPolicy::ReadOnly.has_full_network());
        assert!(!SandboxPolicy::WorkspaceWrite.has_full_network());
        assert!(!SandboxPolicy::DesktopAccess.has_full_network());
        assert!(SandboxPolicy::FullAccess.has_full_network());

        assert!(SandboxPolicy::ReadOnly.is_sandboxed());
        assert!(SandboxPolicy::WorkspaceWrite.is_sandboxed());
        assert!(SandboxPolicy::DesktopAccess.is_sandboxed());
        assert!(!SandboxPolicy::FullAccess.is_sandboxed());
    }

    #[test]
    fn test_desktop_access_policy_properties() {
        assert!(SandboxPolicy::DesktopAccess.is_desktop_access());
        assert!(!SandboxPolicy::ReadOnly.is_desktop_access());
        assert!(!SandboxPolicy::WorkspaceWrite.is_desktop_access());
        assert!(!SandboxPolicy::SelfImprovementWrite.is_desktop_access());
        assert!(!SandboxPolicy::FullAccess.is_desktop_access());

        // Desktop access writes to /workspace (same as WorkspaceWrite)
        assert_eq!(
            SandboxPolicy::DesktopAccess.writable_path(),
            Some("/workspace")
        );

        // Desktop access is NOT self-improvement
        assert!(!SandboxPolicy::DesktopAccess.is_self_improvement());
    }

    #[test]
    fn test_desktop_access_error_message_includes_variant() {
        let err = "bogus".parse::<SandboxPolicy>().unwrap_err();
        assert!(
            err.contains("desktop_access"),
            "error message should mention desktop_access, got: {err}"
        );
    }

    #[test]
    fn test_default_allowlist_has_common_registries() {
        let allowlist = default_allowlist();
        assert!(allowlist.contains(&"crates.io".to_string()));
        assert!(allowlist.contains(&"registry.npmjs.org".to_string()));
        assert!(allowlist.contains(&"github.com".to_string()));
    }
}
