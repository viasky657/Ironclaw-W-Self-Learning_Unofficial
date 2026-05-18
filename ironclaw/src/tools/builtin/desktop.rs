//! Desktop app tools — virtual display interaction via Xvfb sandbox.
//!
//! These tools allow the AI to see and interact with desktop applications
//! running inside an isolated Docker container with a virtual framebuffer
//! (Xvfb). The AI has **no** access to the host display, host clipboard,
//! or host filesystem beyond `/workspace`.
//!
//! # Tools provided
//!
//! | Tool | Description |
//! |------|-------------|
//! | [`DesktopScreenshotTool`] | Capture the virtual display as a base64 PNG |
//! | [`DesktopClickTool`] | Click at (x, y) in the virtual display |
//! | [`DesktopTypeTool`] | Type text into the focused window |
//! | [`DesktopKeyPressTool`] | Press a key or key combination |
//! | [`DesktopOpenAppTool`] | Launch a desktop application |
//! | [`DesktopAccessibilityTreeTool`] | Query AT-SPI2 accessibility tree as JSON |
//! | [`DesktopSessionStartTool`] | Start a desktop session (requires user consent) |
//! | [`DesktopSessionStopTool`] | Stop the desktop session |
//!
//! # Security
//!
//! All tools require:
//! 1. A running [`DesktopSandboxManager`] (injected at construction).
//! 2. Explicit user consent granted via [`DesktopSessionStartTool`].
//!
//! The session start tool has `ApprovalRequirement::Always` — it will always
//! prompt the user for approval, even in auto-approved sessions. This is the
//! consent gate described in the architecture.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::context::JobContext;
use crate::sandbox::{DesktopError, DesktopSandboxManager};
use crate::tools::builtin::desktop_credential_zone::DesktopCredentialZoneTool;
use crate::tools::tool::{
    ApprovalRequirement, RiskLevel, Tool, ToolDomain, ToolError, ToolOutput, ToolRateLimitConfig,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn desktop_err(e: DesktopError) -> ToolError {
    match e {
        DesktopError::ConsentRequired => ToolError::NotAuthorized(e.to_string()),
        DesktopError::NotRunning => ToolError::ExecutionFailed(e.to_string()),
        DesktopError::InvalidInput { reason } => ToolError::InvalidParameters(reason),
        other => ToolError::ExecutionFailed(other.to_string()),
    }
}

fn require_str<'a>(
    params: &'a serde_json::Value,
    key: &str,
) -> Result<&'a str, ToolError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidParameters(format!("missing required parameter '{key}'")))
}

fn require_u32(params: &serde_json::Value, key: &str) -> Result<u32, ToolError> {
    params
        .get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .ok_or_else(|| ToolError::InvalidParameters(format!("missing required parameter '{key}'")))
}

// ── DesktopSessionStartTool ───────────────────────────────────────────────────

/// Start a desktop session inside the virtual display sandbox.
///
/// **This tool always requires explicit user approval** (consent gate).
/// The user must acknowledge that:
/// - The AI will be able to see everything rendered in the virtual display.
/// - The AI can inject keyboard and mouse input into the virtual display.
/// - The user must not open documents containing secrets in this session.
///
/// The virtual display (Xvfb `:99`) has **no** connection to the host display.
pub struct DesktopSessionStartTool {
    manager: Arc<DesktopSandboxManager>,
}

impl DesktopSessionStartTool {
    pub fn new(manager: Arc<DesktopSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DesktopSessionStartTool {
    fn name(&self) -> &str {
        "desktop_session_start"
    }

    fn description(&self) -> &str {
        "Start a desktop session inside an isolated virtual display (Xvfb). \
         The AI will be able to see and interact with everything rendered in the \
         virtual display. The virtual display has NO connection to the host screen. \
         IMPORTANT: Do not open documents containing secrets in this session. \
         This tool always requires explicit user approval."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "consent": {
                    "type": "boolean",
                    "description": "Must be true to confirm the user has acknowledged the security \
                                    implications of a desktop session. The AI will be able to see \
                                    and interact with everything in the virtual display."
                }
            },
            "required": ["consent"]
        })
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
            .map_err(desktop_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "status": "started",
                "message": "Desktop session started. Virtual display is ready. \
                            The AI can now see and interact with the virtual display. \
                            Remember: do not open documents containing secrets."
            }),
            start.elapsed(),
        ))
    }

    /// Desktop session start ALWAYS requires explicit user approval.
    ///
    /// This is the consent gate — it cannot be bypassed by session auto-approve.
    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    fn risk_level_for(&self, _params: &serde_json::Value) -> RiskLevel {
        RiskLevel::High
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn execution_timeout(&self) -> Duration {
        Duration::from_secs(60)
    }
}

