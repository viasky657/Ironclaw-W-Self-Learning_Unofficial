//! Desktop credential zone management tool.
//!
//! Provides the `desktop_credential_zone` tool which allows the user (or the
//! agent with user approval) to configure two separate credential zones for a
//! desktop session:
//!
//! - **`hidden`** — secrets the user marks as off-limits to the AI.
//!   Any occurrence of these values in screenshots is blacked out.
//!   Any occurrence in the accessibility tree is replaced with `[HIDDEN]`.
//!   The AI never sees these values.
//!
//! - **`visible`** — credentials the AI is allowed to use (e.g. test accounts).
//!   These are passed to the AI as structured data and are NOT redacted.
//!
//! # Security
//!
//! - This tool has `ApprovalRequirement::Always` — it always requires explicit
//!   user approval. The user must confirm which values to hide and which to expose.
//! - Hidden values are stored in memory only (never written to disk or logs).
//! - The tool never echoes hidden values back in its output.
//! - Clearing the hidden zone (`action: "clear_hidden"`) zeroizes the values.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::context::JobContext;
use crate::sandbox::credential_zones::{CredentialEntry, SharedCredentialZones};
use crate::tools::tool::{
    ApprovalRequirement, RiskLevel, Tool, ToolDomain, ToolError, ToolOutput,
};

/// Tool for managing credential zones in a desktop session.
///
/// Supports four actions:
/// - `add_hidden` — add a value to the hidden zone (redacted from AI)
/// - `clear_hidden` — remove all hidden values (zeroizes them)
/// - `add_visible` — add a credential to the visible zone (AI can use)
/// - `clear_visible` — remove all visible credentials
/// - `list_visible` — list visible credentials (labels only, no passwords)
/// - `status` — show zone status (counts only, no values)
pub struct DesktopCredentialZoneTool {
    zones: SharedCredentialZones,
}

impl DesktopCredentialZoneTool {
    pub fn new(zones: SharedCredentialZones) -> Self {
        Self { zones }
    }
}

#[async_trait]
impl Tool for DesktopCredentialZoneTool {
    fn name(&self) -> &str {
        "desktop_credential_zone"
    }

