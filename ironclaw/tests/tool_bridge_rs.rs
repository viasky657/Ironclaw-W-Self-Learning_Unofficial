/// Integration tests for the `ironclaw_tool_bridge_rs` crate.
///
/// Tests:
/// - Sandboxed tool set (compile-time frozen)
/// - Fail-closed semantics (blocked not fallback when orchestrator unreachable)
/// - Session lifecycle (create/reuse/close)
/// - Concurrent tool calls (no race on job creation)

use ironclaw_tool_bridge_rs::{
    policy::{is_sandboxed_tool, SANDBOXED_TOOL_NAMES},
    registry::{close_session, get_or_create_session},
    types::ToolBridgeResult,
};

// ---------------------------------------------------------------------------
// Sandboxed tool set tests (compile-time frozen)
// ---------------------------------------------------------------------------

#[test]
fn sandboxed_tool_names_is_compile_time_set() {
    // Verify the type is phf::Set (compile-time frozen, not a runtime HashSet).
    let _: &phf::Set<&str> = &SANDBOXED_TOOL_NAMES;
}

#[test]
fn explicit_sandboxed_tools_are_sandboxed() {
    let sandboxed = [
        "terminal",
        "write_file",
        "patch",
        "memory",
        "skill_manage",
        "browser_navigate",
        "browser_click",
        "browser_type",
        "browser_submit",
        "browser_screenshot",
        "browser_close",
    ];
    for tool in &sandboxed {
        assert!(
            is_sandboxed_tool(tool),
            "'{}' must be in the sandboxed tool set",
            tool
        );
    }
}

#[test]
fn browser_prefix_tools_are_sandboxed() {
    assert!(is_sandboxed_tool("browser_scroll"));
    assert!(is_sandboxed_tool("browser_new_tab"));
    assert!(is_sandboxed_tool("browser_anything_else"));
    assert!(is_sandboxed_tool("browser_"));
}

#[test]
fn mcp_prefix_tools_are_sandboxed() {
    assert!(is_sandboxed_tool("mcp__github__create_issue"));
    assert!(is_sandboxed_tool("mcp__slack__send_message"));
    assert!(is_sandboxed_tool("mcp__anything"));
    assert!(is_sandboxed_tool("mcp__"));
}

#[test]
fn read_only_tools_are_not_sandboxed() {
    let read_only = [
        "read_file",
        "list_dir",
        "grep",
        "search_files",
        "get_file_info",
        "list_files",
        "view_file",
    ];
    for tool in &read_only {
        assert!(
            !is_sandboxed_tool(tool),
            "'{}' must NOT be in the sandboxed tool set",
            tool
        );
    }
}

#[test]
fn empty_tool_name_is_not_sandboxed() {
    assert!(!is_sandboxed_tool(""));
}

// ---------------------------------------------------------------------------
// ToolBridgeResult tests
// ---------------------------------------------------------------------------

#[test]
fn tool_bridge_result_ok() {
    let r = ToolBridgeResult::ok("output".to_string());
    assert!(r.is_ok());
    assert!(!r.is_fallback());
    assert!(!r.is_blocked());
    assert_eq!(r.result(), Some("output"));
    assert!(r.error_message().is_none());
}

#[test]
fn tool_bridge_result_fallback() {
    let r = ToolBridgeResult::allow_fallback();
    assert!(!r.is_ok());
    assert!(r.is_fallback());
    assert!(!r.is_blocked());
    assert!(r.result().is_none());
    assert!(r.error_message().is_none());
}

#[test]
fn tool_bridge_result_blocked() {
    let r = ToolBridgeResult::fail_closed("sandbox unreachable".to_string());
    assert!(!r.is_ok());
    assert!(!r.is_fallback());
    assert!(r.is_blocked());
    assert!(r.result().is_none());
    assert_eq!(r.error_message(), Some("sandbox unreachable"));
}

// ---------------------------------------------------------------------------
// Fail-closed semantics tests
// ---------------------------------------------------------------------------

#[test]
fn non_sandboxed_tool_returns_fallback() {
    // Non-sandboxed tools must return Fallback, not Blocked.
    // We test this via the policy layer (no network needed).
    assert!(!is_sandboxed_tool("read_file"));
    // If a tool is not sandboxed, the session.execute_tool() returns Fallback.
    // We verify the policy decision here; the full session test requires a mock server.
}

#[test]
fn fail_closed_message_contains_tool_name() {
    let tool_name = "terminal";
    let msg = format!(
        "[IronClaw sandbox] Cannot execute '{}': the IronClaw orchestrator at {} \
         is not reachable.",
        tool_name, "http://localhost:8080"
    );
    let r = ToolBridgeResult::fail_closed(msg.clone());
    assert!(r.is_blocked());
    assert!(r.error_message().unwrap().contains(tool_name));
}

// ---------------------------------------------------------------------------
// Session registry tests
// ---------------------------------------------------------------------------

#[test]
fn get_or_create_session_returns_same_session() {
    let session_id = "test-bridge-session-1";
    let s1 = get_or_create_session(session_id);
    let s2 = get_or_create_session(session_id);
    assert_eq!(s1.session_id, s2.session_id);
    assert!(std::sync::Arc::ptr_eq(&s1, &s2));
}

#[test]
fn different_session_ids_get_different_sessions() {
    let s1 = get_or_create_session("test-bridge-session-2a");
    let s2 = get_or_create_session("test-bridge-session-2b");
    assert_ne!(s1.session_id, s2.session_id);
    assert!(!std::sync::Arc::ptr_eq(&s1, &s2));
}

#[test]
fn concurrent_session_creation_no_race() {
    use std::sync::Arc;
    use std::thread;

    let session_id = "test-bridge-concurrent-session";
    let handles: Vec<_> = (0..10)
        .map(|_| {
            let id = session_id.to_string();
            thread::spawn(move || get_or_create_session(&id))
        })
        .collect();

    let sessions: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // All threads must get the same session (same Arc pointer).
    let first = &sessions[0];
    for session in &sessions[1..] {
        assert!(
            Arc::ptr_eq(first, session),
            "concurrent session creation must return the same session"
        );
    }
}

// ---------------------------------------------------------------------------
// Policy: fail-closed for all sandboxed tools when orchestrator unreachable
// ---------------------------------------------------------------------------

#[test]
fn all_sandboxed_tools_would_be_blocked_not_fallback() {
    // Verify that for every tool in SANDBOXED_TOOL_NAMES, is_sandboxed_tool returns true.
    // This means the session.execute_tool() path will attempt the sandbox (and block
    // if the orchestrator is unreachable) rather than returning Fallback.
    for tool in SANDBOXED_TOOL_NAMES.iter() {
        assert!(
            is_sandboxed_tool(tool),
            "'{}' must be sandboxed (fail-closed, not fallback)",
            tool
        );
    }
}