// ── DesktopSessionStopTool ────────────────────────────────────────────────────

/// Stop the desktop session and remove the container.
pub struct DesktopSessionStopTool {
    manager: Arc<DesktopSandboxManager>,
}

impl DesktopSessionStopTool {
    pub fn new(manager: Arc<DesktopSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DesktopSessionStopTool {
    fn name(&self) -> &str {
        "desktop_session_stop"
    }

    fn description(&self) -> &str {
        "Stop the desktop session and remove the virtual display container. \
         All state inside the container is lost."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        self.manager.stop_session().await.map_err(desktop_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "status": "stopped",
                "message": "Desktop session stopped and container removed."
            }),
            start.elapsed(),
        ))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn risk_level_for(&self, _params: &serde_json::Value) -> RiskLevel {
        RiskLevel::Medium
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }
}

// ── DesktopScreenshotTool ─────────────────────────────────────────────────────

/// Capture a screenshot of the virtual display.
///
/// Returns a base64-encoded PNG of the Xvfb framebuffer (`:99`).
/// The AI cannot see the user's actual screen — only the virtual display.
pub struct DesktopScreenshotTool {
    manager: Arc<DesktopSandboxManager>,
}

impl DesktopScreenshotTool {
    pub fn new(manager: Arc<DesktopSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DesktopScreenshotTool {
    fn name(&self) -> &str {
        "desktop_screenshot"
    }

    fn description(&self) -> &str {
        "Capture a screenshot of the virtual display (Xvfb). \
         Returns a base64-encoded PNG image. \
         The screenshot shows only the virtual display — NOT the host screen. \
         Requires an active desktop session (call desktop_session_start first)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(
        &self,
        _params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = std::time::Instant::now();
        let b64 = self.manager.screenshot().await.map_err(desktop_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "image_base64": b64,
                "format": "png",
                "encoding": "base64"
            }),
            start.elapsed(),
        ))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::Never
    }

    fn risk_level_for(&self, _params: &serde_json::Value) -> RiskLevel {
        RiskLevel::Low
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn execution_timeout(&self) -> Duration {
        Duration::from_secs(15)
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        // Screenshots are read-only but can be expensive; limit to 60/min.
        Some(ToolRateLimitConfig::new(60, 600))
    }
}

// ── DesktopClickTool ──────────────────────────────────────────────────────────

/// Click at coordinates in the virtual display.
pub struct DesktopClickTool {
    manager: Arc<DesktopSandboxManager>,
}

impl DesktopClickTool {
    pub fn new(manager: Arc<DesktopSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DesktopClickTool {
    fn name(&self) -> &str {
        "desktop_click"
    }

    fn description(&self) -> &str {
        "Click at the given (x, y) coordinates in the virtual display. \
         Coordinates are in pixels from the top-left corner of the virtual screen. \
         Button: 1=left (default), 2=middle, 3=right. \
         Requires an active desktop session."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "x": {
                    "type": "integer",
                    "description": "X coordinate in pixels (0 = left edge).",
                    "minimum": 0
                },
                "y": {
                    "type": "integer",
                    "description": "Y coordinate in pixels (0 = top edge).",
                    "minimum": 0
                },
                "button": {
                    "type": "integer",
                    "description": "Mouse button: 1=left, 2=middle, 3=right (default: 1).",
                    "minimum": 1,
                    "maximum": 5,
                    "default": 1
                }
            },
            "required": ["x", "y"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let x = require_u32(&params, "x")?;
        let y = require_u32(&params, "y")?;
        let button = params
            .get("button")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as u8;

        let start = std::time::Instant::now();
        self.manager.click(x, y, button).await.map_err(desktop_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "clicked": true,
                "x": x,
                "y": y,
                "button": button
            }),
            start.elapsed(),
        ))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn risk_level_for(&self, _params: &serde_json::Value) -> RiskLevel {
        RiskLevel::Medium
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        Some(ToolRateLimitConfig::new(120, 1200))
    }
}

