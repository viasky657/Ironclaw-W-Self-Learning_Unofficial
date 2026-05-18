"""IronClaw tool bridge for Hermes Agent.

Routes mutating tool calls (``terminal``, ``write_file``, ``patch``,
``memory``, ``skill_manage``, ``browser_*``) through the IronClaw
orchestrator's sandbox instead of executing them directly in the Hermes
process.

## Why this matters

When Hermes runs **standalone** (not as an IronClaw worker), mutating tool
calls execute in the host process with no container isolation.  A prompt
injection attack that bypasses ``tool_guardrails.py`` can execute arbitrary
shell commands on the host.

This bridge intercepts those calls and routes them through
``POST /worker/{job_id}/tool`` on the IronClaw orchestrator, which executes
them inside a Docker container (or in-process WASM sandbox for local mode)
with:

- Network allowlisting (no exfiltration)
- Filesystem isolation (``SandboxPolicy::WorkspaceWrite``)
- Credential injection at the host boundary
- Full audit trail

## Activation

The bridge is active whenever the IronClaw orchestrator is reachable
(auto-detected via ``GET /health``).  Set
``HERMES_PREFER_LOCAL_SELF_IMPROVE=true`` to force direct execution even
when the orchestrator is reachable.

## Session lifecycle

A *bridge session* maps one Hermes agent session to one IronClaw worker job.
The session is created lazily on the first sandboxed tool call and reused for
the lifetime of the agent session.  The session is closed (job marked
complete) when the agent session ends or when the bridge is explicitly shut
down.

## Fully fail-closed semantics — no host execution fallback

**There is no fallback to direct host execution for sandboxed tools.**
Every sandboxed tool call either succeeds inside the IronClaw sandbox or is
blocked with a diagnostic error message.  This applies even when the
orchestrator is unreachable at session-creation time.

When the orchestrator is unreachable, the bridge returns
``ToolBridgeResult(blocked=True)`` with a human-readable diagnostic message
explaining that the IronClaw sandbox is required and how to start it.  The
model sees this message as the tool result and can inform the user.

The ``fallback`` field on ``ToolBridgeResult`` is only used for tools that
are **not** in the sandboxed set (read-only tools like ``read_file``,
``grep``, etc.) — those are never sandboxed and always execute directly.
Only mutating tools are subject to the fail-closed policy.
"""

from __future__ import annotations

import json
import logging
import os
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Dict, Optional
from uuid import uuid4

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Result type
# ---------------------------------------------------------------------------


@dataclass
class ToolBridgeResult:
    """Result of a tool bridge execution attempt.

    Exactly one of the three states is active:

    ``fallback=True``
        The tool is **not** in the sandboxed set (e.g. read-only tools like
        ``read_file``, ``grep``).  The caller **may** execute it directly —
        there is no security risk for non-mutating tools.

    ``blocked=True``
        The tool is sandboxed but could not be executed (sandbox failure,
        orchestrator unreachable, session closed, etc.).  The caller **must
        not** fall back to direct host execution — surface ``error_message``
        to the model as the tool result so the user can diagnose the issue.

    ``result`` is not None
        The tool executed successfully inside the sandbox.  Use ``result``
        as the tool output.
    """

    #: Successful sandbox output (mutually exclusive with fallback/blocked).
    result: Optional[str] = None
    #: True only for non-sandboxed tools — caller may execute directly.
    fallback: bool = False
    #: True when a sandboxed tool could not be executed — never fall back.
    blocked: bool = False
    #: Human-readable diagnostic when blocked=True.
    error_message: str = ""

    @classmethod
    def ok(cls, result: str) -> "ToolBridgeResult":
        return cls(result=result)

    @classmethod
    def allow_fallback(cls) -> "ToolBridgeResult":
        """Tool is not sandboxed — caller may execute directly (no risk)."""
        return cls(fallback=True)

    @classmethod
    def fail_closed(cls, message: str) -> "ToolBridgeResult":
        """Sandboxed tool blocked — do NOT fall back to host execution."""
        return cls(blocked=True, error_message=message)


# ---------------------------------------------------------------------------
# Tool sets
# ---------------------------------------------------------------------------

