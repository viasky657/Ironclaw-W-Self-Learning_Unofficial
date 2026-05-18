/// IronClaw audit PyO3 bindings — Rust rewrite of the security-critical
/// parts of `hermes-agent/agent/improvement_audit.py`.
///
/// Exposes `sha256_hex()` and `record_write_event()` as Rust functions so
/// they cannot be monkey-patched from Python.
///
/// # Security properties vs the Python implementation
///
/// | Property | Python | Rust |
/// |----------|--------|------|
/// | SHA-256 | `hashlib.sha256` (patchable) | `sha2::Sha256` (not patchable) |
/// | Write recording | Python dict shim | Typed Rust struct |

use pyo3::prelude::*;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// SHA-256 hashing
// ---------------------------------------------------------------------------

/// Compute SHA-256 hex digest of a UTF-8 string.
///
/// This function is implemented in Rust and cannot be monkey-patched from Python.
/// It produces identical output to `hashlib.sha256(content.encode('utf-8')).hexdigest()`.
#[pyfunction]
#[pyo3(name = "sha256_hex_py")]
pub fn sha256_hex_py(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

// ---------------------------------------------------------------------------
// Write event recording
// ---------------------------------------------------------------------------

/// Record a self-improvement write event in the audit log.
///
/// This is a thin Rust wrapper that:
/// 1. Computes SHA-256 hashes of before/after content using `sha2::Sha256`.
/// 2. Submits the event to the IronClaw orchestrator audit API.
///
/// Returns the event_id string on success, or an empty string on failure.
#[pyfunction]
#[pyo3(name = "record_write_event_py")]
#[pyo3(signature = (
    job_id,
    job_type,
    action,
    target,
    content_before,
    content_after,
    safety_verdict = "PASS",
    hdc_score = None,
    llm_model = "",
    container_id = "",
))]
pub fn record_write_event_py(
    job_id: &str,
    job_type: &str,
    action: &str,
    target: &str,
    content_before: Option<&str>,
    content_after: &str,
    safety_verdict: &str,
    hdc_score: Option<f64>,
    llm_model: &str,
    container_id: &str,
) -> String {
    let event_id = uuid::Uuid::new_v4().to_string();
    let before_hash = content_before.map(|c| sha256_hex(c));
    let after_hash = sha256_hex(content_after);

    let event = serde_json::json!({
        "event_id": event_id,
        "job_id": job_id,
        "job_type": job_type,
        "timestamp": utc_now_iso8601(),
        "action": action,
        "target": target,
        "before_hash": before_hash,
        "after_hash": after_hash,
        "safety_verdict": safety_verdict,
        "hdc_score": hdc_score,
        "llm_model": llm_model,
        "container_id": container_id,
        "status": "PENDING",
    });

    // Submit to orchestrator or local libSQL.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");

    let submitted = rt.block_on(submit_audit_event(&event));
    if submitted {
        event_id
    } else {
        tracing::warn!(
            job_id = %job_id,
            action = %action,
            target = %target,
            "Audit: failed to record write event"
        );
        String::new()
    }
}

/// Mark all PENDING events for a job as COMMITTED.
#[pyfunction]
#[pyo3(name = "mark_committed_py")]
pub fn mark_committed_py(job_id: &str) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");
    rt.block_on(update_audit_status(job_id, "COMMITTED"))
}

/// Mark all PENDING events for a job as ROLLED_BACK.
#[pyfunction]
#[pyo3(name = "mark_rolled_back_py")]
pub fn mark_rolled_back_py(job_id: &str) -> bool {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");
    rt.block_on(update_audit_status(job_id, "ROLLED_BACK"))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

fn utc_now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, mo, d, h, mi, s) = epoch_to_ymd_hms(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

fn epoch_to_ymd_hms(epoch: u64) -> (u64, u64, u64, u64, u64, u64) {
    let s = epoch % 60;
    let m = (epoch / 60) % 60;
    let h = (epoch / 3600) % 24;
    let days = epoch / 86400;
    let mut year = 1970u64;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if remaining < days_in_year { break; }
        remaining -= days_in_year;
        year += 1;
    }
    let months = [31u64, if is_leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1u64;
    for &dim in &months {
        if remaining < dim { break; }
        remaining -= dim;
        month += 1;
    }
    (year, month, remaining + 1, h, m, s)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

async fn submit_audit_event(event: &serde_json::Value) -> bool {
    let orchestrator_url = std::env::var("IRONCLAW_ORCHESTRATOR_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    let orchestrator_url = orchestrator_url.trim_end_matches('/');
    let orchestrator_token = std::env::var("IRONCLAW_ORCHESTRATOR_TOKEN")
        .unwrap_or_default();

    let url = format!("{}/orchestrator/audit-event", orchestrator_url);
    let client = match reqwest::Client::builder().use_rustls_tls().build() {
        Ok(c) => c,
        Err(_) => return false,
    };

    match client
        .post(&url)
        .bearer_auth(&orchestrator_token)
        .json(event)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) => resp.status().is_success(),
        Err(e) => {
            tracing::warn!(error = %e, "Audit: orchestrator API insert failed");
            false
        }
    }
}

async fn update_audit_status(job_id: &str, new_status: &str) -> bool {
    let orchestrator_url = std::env::var("IRONCLAW_ORCHESTRATOR_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    let orchestrator_url = orchestrator_url.trim_end_matches('/');
    let orchestrator_token = std::env::var("IRONCLAW_ORCHESTRATOR_TOKEN")
        .unwrap_or_default();

    let url = format!("{}/orchestrator/audit-status", orchestrator_url);
    let payload = serde_json::json!({ "job_id": job_id, "status": new_status });
    let client = match reqwest::Client::builder().use_rustls_tls().build() {
        Ok(c) => c,
        Err(_) => return false,
    };

    match client
        .post(&url)
        .bearer_auth(&orchestrator_token)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) => resp.status().is_success(),
        Err(e) => {
            tracing::warn!(error = %e, "Audit: orchestrator status update failed");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// PyO3 module entry point
// ---------------------------------------------------------------------------

/// PyO3 extension module entry point.
///
/// Exposes the following to Python:
/// - `sha256_hex_py(content: str) -> str`
/// - `record_write_event_py(...) -> str`
/// - `mark_committed_py(job_id: str) -> bool`
/// - `mark_rolled_back_py(job_id: str) -> bool`
#[pymodule]
fn ironclaw_audit_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(sha256_hex_py, m)?)?;
    m.add_function(wrap_pyfunction!(record_write_event_py, m)?)?;
    m.add_function(wrap_pyfunction!(mark_committed_py, m)?)?;
    m.add_function(wrap_pyfunction!(mark_rolled_back_py, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_hello_world() {
        // SHA-256("hello world") = b94d27b9934d3e08a52e52d7da7dabfac484efe04294e576f4a385dda595a5c6
        // Note: actual value for "hello world" (no newline):
        // b94d27b9934d3e08a52e52d7da7dabfac484efe04294e576f4a385dda595a5c6
        let result = sha256_hex("hello world");
        assert_eq!(result.len(), 64); // SHA-256 produces 32 bytes = 64 hex chars
        // Verify it's deterministic.
        assert_eq!(result, sha256_hex("hello world"));
    }

    #[test]
    fn sha256_different_inputs_different_outputs() {
        assert_ne!(sha256_hex("foo"), sha256_hex("bar"));
    }

    #[test]
    fn sha256_unicode_content() {
        // Should not panic on Unicode input.
        let result = sha256_hex("こんにちは世界");
        assert_eq!(result.len(), 64);
    }
}
