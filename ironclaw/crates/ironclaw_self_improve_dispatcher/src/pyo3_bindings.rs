use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::config::DispatcherConfig;
use crate::dispatcher::{should_use_ironclaw, trigger_self_improvement};
use crate::rollback::RollbackManager;
use crate::types::{AgentInfo, DispatchResult, Message};

// ---------------------------------------------------------------------------
// PyO3 wrapper types
// ---------------------------------------------------------------------------

/// Python-visible wrapper around `DispatcherConfig`.
#[pyclass(name = "DispatcherConfig")]
pub struct PyDispatcherConfig {
    inner: DispatcherConfig,
}

#[pymethods]
impl PyDispatcherConfig {
    #[new]
    fn new() -> Self {
        Self { inner: DispatcherConfig::from_env() }
    }

    #[getter]
    fn orchestrator_url(&self) -> &str {
        &self.inner.orchestrator_url
    }

    #[getter]
    fn llm_client_mode(&self) -> &str {
        &self.inner.llm_client_mode
    }
}

/// Python-visible wrapper around `AgentInfo`.
#[pyclass(name = "AgentInfo")]
pub struct PyAgentInfo {
    inner: AgentInfo,
}

#[pymethods]
impl PyAgentInfo {
    #[new]
    #[pyo3(signature = (session_id, provider, model, base_url=None, recent_messages=None))]
    fn new(
        session_id: String,
        provider: String,
        model: String,
        base_url: Option<String>,
        recent_messages: Option<Vec<(String, String)>>,
    ) -> Self {
        let msgs = recent_messages
            .unwrap_or_default()
            .into_iter()
            .map(|(role, content)| Message { role, content })
            .collect();
        Self {
            inner: AgentInfo {
                session_id,
                provider,
                model,
                base_url,
                recent_messages: msgs,
            },
        }
    }
}

/// Python-visible wrapper around `DispatchResult`.
#[pyclass(name = "DispatchResult")]
pub struct PyDispatchResult {
    #[pyo3(get)]
    pub job_id: Option<String>,
    #[pyo3(get)]
    pub skipped: bool,
    #[pyo3(get)]
    pub error: Option<String>,
}

impl From<DispatchResult> for PyDispatchResult {
    fn from(r: DispatchResult) -> Self {
        Self { job_id: r.job_id, skipped: r.skipped, error: r.error }
    }
}

/// Python-visible wrapper around `RollbackManager`.
#[pyclass(name = "RollbackManager")]
pub struct PyRollbackManager {
    inner: RollbackManager,
}

#[pymethods]
impl PyRollbackManager {
    #[new]
    #[pyo3(signature = (job_id, skills_path=None))]
    fn new(job_id: String, skills_path: Option<String>) -> Self {
        Self { inner: RollbackManager::new(job_id, skills_path) }
    }

    fn snapshot_skill(
        &self,
        skill_name: &str,
        content_before: Option<String>,
        event_id: &str,
    ) {
        self.inner.snapshot_skill(skill_name, content_before, event_id);
    }

    fn commit(&self) -> bool {
        self.inner.commit()
    }

    fn rollback(&self, reason: &str) -> bool {
        self.inner.rollback(reason)
    }

    #[getter]
    fn snapshot_count(&self) -> usize {
        self.inner.snapshot_count()
    }

    #[getter]
    fn is_committed(&self) -> bool {
        self.inner.is_committed()
    }

    #[getter]
    fn is_rolled_back(&self) -> bool {
        self.inner.is_rolled_back()
    }
}

// ---------------------------------------------------------------------------
// Module-level functions
// ---------------------------------------------------------------------------

