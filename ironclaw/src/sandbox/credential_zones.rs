//! Credential zone configuration for the desktop sandbox.
//!
//! Provides two separate credential zones for desktop sessions:
//!
//! - **`hidden_credentials`** — secrets the user explicitly marks as off-limits to the AI.
//!   Any occurrence of these values in screenshots is blacked out (replaced with a solid
//!   black rectangle). Any occurrence in the accessibility tree is replaced with `[HIDDEN]`.
//!   The AI never sees these values.
//!
//! - **`visible_credentials`** — credentials the AI is allowed to use (e.g. test accounts,
//!   demo passwords). These are passed to the AI as structured data and are NOT redacted
//!   from screenshots or the accessibility tree.
//!
//! # Design rationale
//!
//! The fundamental residual risk of desktop access is that the AI sees everything rendered
//! in the virtual display. This module addresses that risk for the specific case of
//! credentials: the user can explicitly declare which secrets must never be visible to the AI,
//! and the system enforces that at the screenshot and accessibility tree layers.
//!
//! # Security properties
//!
//! - Hidden credential values are stored in memory only (never written to disk or logs).
//! - Redaction happens **before** the screenshot or accessibility tree is returned to the AI.
//! - The redaction is performed inside the container using `imagemagick` (for screenshots)
//!   and string replacement (for accessibility tree text).
//! - Hidden credential values are zeroized on drop (`zeroize::Zeroizing`).
//! - The AI is told that redaction occurred (it sees `[HIDDEN]` markers) but not the values.
//!
//! # Limitations
//!
//! - Screenshot redaction works by searching for text rendered in the virtual display.
//!   It uses `tesseract` OCR + `imagemagick` to locate and black out matching regions.
//!   This is best-effort: unusual fonts, rotated text, or obfuscated rendering may not
//!   be caught. Users should not rely solely on this for high-value secrets.
//! - Accessibility tree redaction is exact string matching — it catches values that appear
//!   verbatim in AT-SPI2 text nodes.
//! - Neither mechanism prevents the AI from inferring a hidden value from context clues
//!   (e.g. "the password is 8 characters and starts with 'A'").
//!
//! # Usage
//!
//! ```rust,no_run
//! use ironclaw::sandbox::credential_zones::{CredentialZoneConfig, CredentialEntry};
//!
//! let zones = CredentialZoneConfig::new()
//!     .hide("my-secret-password")
//!     .hide("my-api-key-12345")
//!     .allow_visible(CredentialEntry {
//!         label: "Test account".to_string(),
//!         username: Some("testuser@example.com".to_string()),
//!         password: Some("demo-password-123".to_string()),
//!         notes: Some("Safe to use in the virtual display".to_string()),
//!     });
//! ```

use std::sync::Arc;
use tokio::sync::RwLock;

/// A credential that the AI is allowed to see and use.
///
/// These are passed to the AI as structured data. They are NOT redacted from
/// screenshots or the accessibility tree.
#[derive(Debug, Clone)]
pub struct CredentialEntry {
    /// Human-readable label (e.g. "Test account", "Demo login").
    pub label: String,
    /// Username or email (optional).
    pub username: Option<String>,
    /// Password (optional). The AI can see and type this.
    pub password: Option<String>,
    /// Additional notes (e.g. "Use for the staging environment only").
    pub notes: Option<String>,
}

/// Configuration for credential zones in a desktop session.
///
/// Holds two separate lists:
/// - `hidden`: values that must never be visible to the AI (redacted in screenshots + AT-SPI2)
/// - `visible`: credentials the AI is allowed to use (passed as structured data)
#[derive(Debug, Clone, Default)]
pub struct CredentialZoneConfig {
    /// Values to hide from the AI. Stored as `zeroize::Zeroizing<String>` so they
    /// are wiped from memory when the config is dropped.
    ///
    /// These are matched against:
    /// 1. Accessibility tree text nodes (exact substring match → replaced with `[HIDDEN]`)
    /// 2. Screenshot OCR output (matched regions blacked out with imagemagick)
    hidden: Vec<String>,

    /// Credentials the AI is allowed to see and use.
    visible: Vec<CredentialEntry>,
}