#: Mutating tools that should be routed through the IronClaw sandbox when
#: available.  Read-only tools (``read_file``, ``list_dir``, ``grep``, etc.)
#: are not included — they carry no write risk and routing them through the
#: sandbox would add unnecessary latency.
#:
#: MCP tool calls are identified at runtime by the ``mcp__`` prefix
#: (see :func:`_is_sandboxed_tool`).
SANDBOXED_TOOL_NAMES: frozenset[str] = frozenset(
    {
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
    }
)

# ---------------------------------------------------------------------------
# Configuration helpers
# ---------------------------------------------------------------------------


def _orchestrator_url() -> str:
    return os.environ.get("IRONCLAW_ORCHESTRATOR_URL", "http://localhost:8080").rstrip("/")


def _orchestrator_token() -> str:
    return os.environ.get("IRONCLAW_ORCHESTRATOR_TOKEN", "")


def _env_bool(name: str, default: bool = False) -> bool:
    val = os.environ.get(name, "").lower()
    if val in ("1", "true", "yes"):
        return True
    if val in ("0", "false", "no"):
        return False
    return default


def _env_float(name: str, default: float) -> float:
    try:
        return float(os.environ.get(name, str(default)))
    except (ValueError, TypeError):
        return default


# ---------------------------------------------------------------------------
# HTTP helpers
# ---------------------------------------------------------------------------


def _post_json(
    url: str,
    payload: Dict[str, Any],
    token: str,
    timeout: float = 30.0,
) -> Optional[Dict[str, Any]]:
    """POST *payload* as JSON to *url* with bearer *token*.

    Returns the parsed JSON response dict on success, or ``None`` on any
    network / HTTP error (errors are logged at WARNING level).
    """
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {token}",
            "Content-Length": str(len(body)),
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        body_text = exc.read().decode("utf-8", errors="replace")
        logger.warning(
            "IronClaw tool bridge: HTTP %s from %s: %s", exc.code, url, body_text
        )
        return None
    except Exception as exc:
        logger.warning("IronClaw tool bridge: request to %s failed: %s", url, exc)
        return None


# ---------------------------------------------------------------------------
# Bridge session
# ---------------------------------------------------------------------------