// ── DesktopTypeTool ───────────────────────────────────────────────────────────

/// Type text into the focused window in the virtual display.
pub struct DesktopTypeTool {
    manager: Arc<DesktopSandboxManager>,
}

impl DesktopTypeTool {
    pub fn new(manager: Arc<DesktopSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DesktopTypeTool {
    fn name(&self) -> &str {
        "desktop_type"
    }

    fn description(&self) -> &str {
        "Type text into the currently focused window in the virtual display. \
         Uses xdotool to inject keyboard events. \
         WARNING: This can type into password fields. \
         Requires an active desktop session."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to type. Maximum 4096 characters.",
                    "maxLength": 4096
                }
            },
            "required": ["text"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let text = require_str(&params, "text")?;

        let start = std::time::Instant::now();
        self.manager.type_text(text).await.map_err(desktop_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "typed": true,
                "length": text.len()
            }),
            start.elapsed(),
        ))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn risk_level_for(&self, _params: &serde_json::Value) -> RiskLevel {
        // Typing can inject into password fields — treat as medium risk.
        RiskLevel::Medium
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        Some(ToolRateLimitConfig::new(60, 600))
    }
}

// ── DesktopKeyPressTool ───────────────────────────────────────────────────────

/// Press a key or key combination in the virtual display.
pub struct DesktopKeyPressTool {
    manager: Arc<DesktopSandboxManager>,
}

impl DesktopKeyPressTool {
    pub fn new(manager: Arc<DesktopSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DesktopKeyPressTool {
    fn name(&self) -> &str {
        "desktop_key_press"
    }

    fn description(&self) -> &str {
        "Press a key or key combination in the virtual display. \
         Key names follow X11 keysym syntax: 'Return', 'Escape', 'ctrl+c', \
         'alt+F4', 'super+d', 'ctrl+shift+t', etc. \
         Requires an active desktop session."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "X11 keysym name or combination (e.g. 'Return', 'ctrl+c', 'alt+F4')."
                }
            },
            "required": ["key"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let key = require_str(&params, "key")?;

        let start = std::time::Instant::now();
        self.manager.key_press(key).await.map_err(desktop_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "pressed": true,
                "key": key
            }),
            start.elapsed(),
        ))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn risk_level_for(&self, _params: &serde_json::Value) -> RiskLevel {
        RiskLevel::Medium
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        Some(ToolRateLimitConfig::new(120, 1200))
    }
}

// ── DesktopOpenAppTool ────────────────────────────────────────────────────────

/// Launch a desktop application inside the virtual display.
pub struct DesktopOpenAppTool {
    manager: Arc<DesktopSandboxManager>,
}