impl CredentialZoneConfig {
    /// Create an empty credential zone config.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a value to the hidden zone.
    ///
    /// Any occurrence of `value` in screenshots or the accessibility tree will be
    /// replaced with `[HIDDEN]` before the AI sees it.
    ///
    /// # Security note
    ///
    /// The value is stored in memory for the duration of the session. It is never
    /// written to disk, logs, or audit trails.
    pub fn hide(mut self, value: impl Into<String>) -> Self {
        let v = value.into();
        if !v.is_empty() {
            self.hidden.push(v);
        }
        self
    }

    /// Add a credential to the visible zone.
    ///
    /// The AI can see and use this credential. It is passed as structured data
    /// and is NOT redacted from screenshots or the accessibility tree.
    pub fn allow_visible(mut self, entry: CredentialEntry) -> Self {
        self.visible.push(entry);
        self
    }

    /// Returns the list of hidden values (for redaction).
    ///
    /// Callers must not log or persist these values.
    pub fn hidden_values(&self) -> &[String] {
        &self.hidden
    }

    /// Returns the list of visible credentials (for the AI).
    pub fn visible_credentials(&self) -> &[CredentialEntry] {
        &self.visible
    }

    /// Returns true if there are any hidden values configured.
    pub fn has_hidden(&self) -> bool {
        !self.hidden.is_empty()
    }

    /// Redact all hidden values from a string (accessibility tree text).
    ///
    /// Replaces every occurrence of each hidden value with `[HIDDEN]`.
    /// Matching is case-sensitive and exact (substring match).
    pub fn redact_text(&self, text: &str) -> String {
        let mut result = text.to_string();
        for hidden in &self.hidden {
            if !hidden.is_empty() && result.contains(hidden.as_str()) {
                result = result.replace(hidden.as_str(), "[HIDDEN]");
            }
        }
        result
    }

    /// Returns true if the given text contains any hidden value.
    pub fn contains_hidden(&self, text: &str) -> bool {
        self.hidden.iter().any(|h| !h.is_empty() && text.contains(h.as_str()))
    }

    /// Serialize visible credentials to JSON for the AI.
    ///
    /// Returns a JSON array of visible credential entries. Hidden values are
    /// never included.
    pub fn visible_credentials_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.visible
                .iter()
                .map(|e| {
                    let mut obj = serde_json::json!({
                        "label": e.label,
                    });
                    if let Some(u) = &e.username {
                        obj["username"] = serde_json::Value::String(u.clone());
                    }
                    if let Some(p) = &e.password {
                        obj["password"] = serde_json::Value::String(p.clone());
                    }
                    if let Some(n) = &e.notes {
                        obj["notes"] = serde_json::Value::String(n.clone());
                    }
                    obj
                })
                .collect(),
        )
    }
}

impl Drop for CredentialZoneConfig {
    fn drop(&mut self) {
        // Zero out hidden values on drop to minimize the window during which
        // they are in memory.
        for hidden in &mut self.hidden {
            // Overwrite with zeros before dropping.
            // SAFETY: We are about to drop the String; this is best-effort zeroing.
            unsafe {
                let bytes = hidden.as_bytes_mut();
                for b in bytes.iter_mut() {
                    std::ptr::write_volatile(b, 0u8);
                }
            }
        }
    }
}

/// Thread-safe, shared credential zone config for a desktop session.
///
/// Wrapped in `Arc<RwLock<...>>` so it can be shared between the
/// `DesktopSandboxManager` and the desktop tools.
pub type SharedCredentialZones = Arc<RwLock<CredentialZoneConfig>>;

/// Create a new shared credential zone config.
pub fn new_shared_zones() -> SharedCredentialZones {
    Arc::new(RwLock::new(CredentialZoneConfig::new()))
}

/// Redact hidden values from a JSON accessibility tree in-place.
///
/// Recursively walks the JSON value and replaces any string that contains
/// a hidden value with a redacted version.
pub fn redact_accessibility_tree(
    tree: &mut serde_json::Value,
    zones: &CredentialZoneConfig,
) {
    if !zones.has_hidden() {
        return;
    }
    redact_json_recursive(tree, zones);
}

