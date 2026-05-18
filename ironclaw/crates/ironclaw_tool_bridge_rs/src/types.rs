/// Result of a tool bridge execution attempt.
///
/// Exactly one of the three states is active:
///
/// - `Ok(result)` — tool executed successfully inside the sandbox.
/// - `Fallback` — tool is NOT sandboxed (read-only tool); caller may execute directly.
/// - `Blocked { message }` — tool is sandboxed but could not be executed;
///   caller MUST NOT fall back to host execution.
#[derive(Debug, Clone)]
pub enum ToolBridgeResult {
    /// Successful sandbox output.
    Ok(String),
    /// Tool is not sandboxed — caller may execute directly (no security risk).
    Fallback,
    /// Sandboxed tool blocked — do NOT fall back to host execution.
    Blocked { message: String },
}

impl ToolBridgeResult {
    pub fn ok(result: String) -> Self {
        Self::Ok(result)
    }

    pub fn allow_fallback() -> Self {
        Self::Fallback
    }

    pub fn fail_closed(message: String) -> Self {
        Self::Blocked { message }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }

    pub fn is_fallback(&self) -> bool {
        matches!(self, Self::Fallback)
    }

    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }

    pub fn result(&self) -> Option<&str> {
        match self {
            Self::Ok(r) => Some(r),
            _ => None,
        }
    }

    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Blocked { message } => Some(message),
            _ => None,
        }
    }
}
