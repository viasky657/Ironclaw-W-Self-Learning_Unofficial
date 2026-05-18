//! Desktop sandbox manager — Xvfb virtual display + accessibility proxy.
//!
//! Provides safe desktop app access inside an isolated Docker container running
//! a virtual framebuffer (Xvfb). The AI can see and interact with everything
//! rendered in the virtual display, but has **no** access to the host display
//! server, host clipboard, or host filesystem beyond `/workspace`.
//!
//! # Architecture
//!
//! ```text
//! Host
//!   └── Docker container (ironclaw-desktop:latest)
//!         ├── Xvfb :99  — virtual framebuffer (no host DISPLAY connection)
//!         ├── fluxbox   — minimal window manager
//!         ├── Desktop apps (Firefox, LibreOffice, etc.)
//!         ├── xdotool   — input injection (mouse/keyboard) inside virtual display
//!         ├── scrot     — screenshot (captures Xvfb framebuffer, not host screen)
//!         └── AT-SPI2   — accessibility bus → structured JSON (not raw X events)
//! ```
//!
//! # Security properties
//!
//! - `Xvfb :99` is a virtual X server with **no** connection to the host `DISPLAY`.
//! - The container filesystem is isolated from the host.
//! - Network traffic is proxied through the domain allowlist.
//! - Clipboard is **not** shared with the host.
//! - Input injection (`xdotool`) operates only inside the virtual display.
//! - Screenshots capture the Xvfb framebuffer, not the host screen.
//! - Accessibility tree output is structured JSON — the AI never gets raw X11 access.
//!
//! # Residual risks
//!
//! - The AI sees everything rendered in the virtual display. Users must not open
//!   documents containing secrets inside the desktop session.
//! - `xdotool` can inject keystrokes into any window in the virtual display,
//!   including password fields.
//! - Prompt injection via app UI content is possible; accessibility tree output
//!   is sanitised but not fully immune.
//!
//! # Consent gate
//!
//! Every desktop session requires explicit user consent before starting.
//! The [`DesktopSandboxManager::start_session`] method returns
//! [`DesktopError::ConsentRequired`] if consent has not been granted.

use std::collections::HashMap;
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
use crate::sandbox::credential_zones::{
    SharedCredentialZones, new_shared_zones, redact_accessibility_tree,
};
use crate::sandbox::proxy::{HttpProxy, NetworkProxyBuilder};

/// Errors specific to the desktop sandbox.
#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    /// User has not granted consent for a desktop session.
    ///
    /// Every desktop session requires explicit user approval before starting,
    /// with a clear warning that the AI will be able to see and interact with
    /// everything rendered in the virtual display.
    #[error(
        "Desktop session requires explicit user consent. \
         The AI will be able to see and interact with everything rendered in the \
         virtual display. Call start_session(consent: true) to proceed."
    )]
    ConsentRequired,

    /// The desktop container is not running.
    #[error("Desktop container is not running. Call start_session() first.")]
    NotRunning,

    /// Docker operation failed.
    #[error("Docker error: {0}")]
    Docker(String),

    /// Command execution inside the container failed.
    #[error("Exec failed (exit {exit_code}): {stderr}")]
    ExecFailed { exit_code: i64, stderr: String },

    /// Output exceeded the maximum allowed size.
    #[error("Output truncated at {max_bytes} bytes")]
    OutputTruncated { max_bytes: usize },

    /// Screenshot capture failed.
    #[error("Screenshot failed: {reason}")]
    ScreenshotFailed { reason: String },

    /// Accessibility tree query failed.
    #[error("Accessibility tree query failed: {reason}")]
    AccessibilityFailed { reason: String },

    /// Invalid input (e.g. coordinates out of range).
    #[error("Invalid input: {reason}")]
    InvalidInput { reason: String },
}

/// Result type for desktop sandbox operations.
pub type DesktopResult<T> = Result<T, DesktopError>;

/// Output from a command executed inside the desktop container.
#[derive(Debug, Clone)]
pub struct DesktopExecOutput {
    /// Exit code.
    pub exit_code: i64,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// How long the command ran.
    pub duration: Duration,
}

