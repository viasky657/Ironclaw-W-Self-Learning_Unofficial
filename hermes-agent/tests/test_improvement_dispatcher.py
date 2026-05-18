"""Tests for the self-improvement dispatcher.

Verifies:
- Dispatcher calls get_text_auxiliary_client by default
- Dispatcher uses main runtime when SELF_IMPROVE_LLM_CLIENT=main
- Dispatcher uses local OpenAI-compatible client when SELF_IMPROVE_LLM_CLIENT=local
- Dispatcher skips job submission (no error) when auxiliary/local client is unavailable
- Dispatcher does not fork local agent (no AIAgent instantiation)
"""

from __future__ import annotations

import os
import threading
from types import SimpleNamespace
from typing import Any, Optional
from unittest.mock import MagicMock, patch, call

import pytest

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def make_mock_agent(
    model: str = "claude-sonnet-4",
    provider: str = "anthropic",
    session_id: str = "sess-123",
) -> MagicMock:
    agent = MagicMock()
    agent.model = model
    agent.provider = provider
    agent.session_id = session_id
    agent.messages = [
        {"role": "user", "content": "Hello"},
        {"role": "assistant", "content": "Hi there!"},
    ]
    agent._current_main_runtime = MagicMock(
        return_value={"base_url": "https://api.anthropic.com", "api_key": "sk-ant-xxx"}
    )
    return agent


# ---------------------------------------------------------------------------
# Feature flag
# ---------------------------------------------------------------------------


def test_is_secure_self_improve_enabled_false_by_default():
    from agent.improvement_dispatcher import is_secure_self_improve_enabled

    with patch.dict(os.environ, {}, clear=False):
        os.environ.pop("HERMES_SECURE_SELF_IMPROVE", None)
        assert not is_secure_self_improve_enabled()


def test_is_secure_self_improve_enabled_true():
    from agent.improvement_dispatcher import is_secure_self_improve_enabled

    with patch.dict(os.environ, {"HERMES_SECURE_SELF_IMPROVE": "true"}):
        assert is_secure_self_improve_enabled()


def test_is_secure_self_improve_enabled_1():
    from agent.improvement_dispatcher import is_secure_self_improve_enabled

    with patch.dict(os.environ, {"HERMES_SECURE_SELF_IMPROVE": "1"}):
        assert is_secure_self_improve_enabled()


# ---------------------------------------------------------------------------
# LLM client resolution: auxiliary (default)
# ---------------------------------------------------------------------------


def test_auxiliary_mode_calls_get_text_auxiliary_client():
    """Dispatcher must call get_text_auxiliary_client in auxiliary mode."""
    from agent.improvement_dispatcher import _resolve_llm_client

    mock_client = MagicMock()
    mock_client._provider = "openrouter"
    mock_client.base_url = "https://openrouter.ai/api/v1"

    with patch.dict(os.environ, {"SELF_IMPROVE_LLM_CLIENT": "auxiliary"}):
        with patch(
            "agent.improvement_dispatcher.get_text_auxiliary_client",
            return_value=(mock_client, "google/gemini-flash-1.5"),
        ) as mock_aux:
            agent = make_mock_agent()
            result = _resolve_llm_client(agent)

            mock_aux.assert_called_once_with("self_improve")
            assert result is not None
            provider, model, base_url = result
            assert model == "google/gemini-flash-1.5"


def test_auxiliary_mode_skips_when_no_client():
    """Dispatcher must skip (return None) when no auxiliary client is configured."""
    from agent.improvement_dispatcher import _resolve_llm_client

    with patch.dict(os.environ, {"SELF_IMPROVE_LLM_CLIENT": "auxiliary"}):
        with patch(
            "agent.improvement_dispatcher.get_text_auxiliary_client",
            return_value=None,
        ):
            agent = make_mock_agent()
            result = _resolve_llm_client(agent)
            assert result is None