impl DesktopOpenAppTool {
    pub fn new(manager: Arc<DesktopSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DesktopOpenAppTool {
    fn name(&self) -> &str {
        "desktop_open_app"
    }

    fn description(&self) -> &str {
        "Launch a desktop application inside the virtual display. \
         The application runs inside the isolated container — it has no access \
         to the host display, host filesystem, or host clipboard. \
         Available apps: firefox, libreoffice, gedit (and others installed in the image). \
         App name must contain only alphanumeric characters, hyphens, underscores, and dots. \
         Requires an active desktop session."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "description": "Application name (e.g. 'firefox', 'libreoffice', 'gedit'). \
                                    Must contain only alphanumeric characters, hyphens, underscores, \
                                    and dots.",
                    "pattern": "^[a-zA-Z0-9._-]+$"
                }
            },
            "required": ["app"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let app = require_str(&params, "app")?;

        let start = std::time::Instant::now();
        self.manager.open_app(app).await.map_err(desktop_err)?;

        Ok(ToolOutput::success(
            serde_json::json!({
                "launched": true,
                "app": app,
                "message": format!(
                    "'{app}' launched in the virtual display. \
                     Use desktop_screenshot to see the result."
                )
            }),
            start.elapsed(),
        ))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::UnlessAutoApproved
    }

    fn risk_level_for(&self, _params: &serde_json::Value) -> RiskLevel {
        RiskLevel::Medium
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        Some(ToolRateLimitConfig::new(20, 100))
    }
}

// ── DesktopAccessibilityTreeTool ──────────────────────────────────────────────

/// Query the AT-SPI2 accessibility tree for the current virtual display state.
///
/// Returns structured JSON describing the UI state of running applications.
/// This is the safe interface for the AI to observe desktop app state —
/// it never gets raw X11 socket access.
pub struct DesktopAccessibilityTreeTool {
    manager: Arc<DesktopSandboxManager>,
}

impl DesktopAccessibilityTreeTool {
    pub fn new(manager: Arc<DesktopSandboxManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DesktopAccessibilityTreeTool {
    fn name(&self) -> &str {
        "desktop_accessibility_tree"
    }

    fn description(&self) -> &str {
        "Query the AT-SPI2 accessibility tree for the current virtual display state. \
         Returns structured JSON with button labels, text fields, and UI element hierarchy. \
         This is safer than raw pixel analysis — the AI gets structured UI state, \
         not raw X11 events. Password field values are automatically redacted. \
         Requires an active desktop session."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "description": "Filter by application name (case-insensitive substring match). \
                                    Omit to query all running applications."
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum tree depth to traverse (default: 10, max: 20).",
                    "minimum": 1,
                    "maximum": 20,
                    "default": 10
                }
            },
            "required": []
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let app_filter = params.get("app").and_then(|v| v.as_str());
        let max_depth = params
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .min(20) as u32;

        let start = std::time::Instant::now();
        let tree = self
            .manager
            .accessibility_tree(app_filter, max_depth)
            .await
            .map_err(desktop_err)?;

        Ok(ToolOutput::success(tree, start.elapsed()))
    }

    fn requires_approval(&self, _params: &serde_json::Value) -> ApprovalRequirement {
        ApprovalRequirement::Never
    }

    fn risk_level_for(&self, _params: &serde_json::Value) -> RiskLevel {
        RiskLevel::Low
    }

    fn domain(&self) -> ToolDomain {
        ToolDomain::Orchestrator
    }