class IronClawBridgeSession:
    """A long-lived worker job on the IronClaw orchestrator that executes
    sandboxed tool calls on behalf of one Hermes agent session.

    The session is created lazily on the first tool call and reused for the
    lifetime of the agent session.  Thread-safe: a single lock guards job_id
    creation so concurrent tool calls don't race to create duplicate jobs.
    """

    def __init__(self, session_id: str) -> None:
        self.session_id = session_id
        self._job_id: Optional[str] = None
        self._job_token: Optional[str] = None
        self._lock = threading.Lock()
        self._closed = False

    # ------------------------------------------------------------------
    # Job lifecycle
    # ------------------------------------------------------------------

    def _create_job(self) -> bool:
        """Create a new worker job on the orchestrator.

        Returns True on success, False on failure.
        """
        url = f"{_orchestrator_url()}/jobs/tool-session"
        payload = {
            "session_id": self.session_id,
            "sandbox_policy": os.environ.get(
                "IRONCLAW_TOOL_SANDBOX_POLICY", "WorkspaceWrite"
            ),
            "max_wall_seconds": int(
                _env_float("IRONCLAW_TOOL_SESSION_MAX_SECS", 3600)
            ),
        }
        resp = _post_json(url, payload, _orchestrator_token(), timeout=10.0)
        if resp is None:
            return False
        job_id = resp.get("job_id")
        job_token = resp.get("job_token")
        if not job_id or not job_token:
            logger.warning(
                "IronClaw tool bridge: /jobs/tool-session response missing "
                "job_id or job_token: %s",
                resp,
            )
            return False
        self._job_id = job_id
        self._job_token = job_token
        logger.info(
            "IronClaw tool bridge: created tool session job %s for agent session %s",
            job_id,
            self.session_id,
        )
        return True

    def _ensure_job(self) -> bool:
        """Ensure a worker job exists, creating one if needed.  Thread-safe."""
        if self._job_id is not None:
            return True
        with self._lock:
            if self._job_id is not None:
                return True
            return self._create_job()

    def close(self) -> None:
        """Mark the worker job as complete and release resources."""
        if self._closed or self._job_id is None:
            return
        self._closed = True
        url = f"{_orchestrator_url()}/worker/{self._job_id}/complete"
        payload = {"status": "success", "result": "tool-session-closed"}
        _post_json(url, payload, self._job_token or "", timeout=5.0)
        logger.debug(
            "IronClaw tool bridge: closed tool session job %s", self._job_id
        )

    # ------------------------------------------------------------------
    # Tool execution
    # ------------------------------------------------------------------

    def execute_tool(
        self,
        tool_name: str,
        tool_args: Dict[str, Any],
        tool_call_id: str = "",
        timeout: float = 60.0,
    ) -> "ToolBridgeResult":
        """Execute *tool_name* with *tool_args* inside the sandbox.

        Returns a :class:`ToolBridgeResult`:

        - ``fallback=True`` — session could not be established (orchestrator
          unreachable); caller may fall back to direct execution.
        - ``blocked=True`` — session exists but execution failed; caller must
          NOT fall back — surface ``error_message`` to the model.
        - ``result`` is not None — success; use as tool output.
        """
        if self._closed:
            # Session was explicitly closed — fail-closed.
            return ToolBridgeResult.fail_closed(
                "[IronClaw sandbox] Tool session was closed — tool execution blocked. "
                "Restart the IronClaw orchestrator and retry."
            )
        if not _is_sandboxed_tool(tool_name):
            # Not a sandboxed tool — caller may execute directly (no risk).
            return ToolBridgeResult.allow_fallback()

        # Try to establish the session.  If this fails, the orchestrator is
        # unreachable — fail-closed: do NOT execute on the host.
        if not self._ensure_job():
            orchestrator_url = _orchestrator_url()
            return ToolBridgeResult.fail_closed(
                f"[IronClaw sandbox] Cannot execute '{tool_name}': the IronClaw "
                f"orchestrator at {orchestrator_url} is not reachable. "
                "Direct host execution is disabled for security. "
                "Start the IronClaw orchestrator and retry, or set "
                "HERMES_PREFER_LOCAL_SELF_IMPROVE=true to opt out of sandboxing."
            )

        # Session is established.  From this point on, all failures are
        # fail-closed: we do NOT fall back to direct host execution.
        url = f"{_orchestrator_url()}/worker/{self._job_id}/tool"
        payload = {
            "tool_name": tool_name,
            "parameters": tool_args,
            "tool_call_id": tool_call_id or str(uuid4()),
        }
        resp = _post_json(url, payload, self._job_token or "", timeout=timeout)
        if resp is None:
            # HTTP failure after session was established — fail-closed.
            msg = (
                f"[IronClaw sandbox] Tool '{tool_name}' could not be executed: "
                "sandbox communication failed. The tool was NOT run on the host."
            )
            logger.warning(
                "IronClaw tool bridge: sandbox call failed for %s (job=%s) — fail-closed",
                tool_name,
                self._job_id,
            )
            return ToolBridgeResult.fail_closed(msg)

        if not resp.get("success", False):
            error = resp.get("error") or "unknown sandbox error"
            msg = f"[IronClaw sandbox] Tool '{tool_name}' failed: {error}"
            logger.warning(
                "IronClaw tool bridge: sandbox returned error for %s (job=%s): %s",
                tool_name,
                self._job_id,
                error,
            )
            return ToolBridgeResult.fail_closed(msg)

        result = resp.get("result", "")
        logger.debug(
            "IronClaw tool bridge: %s completed via sandbox (job=%s)",
            tool_name,
            self._job_id,
        )
        return ToolBridgeResult.ok(result)


# ---------------------------------------------------------------------------
# Session registry
# ---------------------------------------------------------------------------

_sessions: Dict[str, IronClawBridgeSession] = {}
_sessions_lock = threading.Lock()


def get_or_create_session(session_id: str) -> IronClawBridgeSession:
    """Return the bridge session for *session_id*, creating one if needed."""
    with _sessions_lock:
        if session_id not in _sessions:
            _sessions[session_id] = IronClawBridgeSession(session_id)
        return _sessions[session_id]


def close_session(session_id: str) -> None:
    """Close and remove the bridge session for *session_id*."""
    with _sessions_lock:
        session = _sessions.pop(session_id, None)
    if session is not None:
        session.close()