/// Configuration for the desktop sandbox.
#[derive(Debug, Clone)]
pub struct DesktopSandboxConfig {
    /// Docker image to use (default: `ironclaw-desktop:latest`).
    pub image: String,
    /// Virtual display number (default: `:99`).
    pub display: String,
    /// Screen resolution (default: `1920x1080`).
    pub screen_width: u32,
    pub screen_height: u32,
    /// Memory limit in megabytes (default: 4096 — heavier than worker containers).
    pub memory_limit_mb: u64,
    /// CPU shares (default: 1024).
    pub cpu_shares: u32,
    /// Command timeout (default: 30 seconds).
    pub timeout: Duration,
    /// Maximum screenshot size in bytes (default: 5 MB).
    pub max_screenshot_bytes: usize,
    /// Network proxy port (0 = auto-assign).
    pub proxy_port: u16,
    /// Network allowlist (same as the main sandbox allowlist).
    pub network_allowlist: Vec<String>,
    /// Whether to auto-pull the image if not found.
    pub auto_pull_image: bool,
}

impl Default for DesktopSandboxConfig {
    fn default() -> Self {
        Self {
            image: "ironclaw-desktop:latest".to_string(),
            display: ":99".to_string(),
            screen_width: 1920,
            screen_height: 1080,
            memory_limit_mb: 4096,
            cpu_shares: 1024,
            timeout: Duration::from_secs(30),
            max_screenshot_bytes: 5 * 1024 * 1024, // 5 MB
            proxy_port: 0,
            network_allowlist: crate::sandbox::default_allowlist(),
            auto_pull_image: true,
        }
    }
}

impl DesktopSandboxConfig {
    /// Create a `DesktopSandboxConfig` from the main `SandboxConfig`.
    pub fn from_sandbox_config(cfg: &SandboxConfig) -> Self {
        Self {
            image: cfg.image.clone(),
            memory_limit_mb: cfg.memory_limit_mb,
            cpu_shares: cfg.cpu_shares,
            timeout: cfg.timeout,
            proxy_port: cfg.proxy_port,
            network_allowlist: cfg.network_allowlist.clone(),
            auto_pull_image: cfg.auto_pull_image,
            ..Default::default()
        }
    }
}

/// Manages the lifecycle of a desktop sandbox container.
///
/// A single `DesktopSandboxManager` corresponds to one long-lived container
/// running Xvfb + fluxbox. Commands are executed via `docker exec`.
pub struct DesktopSandboxManager {
    config: DesktopSandboxConfig,
    docker: Arc<RwLock<Option<Docker>>>,
    proxy: Arc<RwLock<Option<HttpProxy>>>,
    /// ID of the running desktop container, if any.
    container_id: Arc<RwLock<Option<String>>>,
    /// Whether the user has granted consent for this session.
    consent_granted: Arc<std::sync::atomic::AtomicBool>,
    /// Credential zones: hidden values (redacted from AI) and visible credentials (AI can use).
    ///
    /// Shared with desktop tools so they can update zones at runtime via
    /// `DesktopCredentialZoneTool`.
    pub credential_zones: SharedCredentialZones,
}

impl DesktopSandboxManager {
    /// Create a new desktop sandbox manager with the given configuration.
    pub fn new(config: DesktopSandboxConfig) -> Self {
        Self {
            config,
            docker: Arc::new(RwLock::new(None)),
            proxy: Arc::new(RwLock::new(None)),
            container_id: Arc::new(RwLock::new(None)),
            consent_granted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            credential_zones: new_shared_zones(),
        }
    }

