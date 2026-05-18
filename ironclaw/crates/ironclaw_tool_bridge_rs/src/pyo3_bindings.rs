use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;

use crate::policy::is_sandboxed_tool;
use crate::registry::{close_all_sessions, close_session, get_or_create_session};
use crate::types::ToolBridgeResult;

// ---------------------------------------------------------------------------
// PyO3 wrapper types
// ---------------------------------------------------------------------------

/// Python-visible wrapper around `ToolBridgeResult`.
#[pyclass(name = "ToolBridgeResult")]
pub struct PyToolBridgeResult {
    #[pyo3(get)]
    pub result: Option<String>,
    #[pyo3(get)]
    pub fallback: bool,
    #[pyo3(get)]
    pub blocked: bool,
    #[pyo3(get)]
    pub error_message: String,
}

impl From<ToolBridgeResult> for PyToolBridgeResult {
    fn from(r: ToolBridgeResult) -> Self {
        match r {
            ToolBridgeResult::Ok(result) => Self {
                result: Some(result),
                fallback: false,
                blocked: false,
                error_message: String::new(),
            },
            ToolBridgeResult::Fallback => Self {
                result: None,
                fallback: true,
                blocked: false,
                error_message: String::new(),
            },
            ToolBridgeResult::Blocked { message } => Self {
                result: None,
                fallback: false,
                blocked: true,
                error_message: message,
            },
        }
    }
}

#[pymethods]
impl PyToolBridgeResult {
    fn __repr__(&self) -> String {
        if self.fallback {
            "ToolBridgeResult(fallback=True)".to_string()
        } else if self.blocked {
            format!("ToolBridgeResult(blocked=True, error_message={:?})", self.error_message)
        } else {
            format!("ToolBridgeResult(result={:?})", self.result)
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: get or create a tokio runtime
// ---------------------------------------------------------------------------

fn get_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime")
}

// ---------------------------------------------------------------------------
// Helper: convert Python dict to serde_json::Value
// ---------------------------------------------------------------------------

fn pydict_to_value(dict: &Bound<'_, PyDict>) -> Value {
    let mut map = serde_json::Map::new();
    for (k, v) in dict.iter() {
        let key = k.extract::<String>().unwrap_or_default();
        let val = pyany_to_value(&v);
        map.insert(key, val);
    }
    Value::Object(map)
}

fn pyany_to_value(obj: &Bound<'_, PyAny>) -> Value {
    if let Ok(s) = obj.extract::<String>() {
        return Value::String(s);
    }
    if let Ok(b) = obj.extract::<bool>() {
        return Value::Bool(b);
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = obj.extract::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    if let Ok(d) = obj.downcast::<PyDict>() {
        return pydict_to_value(d);
    }
    if let Ok(s) = obj.str() {
        return Value::String(s.to_string());
    }
    Value::Null
}

// ---------------------------------------------------------------------------
// Exported Python functions
// ---------------------------------------------------------------------------

/// Execute a tool via the IronClaw sandbox (fully fail-closed).
///
/// Returns a `ToolBridgeResult` with `result`, `fallback`, `blocked`, and
/// `error_message` fields.
#[pyfunction]
#[pyo3(name = "execute_tool_via_ironclaw_py")]
#[pyo3(signature = (agent, tool_name, tool_args, tool_call_id="", timeout=60.0))]
pub fn execute_tool_via_ironclaw_py(
    py: Python<'_>,
    agent: &Bound<'_, PyAny>,
    tool_name: &str,
    tool_args: &Bound<'_, PyDict>,
    tool_call_id: &str,
    timeout: f64,
) -> PyResult<PyToolBridgeResult> {
    if !is_sandboxed_tool(tool_name) {
        return Ok(PyToolBridgeResult::from(ToolBridgeResult::allow_fallback()));
    }

    let session_id = agent
        .getattr("session_id")
        .ok()
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let tool_args_value = pydict_to_value(tool_args);
    let tool_call_id = if tool_call_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        tool_call_id.to_string()
    };
    let timeout_secs = timeout.max(1.0) as u64;
    let tool_name = tool_name.to_string();

    let rt = get_runtime();
    let result = rt.block_on(async move {
        let session = get_or_create_session(&session_id);
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // We can't use catch_unwind with async, so we handle errors via Result.
        })) {
            _ => {}
        }
        session
            .execute_tool(&tool_name, tool_args_value, &tool_call_id, timeout_secs)
            .await
    });

    Ok(PyToolBridgeResult::from(result))
}

/// Return `True` when `tool_name` must be routed through IronClaw.
#[pyfunction]
#[pyo3(name = "should_sandbox_tool_py")]
pub fn should_sandbox_tool_py(tool_name: &str) -> bool {
    is_sandboxed_tool(tool_name)
}

/// Get or create a bridge session for `session_id`.
/// Returns the session_id (for use as a handle in subsequent calls).
#[pyfunction]
#[pyo3(name = "get_or_create_session_py")]
pub fn get_or_create_session_py(session_id: &str) -> String {
    let session = get_or_create_session(session_id);
    session.session_id.clone()
}

/// Close the bridge session for `session_id`.
#[pyfunction]
#[pyo3(name = "close_session_py")]
pub fn close_session_py(session_id: &str) {
    let rt = get_runtime();
    rt.block_on(close_session(session_id));
}

/// Close all active bridge sessions (called on agent shutdown).
#[pyfunction]
#[pyo3(name = "close_all_sessions_py")]
pub fn close_all_sessions_py() {
    let rt = get_runtime();
    rt.block_on(close_all_sessions());
}