def close_all_sessions() -> None:
    """Close all active bridge sessions (called on agent shutdown)."""
    with _sessions_lock:
        sessions = list(_sessions.values())
        _sessions.clear()
    for session in sessions:
        try:
            session.close()
        except Exception as exc:
            logger.debug("IronClaw tool bridge: error closing session: %s", exc)


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def _is_sandboxed_tool(tool_name: str) -> bool:
    """Return True if *tool_name* should be routed through the sandbox.

    Covers:
    - Explicit mutating tools in :data:`SANDBOXED_TOOL_NAMES`.
    - ``browser_*`` tools (any prefix match).
    - MCP tool calls (``mcp__*`` prefix — spawned as host processes without
      this bridge, so sandboxing them prevents host-level MCP server access).
    """
    if tool_name in SANDBOXED_TOOL_NAMES:
        return True
    # browser_* tools not explicitly listed above.
    if tool_name.startswith("browser_"):
        return True
    # MCP tool calls: Hermes uses the "mcp__<server>__<tool>" naming convention.
    if tool_name.startswith("mcp__"):
        return True
    return False


def should_sandbox_tool(tool_name: str) -> bool:
    """Return True when *tool_name* must be routed through IronClaw.

    Decision logic:

    1. Tool is not in the sandboxed set → execute directly (no risk, no overhead).
    2. Otherwise → True.  The tool MUST go through the sandbox.  There is no
       opt-out for sandboxed tools — if the orchestrator is unreachable the
       tool is blocked with a diagnostic error, not executed on the host.

    Note: this does NOT probe the orchestrator — the probe is done lazily
    when the session's first tool call is made.  This keeps the hot path
    (per-tool-call check) free of network I/O.
    """
    return _is_sandboxed_tool(tool_name)


def execute_tool_via_ironclaw(
    agent: Any,
    tool_name: str,
    tool_args: Dict[str, Any],
    tool_call_id: str = "",
    timeout: float = 60.0,
) -> "ToolBridgeResult":
    """Execute *tool_name* via the IronClaw sandbox (fully fail-closed).

    Returns a :class:`ToolBridgeResult`:

    - ``fallback=True`` — tool is **not** sandboxed (read-only tool).
      Caller may execute directly.
    - ``blocked=True`` — tool is sandboxed but could not be executed
      (orchestrator unreachable, sandbox failure, etc.).  Caller must NOT
      fall back to host execution — surface ``error_message`` to the model.
    - ``result`` is not None — success.

    Args:
        agent: The parent AIAgent instance (used to derive the session_id).
        tool_name: The tool to execute.
        tool_args: The tool arguments dict.
        tool_call_id: The tool call ID from the LLM response (for correlation).
        timeout: Per-call HTTP timeout in seconds.
    """
    if not should_sandbox_tool(tool_name):
        # Non-sandboxed tool — caller may execute directly.
        return ToolBridgeResult.allow_fallback()

    session_id = getattr(agent, "session_id", None) or str(uuid4())
    try:
        session = get_or_create_session(session_id)
        bridge_result = session.execute_tool(
            tool_name=tool_name,
            tool_args=tool_args,
            tool_call_id=tool_call_id,
            timeout=timeout,
        )
        # bridge_result is always fail-closed for sandboxed tools —
        # it never returns allow_fallback() for a sandboxed tool name.
        return bridge_result
    except Exception as exc:
        # Unexpected exception in the bridge itself.  Fail-closed.
        msg = (
            f"[IronClaw sandbox] Unexpected bridge error for '{tool_name}': {exc}. "
            "The tool was NOT run on the host. "
            "Check the IronClaw orchestrator logs for details."
        )
        logger.warning(
            "IronClaw tool bridge: unexpected error for %s (session=%s): %s — fail-closed",
            tool_name,
            session_id,
            exc,
        )
        return ToolBridgeResult.fail_closed(msg)


__all__ = [
    "SANDBOXED_TOOL_NAMES",
    "ToolBridgeResult",
    "should_sandbox_tool",
    "execute_tool_via_ironclaw",
    "get_or_create_session",
    "close_session",
    "close_all_sessions",
    "IronClawBridgeSession",
]