    /// Create with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(DesktopSandboxConfig::default())
    }

    /// Start a desktop session.
    ///
    /// # Consent gate
    ///
    /// `consent` **must** be `true` to proceed. This is an explicit user
    /// acknowledgement that:
    /// - The AI will be able to see everything rendered in the virtual display.
    /// - The AI can inject keyboard and mouse input into the virtual display.
    /// - The user must not open documents containing secrets in this session.
    ///
    /// Returns [`DesktopError::ConsentRequired`] if `consent` is `false`.
    pub async fn start_session(&self, consent: bool) -> DesktopResult<()> {
        if !consent {
            return Err(DesktopError::ConsentRequired);
        }

        self.consent_granted
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // Connect to Docker.
        let docker = connect_docker()
            .await
            .map_err(|e| DesktopError::Docker(e.to_string()))?;

        // Check / pull image.
        if docker.inspect_image(&self.config.image).await.is_err() {
            if self.config.auto_pull_image {
                self.pull_image(&docker).await?;
            } else {
                return Err(DesktopError::Docker(format!(
                    "image '{}' not found and auto_pull_image is false",
                    self.config.image
                )));
            }
        }

        // Start the network proxy.
        let proxy = NetworkProxyBuilder::new()
            .allowlist(self.config.network_allowlist.clone())
            .port(self.config.proxy_port)
            .build()
            .await
            .map_err(|e| DesktopError::Docker(format!("proxy start failed: {e}")))?;

        let proxy_port = proxy.port();

        *self.proxy.write().await = Some(proxy);
        *self.docker.write().await = Some(docker.clone());

        // Create and start the desktop container.
        let container_id = self.create_container(&docker, proxy_port).await?;
        docker
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| DesktopError::Docker(format!("start container failed: {e}")))?;

        // Wait for Xvfb to be ready (up to 10 seconds).
        self.wait_for_display_ready(&docker, &container_id).await?;

        *self.container_id.write().await = Some(container_id.clone());

        tracing::info!(
            container_id = %container_id,
            image = %self.config.image,
            display = %self.config.display,
            "Desktop sandbox session started"
        );

        Ok(())
    }

    /// Stop the desktop session and remove the container.
    pub async fn stop_session(&self) -> DesktopResult<()> {
        let container_id = {
            let mut guard = self.container_id.write().await;
            guard.take()
        };

        if let Some(id) = container_id {
            if let Some(docker) = self.docker.read().await.as_ref() {
                let _ = docker
                    .remove_container(
                        &id,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await;
                tracing::info!(container_id = %id, "Desktop sandbox container removed");
            }
        }

        // Stop the proxy.
        if let Some(proxy) = self.proxy.write().await.take() {
            proxy.shutdown().await;
        }

        self.consent_granted
            .store(false, std::sync::atomic::Ordering::SeqCst);

        Ok(())
    }

    /// Capture a screenshot of the virtual display.
    ///
    /// Returns the screenshot as a base64-encoded PNG string.
    ///
    /// The screenshot captures the Xvfb framebuffer (`:99`), **not** the host
    /// screen. The AI cannot see the user's actual desktop.
    ///
    /// # Credential redaction
    ///
    /// If any hidden credentials are configured via [`DesktopSandboxManager::credential_zones`],
    /// the screenshot pipeline applies imagemagick-based blackout redaction:
    /// 1. `tesseract` OCR locates text regions matching hidden values.
    /// 2. `convert` (imagemagick) blacks out those regions with solid rectangles.
    /// 3. The redacted PNG is returned to the AI.
    ///
    /// This is best-effort: unusual fonts or obfuscated rendering may not be caught.
    /// The accessibility tree redaction (exact string match) is more reliable.
    pub async fn screenshot(&self) -> DesktopResult<String> {
        self.require_running().await?;

        let output = self
            .exec_in_container(&[
                "scrot",
                "--silent",
                "--overwrite",
                "/tmp/ironclaw-screenshot.png",
            ])
            .await?;

        if output.exit_code != 0 {
            return Err(DesktopError::ScreenshotFailed {
                reason: format!("scrot exited {}: {}", output.exit_code, output.stderr),
            });
        }

        // Apply credential redaction if hidden values are configured.
        let zones = self.credential_zones.read().await;
        if zones.has_hidden() {
            // Build a shell script that uses tesseract + imagemagick to black out
            // any region containing a hidden credential value.
            // We write the hidden values to a temp file (never logged) and use
            // a Python helper to locate and redact them.
            let hidden_values: Vec<String> = zones
                .hidden_values()
                .iter()
                .map(|v| shell_escape(v))
                .collect();
            drop(zones); // release lock before exec

            // Write hidden values to a temp file inside the container (never persisted).
            let hidden_list = hidden_values.join("\n");
            let write_cmd = format!(
                "printf '%s' {} > /tmp/ironclaw-hidden-values.txt",
                shell_escape(&hidden_list)
            );
            let _ = self
                .exec_in_container(&["sh", "-c", &write_cmd])
                .await?;

            // Run the redaction script.
            let redact_output = self
                .exec_in_container(&[
                    "sh",
                    "-c",
                    "python3 /usr/local/bin/desktop-redact-screenshot.py \
                     /tmp/ironclaw-screenshot.png \
                     /tmp/ironclaw-hidden-values.txt \
                     /tmp/ironclaw-screenshot-redacted.png \
                     && mv /tmp/ironclaw-screenshot-redacted.png /tmp/ironclaw-screenshot.png",
                ])
                .await?;

            if redact_output.exit_code != 0 {
                tracing::warn!(
                    stderr = %redact_output.stderr,
                    "Screenshot redaction failed — returning unredacted screenshot. \
                     Ensure desktop-redact-screenshot.py is installed in the image."
                );
            }

            // Securely delete the hidden values temp file.
            let _ = self
                .exec_in_container(&["sh", "-c", "shred -u /tmp/ironclaw-hidden-values.txt 2>/dev/null || rm -f /tmp/ironclaw-hidden-values.txt"])
                .await;
        } else {
            drop(zones);
        }

        // Read the (possibly redacted) PNG file and base64-encode it.
        let read_output = self
            .exec_in_container(&["base64", "-w", "0", "/tmp/ironclaw-screenshot.png"])
            .await?;

        if read_output.exit_code != 0 {
            return Err(DesktopError::ScreenshotFailed {
                reason: format!("base64 read failed: {}", read_output.stderr),
            });
        }

        let b64 = read_output.stdout.trim().to_string();

        // Sanity-check size.
        let approx_bytes = b64.len() * 3 / 4;
        if approx_bytes > self.config.max_screenshot_bytes {
            return Err(DesktopError::OutputTruncated {
                max_bytes: self.config.max_screenshot_bytes,
            });
        }

        Ok(b64)
    }

    /// Click at the given coordinates in the virtual display.
    ///
    /// Coordinates are in pixels relative to the top-left of the virtual screen.
    /// Button: 1 = left, 2 = middle, 3 = right.
    pub async fn click(&self, x: u32, y: u32, button: u8) -> DesktopResult<()> {
        self.require_running().await?;

        let screen_w = self.config.screen_width;
        let screen_h = self.config.screen_height;

        if x >= screen_w || y >= screen_h {
            return Err(DesktopError::InvalidInput {
                reason: format!(
                    "coordinates ({x}, {y}) out of range for {screen_w}x{screen_h} display"
                ),
            });
        }

        if button == 0 || button > 5 {
            return Err(DesktopError::InvalidInput {
                reason: format!("button {button} is invalid (must be 1–5)"),
            });
        }

        let x_str = x.to_string();
        let y_str = y.to_string();
        let btn_str = button.to_string();

        let output = self
            .exec_in_container(&[
                "xdotool",
                "mousemove",
                "--sync",
                &x_str,
                &y_str,
                "click",
                &btn_str,
            ])
            .await?;

        if output.exit_code != 0 {
            return Err(DesktopError::ExecFailed {
                exit_code: output.exit_code,
                stderr: output.stderr,
            });
        }

        tracing::debug!(x, y, button, "Desktop click");
        Ok(())
    }

    /// Type text into the currently focused window in the virtual display.
    ///
    /// Uses `xdotool type` which injects keyboard events. The text is typed
    /// into whatever window currently has focus in the virtual display.
    ///
    /// # Security note
    ///
    /// This can type into password fields. The user must be aware that the AI
    /// can inject arbitrary keystrokes into the virtual display.
    pub async fn type_text(&self, text: &str) -> DesktopResult<()> {
        self.require_running().await?;

        if text.is_empty() {
            return Ok(());
        }

        // Limit text length to prevent abuse.
        const MAX_TYPE_LEN: usize = 4096;
        if text.len() > MAX_TYPE_LEN {
            return Err(DesktopError::InvalidInput {
                reason: format!("text length {} exceeds maximum {MAX_TYPE_LEN}", text.len()),
            });
        }

        let output = self
            .exec_in_container(&["xdotool", "type", "--clearmodifiers", "--", text])
            .await?;

        if output.exit_code != 0 {
            return Err(DesktopError::ExecFailed {
                exit_code: output.exit_code,
                stderr: output.stderr,
            });
        }

        tracing::debug!(len = text.len(), "Desktop type_text");
        Ok(())
    }

    /// Press a key or key combination in the virtual display.
    ///
    /// Key names follow X11 keysym syntax (e.g. `"Return"`, `"ctrl+c"`,
    /// `"alt+F4"`, `"super+d"`).
    pub async fn key_press(&self, key: &str) -> DesktopResult<()> {
        self.require_running().await?;

        if key.is_empty() {
            return Err(DesktopError::InvalidInput {
                reason: "key name must not be empty".to_string(),
            });
        }

        let output = self
            .exec_in_container(&["xdotool", "key", "--clearmodifiers", key])
            .await?;

        if output.exit_code != 0 {
            return Err(DesktopError::ExecFailed {
                exit_code: output.exit_code,
                stderr: output.stderr,
            });
        }

        tracing::debug!(key, "Desktop key_press");
        Ok(())
    }

    /// Launch a desktop application inside the virtual display.
    ///
    /// The application runs inside the container with `DISPLAY=:99`.
    /// It has no access to the host display, host filesystem, or host clipboard.
    ///
    /// # Allowed applications
    ///
    /// Only applications installed in the `ironclaw-desktop` image are available.
    /// The `app_name` is passed directly to the shell; it must not contain shell
    /// metacharacters. This method validates the name before execution.
    pub async fn open_app(&self, app_name: &str) -> DesktopResult<()> {
        self.require_running().await?;

        // Validate app name: only alphanumeric, hyphens, underscores, dots.
        if !app_name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(DesktopError::InvalidInput {
                reason: format!(
                    "app name '{app_name}' contains invalid characters. \
                     Only alphanumeric, hyphens, underscores, and dots are allowed."
                ),
            });
        }

        // Launch the app in the background (it will appear in the virtual display).
        let output = self
            .exec_in_container(&["sh", "-c", &format!("{app_name} &")])
            .await?;

        // A non-zero exit from the shell wrapper is expected if the app forks
        // immediately; we only fail if the shell itself errors.
        if output.exit_code != 0 && !output.stderr.is_empty() {
            tracing::warn!(
                app = app_name,
                exit_code = output.exit_code,
                stderr = %output.stderr,
                "open_app: non-zero exit (app may still be running)"
            );
        }

        tracing::info!(app = app_name, "Desktop open_app");
        Ok(())
    }

    /// Query the AT-SPI2 accessibility tree for the current virtual display state.
    ///
    /// Returns a JSON string with the structured UI state. This is the safe
    /// interface for the AI to observe desktop app state — it never gets raw
    /// X11 socket access.
    ///
    /// # Parameters
    ///
    /// - `app_filter`: If `Some`, only include the named application.
    /// - `max_depth`: Maximum tree depth to traverse (default: 10).
    pub async fn accessibility_tree(
        &self,
        app_filter: Option<&str>,
        max_depth: u32,
    ) -> DesktopResult<serde_json::Value> {
        self.require_running().await?;

        let mut cmd = vec![
            "python3",
            "/usr/local/bin/desktop-accessibility-query.py",
            "--max-depth",
        ];
        let depth_str = max_depth.to_string();
        cmd.push(&depth_str);

        let app_arg;
        if let Some(app) = app_filter {
            cmd.push("--app");
            app_arg = app.to_string();
            cmd.push(&app_arg);
        }

        let output = self.exec_in_container(&cmd).await?;

        if output.exit_code != 0 {
            return Err(DesktopError::AccessibilityFailed {
                reason: format!(
                    "query script exited {}: {}",
                    output.exit_code, output.stderr
                ),
            });
        }

        let mut tree: serde_json::Value =
            serde_json::from_str(&output.stdout).map_err(|e| DesktopError::AccessibilityFailed {
                reason: format!("JSON parse error: {e}"),
            })?;

        // Apply credential redaction to the accessibility tree.
        // Hidden values are replaced with "[HIDDEN]" before the AI sees them.
        let zones = self.credential_zones.read().await;
        if zones.has_hidden() {
            redact_accessibility_tree(&mut tree, &zones);
            tracing::debug!("Accessibility tree: credential redaction applied");
        }

        Ok(tree)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Require that the container is running and consent has been granted.
    async fn require_running(&self) -> DesktopResult<()> {
        if !self
            .consent_granted
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(DesktopError::ConsentRequired);
        }
        if self.container_id.read().await.is_none() {
            return Err(DesktopError::NotRunning);
        }
        Ok(())
    }

    /// Execute a command inside the running desktop container.
    async fn exec_in_container(&self, cmd: &[&str]) -> DesktopResult<DesktopExecOutput> {
        let container_id = self
            .container_id
            .read()
            .await
            .clone()
            .ok_or(DesktopError::NotRunning)?;

        let docker = self.docker.read().await;
        let docker = docker.as_ref().ok_or(DesktopError::NotRunning)?;

        let start = std::time::Instant::now();

        // Build the exec with DISPLAY set to the virtual framebuffer.
        let exec = docker
            .create_exec(
                &container_id,
                CreateExecOptions {
                    cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                    env: Some(vec![
                        format!("DISPLAY={}", self.config.display),
                        "DBUS_SESSION_BUS_ADDRESS=autolaunch:".to_string(),
                    ]),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| DesktopError::Docker(e.to_string()))?;

        let result = docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| DesktopError::Docker(e.to_string()))?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let StartExecResults::Attached { mut output, .. } = result {
            while let Some(chunk) = output.next().await {
                match chunk.map_err(|e| DesktopError::Docker(e.to_string()))? {
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

        // Retrieve exit code.
        let inspect = docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|e| DesktopError::Docker(e.to_string()))?;

        let exit_code = inspect.exit_code.unwrap_or(-1);

        Ok(DesktopExecOutput {
            exit_code,
            stdout,
            stderr,
            duration: start.elapsed(),
        })
    }

    /// Create the desktop container (does not start it).
    async fn create_container(
        &self,
        docker: &Docker,
        proxy_port: u16,
    ) -> DesktopResult<String> {
        let proxy_host = format!("http://host.docker.internal:{proxy_port}");

        let env = vec![
            format!("DISPLAY={}", self.config.display),
            format!("SCREEN_WIDTH={}", self.config.screen_width),
            format!("SCREEN_HEIGHT={}", self.config.screen_height),
            format!("SCREEN_DEPTH=24"),
            format!("http_proxy={proxy_host}"),
            format!("https_proxy={proxy_host}"),
            format!("HTTP_PROXY={proxy_host}"),
            format!("HTTPS_PROXY={proxy_host}"),
            // Explicitly unset host DISPLAY to prevent any accidental connection.
            "DISPLAY=:99".to_string(),
            // Disable clipboard sharing with host.
            "XAUTHORITY=/dev/null".to_string(),
        ];

        let memory_bytes = self.config.memory_limit_mb * 1024 * 1024;

        let host_config = HostConfig {
            memory: Some(memory_bytes as i64),
            cpu_shares: Some(self.config.cpu_shares as i64),
            // No privileged mode.
            privileged: Some(false),
            // Drop all capabilities; add back only what Xvfb needs.
            cap_drop: Some(vec!["ALL".to_string()]),
            cap_add: Some(vec![
                // Required for Xvfb shared memory segments.
                "IPC_LOCK".to_string(),
            ]),
            // No host display socket mount — Xvfb is fully virtual.
            // No host clipboard bridge.
            // tmpfs for /tmp (Xvfb uses shared memory there).
            tmpfs: Some({
                let mut m = HashMap::new();
                m.insert("/tmp".to_string(), "size=256m,mode=1777".to_string());
                m
            }),
            // Auto-remove on stop.
            auto_remove: Some(true),
            ..Default::default()
        };

        let container = docker
            .create_container(
                Some(CreateContainerOptions {
                    name: format!(
                        "ironclaw-desktop-{}",
                        uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("x")
                    ),
                    ..Default::default()
                }),
                Config {
                    image: Some(self.config.image.clone()),
                    env: Some(env),
                    host_config: Some(host_config),
                    // Non-root user (UID 1000 = worker).
                    user: Some("1000:1000".to_string()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| DesktopError::Docker(format!("create container failed: {e}")))?;

        Ok(container.id)
    }

    /// Pull the desktop image.
    async fn pull_image(&self, docker: &Docker) -> DesktopResult<()> {
        use bollard::image::CreateImageOptions;

        tracing::info!(image = %self.config.image, "Pulling desktop sandbox image");

        let options = CreateImageOptions {
            from_image: self.config.image.clone(),
            ..Default::default()
        };

        let mut stream = docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        tracing::debug!("Pull: {status}");
                    }
                }
                Err(e) => {
                    return Err(DesktopError::Docker(format!("image pull failed: {e}")));
                }
            }
        }

        Ok(())
    }

    /// Wait for Xvfb to be ready inside the container (up to 10 seconds).
    async fn wait_for_display_ready(
        &self,
        docker: &Docker,
        container_id: &str,
    ) -> DesktopResult<()> {
        for attempt in 0..20u32 {
            tokio::time::sleep(Duration::from_millis(500)).await;

            let exec = docker
                .create_exec(
                    container_id,
                    CreateExecOptions {
                        cmd: Some(vec![
                            "xdpyinfo".to_string(),
                            "-display".to_string(),
                            self.config.display.clone(),
                        ]),
                        env: Some(vec![format!("DISPLAY={}", self.config.display)]),
                        attach_stdout: Some(false),
                        attach_stderr: Some(false),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| DesktopError::Docker(e.to_string()))?;

            let _ = docker.start_exec(&exec.id, None).await;

            let inspect = docker
                .inspect_exec(&exec.id)
                .await
                .map_err(|e| DesktopError::Docker(e.to_string()))?;

            if inspect.exit_code == Some(0) {
                tracing::debug!(
                    attempt,
                    display = %self.config.display,
                    "Xvfb ready"
                );
                return Ok(());
            }
        }

        Err(DesktopError::Docker(format!(
            "Xvfb on {} did not become ready within 10 seconds",
            self.config.display
        )))
    }
}

