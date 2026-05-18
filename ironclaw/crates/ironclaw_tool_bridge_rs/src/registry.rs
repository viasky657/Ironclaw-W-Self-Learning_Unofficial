use std::sync::Arc;
use dashmap::DashMap;
use once_cell::sync::Lazy;

use crate::session::BridgeSession;

/// Global session registry — maps session_id → BridgeSession.
///
/// Uses `DashMap` for lock-free concurrent access.
/// Initialized as a global singleton via `once_cell::sync::Lazy`.
static SESSION_REGISTRY: Lazy<Arc<DashMap<String, Arc<BridgeSession>>>> =
    Lazy::new(|| Arc::new(DashMap::new()));

/// Get or create a bridge session for `session_id`.
pub fn get_or_create_session(session_id: &str) -> Arc<BridgeSession> {
    if let Some(session) = SESSION_REGISTRY.get(session_id) {
        return session.clone();
    }
    // Insert a new session. DashMap handles concurrent inserts safely.
    let session = Arc::new(BridgeSession::new(session_id.to_string()));
    SESSION_REGISTRY.insert(session_id.to_string(), session.clone());
    session
}

/// Close and remove the bridge session for `session_id`.
pub async fn close_session(session_id: &str) {
    if let Some((_, session)) = SESSION_REGISTRY.remove(session_id) {
        session.close().await;
    }
}

/// Close all active bridge sessions (called on agent shutdown).
pub async fn close_all_sessions() {
    let sessions: Vec<Arc<BridgeSession>> = SESSION_REGISTRY
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    SESSION_REGISTRY.clear();

    for session in sessions {
        if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // We can't await inside catch_unwind, so we spawn a task.
            let s = session.clone();
            tokio::spawn(async move { s.close().await });
        })) {
            tracing::debug!("Tool bridge: error closing session: {:?}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_or_create_returns_same_session() {
        let s1 = get_or_create_session("test-session-registry-1");
        let s2 = get_or_create_session("test-session-registry-1");
        // Both should point to the same session (same session_id).
        assert_eq!(s1.session_id, s2.session_id);
        assert!(Arc::ptr_eq(&s1, &s2));
    }

    #[test]
    fn different_session_ids_get_different_sessions() {
        let s1 = get_or_create_session("test-session-registry-2a");
        let s2 = get_or_create_session("test-session-registry-2b");
        assert_ne!(s1.session_id, s2.session_id);
        assert!(!Arc::ptr_eq(&s1, &s2));
    }
}