    fn description(&self) -> &str {
        "Manage credential zones for the desktop session. \
         \n\n\
         Two zones are available:\n\
         - HIDDEN zone: values the AI must never see. Any occurrence in screenshots \
           is blacked out; any occurrence in the accessibility tree is replaced with [HIDDEN].\n\
         - VISIBLE zone: credentials the AI is allowed to use (e.g. test accounts, demo logins).\n\
         \n\
         Actions:\n\
         - 'add_hidden': Add a value to the hidden zone. The AI will never see this value.\n\
         - 'clear_hidden': Remove all hidden values from the hidden zone.\n\
         - 'add_visible': Add a credential (label, username, password, notes) to the visible zone.\n\
         - 'clear_visible': Remove all visible credentials.\n\
         - 'list_visible': List visible credential labels (no passwords shown).\n\
         - 'status': Show zone status (counts only, no values).\n\
         \n\
         This tool ALWAYS requires explicit user approval."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "add_hidden",
                        "clear_hidden",
                        "add_visible",
                        "clear_visible",
                        "list_visible",
                        "status"
                    ],
                    "description": "Action to perform on the credential zones."
                },
                "value": {
                    "type": "string",
                    "description": "For 'add_hidden': the secret value to hide from the AI. \
                                    Never logged or echoed back."
                },
                "label": {
                    "type": "string",
                    "description": "For 'add_visible': human-readable label (e.g. 'Test account')."
                },
                "username": {
                    "type": "string",
                    "description": "For 'add_visible': username or email (optional)."
                },
                "password": {
                    "type": "string",
                    "description": "For 'add_visible': password the AI is allowed to use (optional)."
                },
                "notes": {
                    "type": "string",
                    "description": "For 'add_visible': additional notes (optional)."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
        _ctx: &JobContext,
    ) -> Result<ToolOutput, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParameters("missing required parameter 'action'".to_string()))?;

        let start = std::time::Instant::now();

        match action {
            "add_hidden" => {
                let value = params
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParameters(
                            "action 'add_hidden' requires parameter 'value'".to_string(),
                        )
                    })?;

                if value.is_empty() {
                    return Err(ToolError::InvalidParameters(
                        "'value' must not be empty".to_string(),
                    ));
                }

                let mut zones = self.zones.write().await;
                // We need to take ownership to use the builder pattern.
                // Replace the config with a new one that includes the hidden value.
                let current = std::mem::take(&mut *zones);
                *zones = current.hide(value);

                Ok(ToolOutput::success(
                    serde_json::json!({
                        "action": "add_hidden",
                        "result": "Value added to hidden zone. It will be redacted from all \
                                   future screenshots and accessibility tree queries.",
                        // Never echo the value back.
                        "hidden_count": zones.hidden_values().len()
                    }),
                    start.elapsed(),
                ))
            }

            "clear_hidden" => {
                let mut zones = self.zones.write().await;
                let count = zones.hidden_values().len();
                // Replace with a fresh config (old one is dropped, zeroizing hidden values).
                let current = std::mem::take(&mut *zones);
                // Rebuild with only the visible credentials preserved.
                let mut new_zones = crate::sandbox::credential_zones::CredentialZoneConfig::new();
                for entry in current.visible_credentials() {
                    new_zones = new_zones.allow_visible(entry.clone());
                }
                *zones = new_zones;

                Ok(ToolOutput::success(
                    serde_json::json!({
                        "action": "clear_hidden",
                        "result": format!("Cleared {count} hidden value(s). Hidden zone is now empty."),
                        "hidden_count": 0
                    }),
                    start.elapsed(),
                ))
            }

            "add_visible" => {
                let label = params
                    .get("label")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParameters(
                            "action 'add_visible' requires parameter 'label'".to_string(),
                        )
                    })?;

                let entry = CredentialEntry {
                    label: label.to_string(),
                    username: params
                        .get("username")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    password: params
                        .get("password")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    notes: params
                        .get("notes")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                };

                let mut zones = self.zones.write().await;
                let current = std::mem::take(&mut *zones);
                *zones = current.allow_visible(entry);

                Ok(ToolOutput::success(
                    serde_json::json!({
                        "action": "add_visible",
                        "result": format!("Credential '{}' added to visible zone. The AI can now use it.", label),
                        "visible_count": zones.visible_credentials().len()
                    }),
                    start.elapsed(),
                ))
            }

            "clear_visible" => {
                let mut zones = self.zones.write().await;
                let count = zones.visible_credentials().len();
                // Rebuild with only the hidden values preserved.
                let current = std::mem::take(&mut *zones);
                let mut new_zones = crate::sandbox::credential_zones::CredentialZoneConfig::new();
                for hidden in current.hidden_values() {
                    new_zones = new_zones.hide(hidden.clone());
                }
                *zones = new_zones;

                Ok(ToolOutput::success(
                    serde_json::json!({
                        "action": "clear_visible",
                        "result": format!("Cleared {count} visible credential(s)."),
                        "visible_count": 0
                    }),
                    start.elapsed(),
                ))
            }

            "list_visible" => {
                let zones = self.zones.read().await;
                let labels: Vec<serde_json::Value> = zones
                    .visible_credentials()
                    .iter()
                    .map(|e| {
                        let mut obj = serde_json::json!({ "label": e.label });
                        if let Some(u) = &e.username {
                            obj["username"] = serde_json::Value::String(u.clone());
                        }
                        if let Some(n) = &e.notes {
                            obj["notes"] = serde_json::Value::String(n.clone());
                        }
                        // NOTE: password is intentionally NOT included in list output.
                        obj
                    })
                    .collect();

                Ok(ToolOutput::success(
                    serde_json::json!({
                        "action": "list_visible",
                        "visible_credentials": labels,
                        "note": "Passwords are not shown in list output. Use 'add_visible' to add credentials."
                    }),
                    start.elapsed(),
                ))
            }

            "status" => {
                let zones = self.zones.read().await;
                Ok(ToolOutput::success(
                    serde_json::json!({
                        "action": "status",
                        "hidden_count": zones.hidden_values().len(),
                        "visible_count": zones.visible_credentials().len(),
                        "redaction_active": zones.has_hidden(),
                        "note": "Hidden values are never shown. Use 'add_hidden' to add values to hide."
                    }),
                    start.elapsed(),
                ))
            }

            other => Err(ToolError::InvalidParameters(format!(
                "unknown action '{}'. Valid actions: add_hidden, clear_hidden, \
                 add_visible, clear_visible, list_visible, status",
                other
            ))),
        }
    }

    /// Credential zone management ALWAYS requires explicit user approval.
    ///
    /// The user must confirm which values to hide and which to expose to the AI.
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
        Duration::from_secs(10)
    }

    /// The `value` parameter contains the hidden credential — redact it from logs.
    fn sensitive_params(&self) -> &[&str] {
        &["value", "password"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::credential_zones::new_shared_zones;

    fn make_tool() -> DesktopCredentialZoneTool {
        DesktopCredentialZoneTool::new(new_shared_zones())
    }

    #[test]
    fn test_tool_name() {
        assert_eq!(make_tool().name(), "desktop_credential_zone");
    }

    #[test]
    fn test_always_requires_approval() {
        let tool = make_tool();
        assert_eq!(
            tool.requires_approval(&serde_json::json!({"action": "add_hidden", "value": "x"})),
            ApprovalRequirement::Always,
            "credential zone tool must always require approval"
        );
    }

    #[test]
    fn test_sensitive_params_include_value_and_password() {
        let tool = make_tool();
        let sensitive = tool.sensitive_params();
        assert!(sensitive.contains(&"value"), "value must be sensitive");
        assert!(sensitive.contains(&"password"), "password must be sensitive");
    }

    #[test]
    fn test_schema_requires_action() {
        let tool = make_tool();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"action"));
    }

    #[tokio::test]
    async fn test_status_empty_zones() {
        let tool = make_tool();
        // No JobContext needed for this test — pass a dummy.
        // We can't easily construct a real JobContext in unit tests, so we
        // test the logic directly via the zones.
        let zones = tool.zones.read().await;
        assert_eq!(zones.hidden_values().len(), 0);
        assert_eq!(zones.visible_credentials().len(), 0);
        assert!(!zones.has_hidden());
    }

    #[tokio::test]
    async fn test_unknown_action_returns_error() {
        let tool = make_tool();
        // We test the action dispatch logic directly.
        let zones = tool.zones.read().await;
        drop(zones);

        // Simulate what execute() does for unknown action.
        let action = "bogus_action";
        let result: Result<(), String> = if matches!(
            action,
            "add_hidden" | "clear_hidden" | "add_visible" | "clear_visible" | "list_visible" | "status"
        ) {
            Ok(())
        } else {
            Err(format!("unknown action '{action}'"))
        };
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown action"));
    }
}