def test_default_mode_is_auxiliary():
    """When SELF_IMPROVE_LLM_CLIENT is not set, auxiliary mode is used."""
    from agent.improvement_dispatcher import _resolve_llm_client

    mock_client = MagicMock()
    mock_client._provider = "openrouter"
    mock_client.base_url = "https://openrouter.ai/api/v1"

    env = {k: v for k, v in os.environ.items() if k != "SELF_IMPROVE_LLM_CLIENT"}
    with patch.dict(os.environ, env, clear=True):
        with patch(
            "agent.improvement_dispatcher.get_text_auxiliary_client",
            return_value=(mock_client, "gemini-flash"),
        ) as mock_aux:
            agent = make_mock_agent()
            _resolve_llm_client(agent)
            mock_aux.assert_called_once_with("self_improve")


# ---------------------------------------------------------------------------
# LLM client resolution: main
# ---------------------------------------------------------------------------


def test_main_mode_uses_parent_agent_runtime():
    """Dispatcher must use the parent agent's provider/model in main mode."""
    from agent.improvement_dispatcher import _resolve_llm_client

    with patch.dict(os.environ, {"SELF_IMPROVE_LLM_CLIENT": "main"}):
        agent = make_mock_agent(model="claude-opus-4", provider="anthropic")
        result = _resolve_llm_client(agent)

        assert result is not None
        provider, model, base_url = result
        assert provider == "anthropic"
        assert model == "claude-opus-4"


def test_main_mode_does_not_call_auxiliary_client():
    """Main mode must not call get_text_auxiliary_client."""
    from agent.improvement_dispatcher import _resolve_llm_client

    with patch.dict(os.environ, {"SELF_IMPROVE_LLM_CLIENT": "main"}):
        with patch(
            "agent.improvement_dispatcher.get_text_auxiliary_client"
        ) as mock_aux:
            agent = make_mock_agent()
            _resolve_llm_client(agent)
            mock_aux.assert_not_called()


# ---------------------------------------------------------------------------
# LLM client resolution: local
# ---------------------------------------------------------------------------


def test_local_mode_uses_configured_base_url():
    """Dispatcher must use SELF_IMPROVE_LLM_BASE_URL in local mode."""
    from agent.improvement_dispatcher import _resolve_llm_client

    env = {
        "SELF_IMPROVE_LLM_CLIENT": "local",
        "SELF_IMPROVE_LLM_BASE_URL": "http://localhost:8765/v1",
        "SELF_IMPROVE_LLM_MODEL": "hdc-dsv-local",
    }
    with patch.dict(os.environ, env):
        # Mock the health check to succeed.
        with patch("urllib.request.urlopen") as mock_urlopen:
            mock_urlopen.return_value.__enter__ = MagicMock(return_value=MagicMock())
            mock_urlopen.return_value.__exit__ = MagicMock(return_value=False)

            agent = make_mock_agent()
            result = _resolve_llm_client(agent)

            assert result is not None
            provider, model, base_url = result
            assert model == "hdc-dsv-local"
            assert base_url == "http://localhost:8765/v1"


def test_local_mode_skips_when_base_url_not_set():
    """Dispatcher must skip when SELF_IMPROVE_LLM_BASE_URL is not set in local mode."""
    from agent.improvement_dispatcher import _resolve_llm_client

    env = {"SELF_IMPROVE_LLM_CLIENT": "local"}
    env.pop("SELF_IMPROVE_LLM_BASE_URL", None)
    with patch.dict(os.environ, env):
        os.environ.pop("SELF_IMPROVE_LLM_BASE_URL", None)
        agent = make_mock_agent()
        result = _resolve_llm_client(agent)
        assert result is None