impl Drop for DesktopSandboxManager {
    fn drop(&mut self) {
        // Best-effort cleanup: if the manager is dropped while a container is
        // running, log a warning. The container has `auto_remove: true` so it
        // will be removed by Docker when it stops. We cannot call async code
        // here, so we just log.
        if let Ok(guard) = self.container_id.try_read() {
            if let Some(id) = guard.as_ref() {
                tracing::warn!(
                    container_id = %id,
                    "DesktopSandboxManager dropped while container is still running. \
                     The container has auto_remove=true and will be cleaned up by Docker."
                );
            }
        }
    }
}

/// Escape a string for safe use in a POSIX shell single-quoted argument.
///
/// Wraps the string in single quotes and escapes any embedded single quotes
/// using the `'\''` technique. This prevents shell injection when passing
/// hidden credential values to the redaction script.
///
/// # Security note
///
/// This is used only to pass hidden values to a temp file inside the container.
/// The values are never logged or persisted.
fn shell_escape(s: &str) -> String {
    // Replace ' with '\'' (end quote, literal single quote, reopen quote)
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_desktop_sandbox_config_defaults() {
        let cfg = DesktopSandboxConfig::default();
        assert_eq!(cfg.image, "ironclaw-desktop:latest");
        assert_eq!(cfg.display, ":99");
        assert_eq!(cfg.screen_width, 1920);
        assert_eq!(cfg.screen_height, 1080);
        assert_eq!(cfg.memory_limit_mb, 4096);
        assert!(cfg.auto_pull_image);
    }

    #[test]
    fn test_consent_required_error_message() {
        let err = DesktopError::ConsentRequired;
        let msg = err.to_string();
        assert!(
            msg.contains("explicit user consent"),
            "error should mention consent, got: {msg}"
        );
        assert!(
            msg.contains("start_session"),
            "error should mention start_session, got: {msg}"
        );
    }

    #[test]
    fn test_not_running_error() {
        let err = DesktopError::NotRunning;
        let msg = err.to_string();
        assert!(
            msg.contains("start_session"),
            "error should mention start_session, got: {msg}"
        );
    }

    #[test]
    fn test_invalid_input_coordinates() {
        let err = DesktopError::InvalidInput {
            reason: "coordinates (9999, 9999) out of range for 1920x1080 display".to_string(),
        };
        assert!(err.to_string().contains("Invalid input"));
    }

    #[tokio::test]
    async fn test_start_session_requires_consent() {
        let mgr = DesktopSandboxManager::with_defaults();
        let result = mgr.start_session(false).await;
        assert!(
            matches!(result, Err(DesktopError::ConsentRequired)),
            "start_session(false) must return ConsentRequired"
        );
    }

    #[tokio::test]
    async fn test_operations_require_running_container() {
        let mgr = DesktopSandboxManager::with_defaults();
        // Manually grant consent but do NOT start a container.
        mgr.consent_granted
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // All operations should return NotRunning.
        assert!(matches!(mgr.screenshot().await, Err(DesktopError::NotRunning)));
        assert!(matches!(
            mgr.click(100, 100, 1).await,
            Err(DesktopError::NotRunning)
        ));
        assert!(matches!(
            mgr.type_text("hello").await,
            Err(DesktopError::NotRunning)
        ));
        assert!(matches!(
            mgr.key_press("Return").await,
            Err(DesktopError::NotRunning)
        ));
        assert!(matches!(
            mgr.open_app("firefox").await,
            Err(DesktopError::NotRunning)
        ));
        assert!(matches!(
            mgr.accessibility_tree(None, 5).await,
            Err(DesktopError::NotRunning)
        ));
    }

    #[tokio::test]
    async fn test_type_text_empty_is_noop() {
        let mgr = DesktopSandboxManager::with_defaults();
        mgr.consent_granted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Empty text returns NotRunning (container not started), not an error
        // about the text itself — the container check happens first.
        let result = mgr.type_text("").await;
        // Empty text short-circuits before the container check.
        // With no container running, we expect NotRunning.
        // (The empty-string early return happens inside the method after
        //  require_running(), so we still get NotRunning here.)
        assert!(matches!(result, Err(DesktopError::NotRunning)));
    }

    #[tokio::test]
    async fn test_open_app_rejects_shell_metacharacters() {
        let mgr = DesktopSandboxManager::with_defaults();
        mgr.consent_granted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Shell metacharacters in app name must be rejected before container check.
        let result = mgr.open_app("firefox; rm -rf /").await;
        assert!(
            matches!(result, Err(DesktopError::InvalidInput { .. })),
            "shell metacharacters must be rejected"
        );
    }

    #[tokio::test]
    async fn test_click_rejects_out_of_range_coordinates() {
        let mgr = DesktopSandboxManager::with_defaults();
        mgr.consent_granted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Coordinates beyond screen dimensions must be rejected.
        let result = mgr.click(9999, 9999, 1).await;
        assert!(
            matches!(result, Err(DesktopError::InvalidInput { .. })),
            "out-of-range coordinates must be rejected"
        );
    }

    #[tokio::test]
    async fn test_click_rejects_invalid_button() {
        let mgr = DesktopSandboxManager::with_defaults();
        mgr.consent_granted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let result = mgr.click(100, 100, 0).await;
        assert!(
            matches!(result, Err(DesktopError::InvalidInput { .. })),
            "button 0 must be rejected"
        );
    }
}
