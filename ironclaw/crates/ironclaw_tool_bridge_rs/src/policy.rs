/// Compile-time frozen set of sandboxed tool names.
///
/// Using `phf::Set` means the set is baked into the binary at compile time —
/// no runtime string allocation, no mutation possible.
///
/// MCP tool calls (`mcp__*`) and browser tools (`browser_*`) are handled
/// by prefix matching in `is_sandboxed_tool()`.
pub static SANDBOXED_TOOL_NAMES: phf::Set<&'static str> = phf::phf_set! {
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
};

/// Return `true` if `tool_name` must be routed through the IronClaw sandbox.
///
/// Covers:
/// - Explicit mutating tools in `SANDBOXED_TOOL_NAMES` (compile-time frozen).
/// - `browser_*` tools (any prefix match — catches unlisted browser tools).
/// - MCP tool calls (`mcp__*` prefix — spawned as host processes without
///   this bridge, so sandboxing them prevents host-level MCP server access).
///
/// This function is pure and allocation-free — safe to call on every tool dispatch.
#[inline]
pub fn is_sandboxed_tool(tool_name: &str) -> bool {
    if SANDBOXED_TOOL_NAMES.contains(tool_name) {
        return true;
    }
    // browser_* tools not explicitly listed above.
    if tool_name.starts_with("browser_") {
        return true;
    }
    // MCP tool calls: Hermes uses the "mcp__<server>__<tool>" naming convention.
    if tool_name.starts_with("mcp__") {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_sandboxed_tools() {
        for tool in &[
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
        ] {
            assert!(is_sandboxed_tool(tool), "{} should be sandboxed", tool);
        }
    }

    #[test]
    fn browser_prefix_is_sandboxed() {
        assert!(is_sandboxed_tool("browser_scroll"));
        assert!(is_sandboxed_tool("browser_new_tab"));
        assert!(is_sandboxed_tool("browser_anything_else"));
    }

    #[test]
    fn mcp_prefix_is_sandboxed() {
        assert!(is_sandboxed_tool("mcp__github__create_issue"));
        assert!(is_sandboxed_tool("mcp__slack__send_message"));
        assert!(is_sandboxed_tool("mcp__anything"));
    }

    #[test]
    fn read_only_tools_not_sandboxed() {
        for tool in &["read_file", "list_dir", "grep", "search_files", "get_file_info"] {
            assert!(!is_sandboxed_tool(tool), "{} should NOT be sandboxed", tool);
        }
    }

    #[test]
    fn sandboxed_set_is_compile_time_frozen() {
        // Verify the set cannot be mutated at runtime — it's a phf::Set (static).
        // This test just confirms the type is correct.
        let _: &phf::Set<&str> = &SANDBOXED_TOOL_NAMES;
    }
}
