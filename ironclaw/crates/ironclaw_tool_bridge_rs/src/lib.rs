/// IronClaw tool bridge — Rust rewrite of
/// `hermes-agent/agent/ironclaw_tool_bridge.py`.
///
/// Exposed to Python via PyO3 as the `ironclaw_tool_bridge_rs` extension module.
///
/// # Security properties vs the Python implementation
///
/// | Property | Python | Rust |
/// |----------|--------|------|
/// | Sandboxed tool set | Runtime `frozenset` (mutable before freeze) | Compile-time `phf::Set` |
/// | Fail-closed guarantee | Python import can fail silently | `.so` crash = hard fail |
/// | Session thread safety | Python GIL | `Arc<Mutex<SessionState>>` |
/// | Session registry | `dict` + `threading.Lock` | `DashMap` (lock-free) |

pub mod policy;
pub mod pyo3_bindings;
pub mod registry;
pub mod session;
pub mod types;

use pyo3::prelude::*;
use pyo3_bindings::{
    PyToolBridgeResult,
    close_all_sessions_py,
    close_session_py,
    execute_tool_via_ironclaw_py,
    get_or_create_session_py,
    should_sandbox_tool_py,
};

/// PyO3 extension module entry point.
///
/// Exposes the following to Python:
/// - `execute_tool_via_ironclaw_py(agent, tool_name, tool_args, tool_call_id="", timeout=60.0) -> ToolBridgeResult`
/// - `should_sandbox_tool_py(tool_name) -> bool`
/// - `get_or_create_session_py(session_id) -> str`
/// - `close_session_py(session_id) -> None`
/// - `close_all_sessions_py() -> None`
/// - `ToolBridgeResult` class
#[pymodule]
fn ironclaw_tool_bridge_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(execute_tool_via_ironclaw_py, m)?)?;
    m.add_function(wrap_pyfunction!(should_sandbox_tool_py, m)?)?;
    m.add_function(wrap_pyfunction!(get_or_create_session_py, m)?)?;
    m.add_function(wrap_pyfunction!(close_session_py, m)?)?;
    m.add_function(wrap_pyfunction!(close_all_sessions_py, m)?)?;
    m.add_class::<PyToolBridgeResult>()?;
    Ok(())
}