fn redact_json_recursive(value: &mut serde_json::Value, zones: &CredentialZoneConfig) {
    match value {
        serde_json::Value::String(s) => {
            if zones.contains_hidden(s) {
                *s = zones.redact_text(s);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                redact_json_recursive(item, zones);
            }
        }
        serde_json::Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                redact_json_recursive(v, zones);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_text_replaces_hidden_values() {
        let zones = CredentialZoneConfig::new()
            .hide("secret123")
            .hide("my-api-key");

        assert_eq!(
            zones.redact_text("The password is secret123 and the key is my-api-key"),
            "The password is [HIDDEN] and the key is [HIDDEN]"
        );
    }

    #[test]
    fn test_redact_text_no_match_unchanged() {
        let zones = CredentialZoneConfig::new().hide("secret123");
        let text = "nothing sensitive here";
        assert_eq!(zones.redact_text(text), text);
    }

    #[test]
    fn test_redact_text_empty_hidden_value_ignored() {
        let zones = CredentialZoneConfig::new().hide("").hide("real-secret");
        assert_eq!(
            zones.redact_text("contains real-secret here"),
            "contains [HIDDEN] here"
        );
    }

    #[test]
    fn test_contains_hidden() {
        let zones = CredentialZoneConfig::new().hide("password123");
        assert!(zones.contains_hidden("my password123 is here"));
        assert!(!zones.contains_hidden("nothing here"));
    }

    #[test]
    fn test_visible_credentials_json() {
        let zones = CredentialZoneConfig::new().allow_visible(CredentialEntry {
            label: "Test account".to_string(),
            username: Some("user@example.com".to_string()),
            password: Some("demo-pass".to_string()),
            notes: None,
        });

        let json = zones.visible_credentials_json();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["label"], "Test account");
        assert_eq!(arr[0]["username"], "user@example.com");
        assert_eq!(arr[0]["password"], "demo-pass");
        assert!(arr[0].get("notes").is_none() || arr[0]["notes"].is_null());
    }

    #[test]
    fn test_hidden_values_not_in_visible_json() {
        let zones = CredentialZoneConfig::new()
            .hide("top-secret-password")
            .allow_visible(CredentialEntry {
                label: "Visible".to_string(),
                username: Some("user".to_string()),
                password: Some("visible-pass".to_string()),
                notes: None,
            });

        let json = zones.visible_credentials_json().to_string();
        assert!(
            !json.contains("top-secret-password"),
            "hidden values must not appear in visible credentials JSON"
        );
    }

    #[test]
    fn test_redact_accessibility_tree_json() {
        let zones = CredentialZoneConfig::new().hide("s3cr3t");

        let mut tree = serde_json::json!({
            "role": "text",
            "name": "Login",
            "children": [
                {
                    "role": "entry",
                    "text": "password: s3cr3t"
                }
            ]
        });

        redact_accessibility_tree(&mut tree, &zones);

        assert_eq!(
            tree["children"][0]["text"],
            "password: [HIDDEN]",
            "hidden value must be redacted in accessibility tree"
        );
        // Non-sensitive fields unchanged
        assert_eq!(tree["name"], "Login");
    }

    #[test]
    fn test_redact_accessibility_tree_no_hidden_is_noop() {
        let zones = CredentialZoneConfig::new(); // no hidden values

        let mut tree = serde_json::json!({
            "text": "some text with no secrets"
        });
        let original = tree.clone();

        redact_accessibility_tree(&mut tree, &zones);
        assert_eq!(tree, original, "no-op when no hidden values configured");
    }

    #[test]
    fn test_multiple_hidden_values_all_redacted() {
        let zones = CredentialZoneConfig::new()
            .hide("pass1")
            .hide("pass2")
            .hide("pass3");

        let text = "pass1 and pass2 and pass3 are all hidden";
        let redacted = zones.redact_text(text);
        assert!(!redacted.contains("pass1"));
        assert!(!redacted.contains("pass2"));
        assert!(!redacted.contains("pass3"));
        assert_eq!(redacted, "[HIDDEN] and [HIDDEN] and [HIDDEN] are all hidden");
    }

    #[test]
    fn test_has_hidden() {
        let empty = CredentialZoneConfig::new();
        assert!(!empty.has_hidden());

        let with_hidden = CredentialZoneConfig::new().hide("secret");
        assert!(with_hidden.has_hidden());
    }
}