/// Extract `AgentInfo` from a Python agent object using safe attribute access.
fn extract_agent_info(py: Python<'_>, agent: &Bound<'_, PyAny>) -> PyResult<AgentInfo> {
    let session_id = agent
        .getattr("session_id")
        .ok()
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let provider = agent
        .getattr("provider")
        .ok()
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_else(|| "unknown".to_string());

    let model = agent
        .getattr("model")
        .ok()
        .and_then(|v| v.extract::<String>().ok())
        .unwrap_or_else(|| "unknown".to_string());

    let base_url = agent
        .getattr("base_url")
        .ok()
        .and_then(|v| v.extract::<Option<String>>().ok())
        .flatten();

    // Extract recent messages from agent.messages (last 10, role+content only).
    let recent_messages = extract_recent_messages(py, agent);

    Ok(AgentInfo { session_id, provider, model, base_url, recent_messages })
}

fn extract_recent_messages(py: Python<'_>, agent: &Bound<'_, PyAny>) -> Vec<Message> {
    let messages = match agent.getattr("messages") {
        Ok(m) => m,
        Err(_) => return vec![],
    };
    let list = match messages.downcast::<PyList>() {
        Ok(l) => l,
        Err(_) => return vec![],
    };
    let items: Vec<&Bound<'_, PyAny>> = list.iter().collect();
    items
        .iter()
        .rev()
        .take(10)
        .rev()
        .filter_map(|item| {
            let dict = item.downcast::<PyDict>().ok()?;
            let role = dict
                .get_item("role")
                .ok()
                .flatten()
                .and_then(|v| v.extract::<String>().ok())
                .unwrap_or_else(|| "unknown".to_string());
            let content = dict
                .get_item("content")
                .ok()
                .flatten()
                .and_then(|v| v.extract::<String>().ok())
                .unwrap_or_default();
            // Truncate to 2048 chars.
            let content = if content.len() > 2048 {
                content[..2048].to_string()
            } else {
                content
            };
            Some(Message { role, content })
        })
        .collect()
}

/// Get or create a tokio runtime for blocking calls from Python.
fn get_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime")
}

// ---------------------------------------------------------------------------
// Exported Python functions
// ---------------------------------------------------------------------------

/// Trigger a sandboxed self-improvement job (blocking).
///
/// Returns a `DispatchResult` with `job_id`, `skipped`, and `error` fields.
#[pyfunction]
#[pyo3(name = "trigger_self_improvement_py")]
pub fn trigger_self_improvement_py(
    py: Python<'_>,
    agent: &Bound<'_, PyAny>,
    job_type: &str,
    conversation_snapshot: Option<&Bound<'_, PyDict>>,
) -> PyResult<PyDispatchResult> {
    let config = DispatcherConfig::from_env();
    let agent_info = extract_agent_info(py, agent)?;

    // If a pre-built snapshot dict was passed, serialize it to JSON and use it
    // as the messages list (best-effort — we ignore it and use agent_info.recent_messages
    // since the Rust snapshot builder is typed).
    let _ = conversation_snapshot; // accepted for API compatibility; Rust uses typed snapshot

    let rt = get_runtime();
    let result = rt.block_on(trigger_self_improvement(&config, &agent_info, job_type, None));
    Ok(result.into())
}

/// Trigger a sandboxed self-improvement job in a background thread (non-blocking).
#[pyfunction]
#[pyo3(name = "trigger_self_improvement_async_py")]
pub fn trigger_self_improvement_async_py(
    py: Python<'_>,
    agent: &Bound<'_, PyAny>,
    job_type: String,
    _conversation_snapshot: Option<&Bound<'_, PyDict>>,
) -> PyResult<()> {
    let config = DispatcherConfig::from_env();
    let agent_info = extract_agent_info(py, agent)?;

    std::thread::spawn(move || {
        let rt = get_runtime();
        let result = rt.block_on(trigger_self_improvement(&config, &agent_info, &job_type, None));
        if let Some(err) = result.error {
            tracing::warn!(error = %err, "Self-improve: background trigger failed");
        }
    });

    Ok(())
}

/// Return `True` when IronClaw should handle self-improvement work (blocking probe).
#[pyfunction]
#[pyo3(name = "should_use_ironclaw_py")]
pub fn should_use_ironclaw_py() -> bool {
    let config = DispatcherConfig::from_env();
    let rt = get_runtime();
    rt.block_on(should_use_ironclaw(&config))
}