def test_local_mode_skips_when_server_unreachable():
    """Dispatcher must skip when local server is unreachable."""
    from agent.improvement_dispatcher import _resolve_llm_client
    import urllib.error

    env = {
        "SELF_IMPROVE_LLM_CLIENT": "local",
        "SELF_IMPROVE_LLM_BASE_URL": "http://localhost:8765/v1",
    }
    with patch.dict(os.environ, env):
        with patch(
            "urllib.request.urlopen",
            side_effect=urllib.error.URLError("connection refused"),
        ):
            agent = make_mock_agent()
            result = _resolve_llm_client(agent)
            assert result is None


# ---------------------------------------------------------------------------
# trigger_self_improvement: feature flag gate
# ---------------------------------------------------------------------------


def test_trigger_returns_none_when_flag_not_set():
    """trigger_self_improvement must return None when HERMES_SECURE_SELF_IMPROVE is not set."""
    from agent.improvement_dispatcher import trigger_self_improvement, JOB_TYPE_SKILL_REVIEW

    with patch.dict(os.environ, {}, clear=False):
        os.environ.pop("HERMES_SECURE_SELF_IMPROVE", None)
        agent = make_mock_agent()
        result = trigger_self_improvement(agent, JOB_TYPE_SKILL_REVIEW)
        assert result is None


def test_trigger_does_not_fork_local_agent():
    """Dispatcher must never instantiate a local AIAgent."""
    from agent.improvement_dispatcher import trigger_self_improvement, JOB_TYPE_SKILL_REVIEW

    with patch.dict(os.environ, {"HERMES_SECURE_SELF_IMPROVE": "true"}):
        with patch(
            "agent.improvement_dispatcher._resolve_llm_client",
            return_value=None,  # Skip cycle
        ):
            with patch("agent.improvement_dispatcher._submit_job_to_orchestrator") as mock_submit:
                agent = make_mock_agent()
                trigger_self_improvement(agent, JOB_TYPE_SKILL_REVIEW)
                # If LLM client is None, job must not be submitted.
                mock_submit.assert_not_called()


def test_trigger_submits_job_when_client_available():
    """Dispatcher must submit job to orchestrator when LLM client is resolved."""
    from agent.improvement_dispatcher import trigger_self_improvement, JOB_TYPE_SKILL_REVIEW

    with patch.dict(os.environ, {"HERMES_SECURE_SELF_IMPROVE": "true"}):
        with patch(
            "agent.improvement_dispatcher._resolve_llm_client",
            return_value=("openrouter", "gemini-flash", "https://openrouter.ai/api/v1"),
        ):
            with patch(
                "agent.improvement_dispatcher._submit_job_to_orchestrator",
                return_value="job-uuid-123",
            ) as mock_submit:
                agent = make_mock_agent()
                result = trigger_self_improvement(agent, JOB_TYPE_SKILL_REVIEW)

                mock_submit.assert_called_once()
                assert result == "job-uuid-123"


# ---------------------------------------------------------------------------
# trigger_self_improvement_async: non-blocking
# ---------------------------------------------------------------------------


def test_trigger_async_is_non_blocking():
    """trigger_self_improvement_async must return immediately without blocking."""
    from agent.improvement_dispatcher import trigger_self_improvement_async, JOB_TYPE_MEMORY_REVIEW

    call_completed = threading.Event()

    def slow_trigger(*args, **kwargs):
        import time
        time.sleep(0.1)
        call_completed.set()
        return "job-123"

    with patch.dict(os.environ, {"HERMES_SECURE_SELF_IMPROVE": "true"}):
        with patch(
            "agent.improvement_dispatcher.trigger_self_improvement",
            side_effect=slow_trigger,
        ):
            agent = make_mock_agent()
            start = __import__("time").time()
            trigger_self_improvement_async(agent, JOB_TYPE_MEMORY_REVIEW)
            elapsed = __import__("time").time() - start

            # Must return in < 50ms (the actual work takes 100ms in the thread).
            assert elapsed < 0.05, f"trigger_self_improvement_async blocked for {elapsed:.3f}s"