    fn execution_timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn rate_limit_config(&self) -> Option<ToolRateLimitConfig> {
        Some(ToolRateLimitConfig::new(30, 300))
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Build all desktop tools sharing a single [`DesktopSandboxManager`].
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use ironclaw::sandbox::DesktopSandboxManager;
/// use ironclaw::tools::builtin::desktop::build_desktop_tools;
///
/// let manager = Arc::new(DesktopSandboxManager::with_defaults());
/// let tools = build_desktop_tools(manager);
/// // Register `tools` with the ToolRegistry.
/// ```
pub fn build_desktop_tools(
    manager: Arc<DesktopSandboxManager>,
) -> Vec<Box<dyn crate::tools::tool::Tool>> {
    // Share the credential zones between the manager and the credential zone tool.
    let zones = Arc::clone(&manager.credential_zones);

    vec![
        Box::new(DesktopSessionStartTool::new(Arc::clone(&manager))),
        Box::new(DesktopSessionStopTool::new(Arc::clone(&manager))),
        Box::new(DesktopScreenshotTool::new(Arc::clone(&manager))),
        Box::new(DesktopClickTool::new(Arc::clone(&manager))),
        Box::new(DesktopTypeTool::new(Arc::clone(&manager))),
        Box::new(DesktopKeyPressTool::new(Arc::clone(&manager))),
        Box::new(DesktopOpenAppTool::new(Arc::clone(&manager))),
        Box::new(DesktopAccessibilityTreeTool::new(Arc::clone(&manager))),
        // Credential zone management — shares the same zones as the manager.
        Box::new(DesktopCredentialZoneTool::new(zones)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager() -> Arc<DesktopSandboxManager> {
        Arc::new(DesktopSandboxManager::with_defaults())
    }

    #[test]
    fn test_tool_names() {
        let mgr = make_manager();
        assert_eq!(DesktopSessionStartTool::new(Arc::clone(&mgr)).name(), "desktop_session_start");
        assert_eq!(DesktopSessionStopTool::new(Arc::clone(&mgr)).name(), "desktop_session_stop");
        assert_eq!(DesktopScreenshotTool::new(Arc::clone(&mgr)).name(), "desktop_screenshot");
        assert_eq!(DesktopClickTool::new(Arc::clone(&mgr)).name(), "desktop_click");
        assert_eq!(DesktopTypeTool::new(Arc::clone(&mgr)).name(), "desktop_type");
        assert_eq!(DesktopKeyPressTool::new(Arc::clone(&mgr)).name(), "desktop_key_press");
        assert_eq!(DesktopOpenAppTool::new(Arc::clone(&mgr)).name(), "desktop_open_app");
        assert_eq!(
            DesktopAccessibilityTreeTool::new(Arc::clone(&mgr)).name(),
            "desktop_accessibility_tree"
        );
    }

    #[test]
    fn test_session_start_always_requires_approval() {
        let mgr = make_manager();
        let tool = DesktopSessionStartTool::new(mgr);
        assert_eq!(
            tool.requires_approval(&serde_json::json!({"consent": true})),
            ApprovalRequirement::Always,
            "desktop_session_start must always require approval (consent gate)"
        );
    }

    #[test]
    fn test_screenshot_never_requires_approval() {
        let mgr = make_manager();
        let tool = DesktopScreenshotTool::new(mgr);
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::Never
        );
    }

    #[test]
    fn test_accessibility_tree_never_requires_approval() {
        let mgr = make_manager();
        let tool = DesktopAccessibilityTreeTool::new(mgr);
        assert_eq!(
            tool.requires_approval(&serde_json::json!({})),
            ApprovalRequirement::Never
        );
    }

    #[test]
    fn test_all_tools_are_orchestrator_domain() {
        let mgr = make_manager();
        // All desktop tools run in the orchestrator (they call into the
        // DesktopSandboxManager which manages the container via Docker API).
        assert_eq!(DesktopSessionStartTool::new(Arc::clone(&mgr)).domain(), ToolDomain::Orchestrator);
        assert_eq!(DesktopScreenshotTool::new(Arc::clone(&mgr)).domain(), ToolDomain::Orchestrator);
        assert_eq!(DesktopClickTool::new(Arc::clone(&mgr)).domain(), ToolDomain::Orchestrator);
        assert_eq!(DesktopTypeTool::new(Arc::clone(&mgr)).domain(), ToolDomain::Orchestrator);
        assert_eq!(DesktopOpenAppTool::new(Arc::clone(&mgr)).domain(), ToolDomain::Orchestrator);
        assert_eq!(
            DesktopAccessibilityTreeTool::new(Arc::clone(&mgr)).domain(),
            ToolDomain::Orchestrator
        );
    }

    #[test]
    fn test_build_desktop_tools_returns_all_nine() {
        let mgr = make_manager();
        let tools = build_desktop_tools(mgr);
        assert_eq!(tools.len(), 9, "build_desktop_tools should return 9 tools");

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"desktop_session_start"));
        assert!(names.contains(&"desktop_session_stop"));
        assert!(names.contains(&"desktop_screenshot"));
        assert!(names.contains(&"desktop_click"));
        assert!(names.contains(&"desktop_type"));
        assert!(names.contains(&"desktop_key_press"));
        assert!(names.contains(&"desktop_open_app"));
        assert!(names.contains(&"desktop_accessibility_tree"));
        assert!(names.contains(&"desktop_credential_zone"));
    }

    #[test]
    fn test_click_schema_requires_x_and_y() {
        let mgr = make_manager();
        let tool = DesktopClickTool::new(mgr);
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_names.contains(&"x"));
        assert!(required_names.contains(&"y"));
    }

    #[test]
    fn test_session_start_schema_requires_consent() {
        let mgr = make_manager();
        let tool = DesktopSessionStartTool::new(mgr);
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(
            required_names.contains(&"consent"),
            "consent must be a required parameter for desktop_session_start"
        );
    }
}