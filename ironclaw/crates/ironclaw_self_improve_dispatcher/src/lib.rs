/// IronClaw self-improvement dispatcher — Rust rewrite of
/// `hermes-agent/agent/improvement_dispatcher.py` and
/// `hermes-agent/agent/improvement_rollback.py`.
///
/// Exposed to Python via PyO3 as the `ironclaw_self_improve_dispatcher` extension module.
///
/// # Security properties vs the Python implementation
///
/// | Property | Python | Rust |
/// |----------|--------|------|
/// | Encryption | AES-256-GCM **or** base64 fallback | AES-256-GCM only (hard dep) |
/// | Key material | Python GC, not zeroed | `zeroize::Zeroizing` on drop |
/// | Snapshot format | `json.dumps` + pickle risk | `serde_json` typed |
/// | Fail-closed | Python import can fail | `.so` crash = hard fail |
/// | Thread safety | Python GIL | `Arc<Mutex<>>` explicit |

pub mod config;
pub mod crypto;
pub mod dispatcher;
pub mod llm_resolver;
pub mod orchestrator_client;
pub mod pyo3_bindings;
pub mod rollback;
pub mod snapshot;
pub mod types;

use pyo3::prelude::*;
use pyo3_bindings::{
    PyAgentInfo, PyDispatcherConfig, PyDispatchResult, PyRollbackManager,
    should_use_ironclaw_py, trigger_self_improvement_async_py, trigger_self_improvement_py,
};

/// PyO3 extension module entry point.
///
/// Exposes the following to Python:
/// - `trigger_self_improvement_py(agent, job_type, conversation_snapshot=None) -> DispatchResult`
/// - `trigger_self_improvement_async_py(agent, job_type, conversation_snapshot=None) -> None`
/// - `should_use_ironclaw_py() -> bool`
/// - `DispatcherConfig` class
/// - `AgentInfo` class
/// - `DispatchResult` class
/// - `RollbackManager` class
#[pymodule]
fn ironclaw_self_improve_dispatcher(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(trigger_self_improvement_py, m)?)?;
    m.add_function(wrap_pyfunction!(trigger_self_improvement_async_py, m)?)?;
    m.add_function(wrap_pyfunction!(should_use_ironclaw_py, m)?)?;
    m.add_class::<PyDispatcherConfig>()?;
    m.add_class::<PyAgentInfo>()?;
    m.add_class::<PyDispatchResult>()?;
    m.add_class::<PyRollbackManager>()?;

    // Job type constants — mirror the Python JOB_TYPE_* constants for backward compat.
    m.add("JOB_TYPE_MEMORY_REVIEW", "MEMORY_REVIEW")?;
    m.add("JOB_TYPE_SKILL_REVIEW", "SKILL_REVIEW")?;
    m.add("JOB_TYPE_CURATOR_RUN", "CURATOR_RUN")?;
    m.add("JOB_TYPE_SWE_TASK", "SWE_TASK")?;

    Ok(())
}
