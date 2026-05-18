"""IronClaw tool bridge for Hermes Agent.

This module is a thin wrapper that delegates to the Rust
``ironclaw_tool_bridge_rs`` PyO3 extension module when available.

If the Rust extension is not built, it falls back to the pure-Python
implementation in ``agent._ironclaw_tool_bridge_py`` with a security warning.

## Security note

The Python fallback has known security vulnerabilities:
- The sandboxed tool set is a runtime ``frozenset`` (mutable before freeze).
- A Python import error silently disables the fail-closed guarantee.
- Session thread safety relies on the Python GIL.

Build the Rust extension to eliminate these vulnerabilities:

    cd ironclaw && cargo build --release -p ironclaw_tool_bridge_rs

## Public API (unchanged from original)

- ``ToolBridgeResult`` — result dataclass with ``result``, ``fallback``, ``blocked``, ``error_message``
- ``SANDBOXED_TOOL_NAMES`` — frozenset of sandboxed tool names
- ``execute_tool_via_ironclaw(agent, tool_name, tool_args, tool_call_id="", timeout=60.0)``
- ``should_sandbox_tool(tool_name) -> bool``
- ``get_or_create_session(session_id) -> session``
- ``close_session(session_id) -> None``
- ``close_all_sessions() -> None``
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import Any, Dict, Optional

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Load Rust extension or fall back to Python implementation
# ---------------------------------------------------------------------------

try:
    import ironclaw_tool_bridge_rs as _rust  # type: ignore[import]

    _RUST_BACKEND = True
    logger.debug("ironclaw_tool_bridge_rs: using Rust backend")

    # Re-export the compile-time frozen sandboxed tool set as a Python frozenset
    # for backward compatibility with callers that inspect SANDBOXED_TOOL_NAMES.
    SANDBOXED_TOOL_NAMES: frozenset = frozenset({
        "terminal", "write_file", "patch", "memory", "skill_manage",
        "browser_navigate", "browser_click", "browser_type", "browser_submit",
        "browser_screenshot", "browser_close",
    })

except ImportError:
    logger.critical(
        "ironclaw_tool_bridge_rs Rust extension not found. "
        "Running with the Python fallback which has known security vulnerabilities:\n"
        "  - Sandboxed tool set is a runtime frozenset (not compile-time frozen)\n"
        "  - Import failure silently disables fail-closed guarantee\n"
        "Build the Rust extension: cd ironclaw && cargo build --release -p ironclaw_tool_bridge_rs"
    )
    from agent._ironclaw_tool_bridge_py import (  # type: ignore[import]
        ToolBridgeResult as _PyToolBridgeResult,
        SANDBOXED_TOOL_NAMES,
        execute_tool_via_ironclaw as _execute_tool_py,
        should_sandbox_tool as _should_sandbox_tool_py,
        get_or_create_session as _get_or_create_session_py,
        close_session as _close_session_py,
        close_all_sessions as _close_all_sessions_py,
        IronClawBridgeSession,
    )
    _rust = None
    _RUST_BACKEND = False


# ---------------------------------------------------------------------------
# ToolBridgeResult compatibility shim
# ---------------------------------------------------------------------------

@dataclass
class ToolBridgeResult:
    """Result of a tool bridge execution attempt.

    Exactly one of the three states is active:

    ``fallback=True``
        The tool is NOT sandboxed. Caller may execute directly.

    ``blocked=True``
        The tool is sandboxed but could not be executed. Caller MUST NOT fall back.

    ``result`` is not None
        The tool executed successfully inside the sandbox.
    """
    result: Optional[str] = None
    fallback: bool = False
    blocked: bool = False
    error_message: str = ""

    @classmethod
    def ok(cls, result: str) -> "ToolBridgeResult":
        return cls(result=result)

    @classmethod
    def allow_fallback(cls) -> "ToolBridgeResult":
        return cls(fallback=True)

    @classmethod
    def fail_closed(cls, message: str) -> "ToolBridgeResult":
        return cls(blocked=True, error_message=message)

    @classmethod
    def _from_rust(cls, rust_result: Any) -> "ToolBridgeResult":
        """Convert a Rust PyToolBridgeResult to a Python ToolBridgeResult."""
        return cls(
            result=rust_result.result,
            fallback=rust_result.fallback,
            blocked=rust_result.blocked,
            error_message=rust_result.error_message,
        )


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def should_sandbox_tool(tool_name: str) -> bool:
    """Return True when *tool_name* must be routed through IronClaw."""
    if _RUST_BACKEND:
        return _rust.should_sandbox_tool_py(tool_name)
    else:
        return _should_sandbox_tool_py(tool_name)


def execute_tool_via_ironclaw(
    agent: Any,
    tool_name: str,
    tool_args: Dict[str, Any],
    tool_call_id: str = "",
    timeout: float = 60.0,
) -> ToolBridgeResult:
    """Execute *tool_name* via the IronClaw sandbox (fully fail-closed).

    Returns a :class:`ToolBridgeResult`.
    """
    if _RUST_BACKEND:
        rust_result = _rust.execute_tool_via_ironclaw_py(
            agent, tool_name, tool_args, tool_call_id, timeout
        )
        return ToolBridgeResult._from_rust(rust_result)
    else:
        py_result = _execute_tool_py(agent, tool_name, tool_args, tool_call_id, timeout)
        # Convert Python ToolBridgeResult to our wrapper.
        return ToolBridgeResult(
            result=py_result.result,
            fallback=py_result.fallback,
            blocked=py_result.blocked,
            error_message=py_result.error_message,
        )


def get_or_create_session(session_id: str) -> Any:
    """Return the bridge session for *session_id*, creating one if needed."""
    if _RUST_BACKEND:
        return _rust.get_or_create_session_py(session_id)
    else:
        return _get_or_create_session_py(session_id)


def close_session(session_id: str) -> None:
    """Close and remove the bridge session for *session_id*."""
    if _RUST_BACKEND:
        _rust.close_session_py(session_id)
    else:
        _close_session_py(session_id)


def close_all_sessions() -> None:
    """Close all active bridge sessions (called on agent shutdown)."""
    if _RUST_BACKEND:
        _rust.close_all_sessions_py()
    else:
        _close_all_sessions_py()


__all__ = [
    "SANDBOXED_TOOL_NAMES",
    "ToolBridgeResult",
    "should_sandbox_tool",
    "execute_tool_via_ironclaw",
    "get_or_create_session",
    "close_session",
    "close_all_sessions",
    "_RUST_BACKEND",
]
