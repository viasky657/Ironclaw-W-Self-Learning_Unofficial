# Hermes + IronClaw: Secure Self-Learning & Self-Improvement — Implementation README

> **Security Scope Clarification (read this first)**
>
> This implementation sandboxes **self-improvement work only** (background review, curator, SWE runner).
> **Regular agent tool calls** (`terminal`, `write_file`, `browser_*`, etc.) during a normal user conversation
> still execute through Hermes's existing `tool_executor.py` / `tool_guardrails.py` path — they are **not**
> routed through the IronClaw container sandbox by this implementation.
> See the [Security Scope](#security-scope-what-is-and-is-not-sandboxed) section for the full breakdown
> and the gap analysis with recommended next steps.

**Plan source:** [`plans/hermes-ironclaw-secure-self-improvement.md`](hermes-ironclaw-secure-self-improvement.md)
**Rust security hardening:** [`plans/rust-self-improvement-rewrite.md`](rust-self-improvement-rewrite.md)
**Status:** Implemented (all 8 phases + Rust security hardening)
**Date:** 2026-05-18

---

## Overview

This implementation routes all Hermes Agent self-improvement work (background review, curator, SWE runner) through IronClaw's sandbox stack. Instead of forking a local `AIAgent` in the same process, every self-modification job runs inside a secure, auditable container (or in-process WASM sandbox for local mode) with:

- **Capability-gated tool access** — only `skill_manage` + `memory` writes allowed
- **Network allowlisting** — no exfiltration; container cannot reach the internet
- **Credential injection at the host boundary** — API keys never enter the container
- **Full audit trail** — every write is recorded as an immutable event in PostgreSQL or libSQL
- **Atomic rollback** — any failure restores all writes to their before-state
- **HDC DSV quality gate** — optional local neuromorphic model scores writes before committing
- **LLM client selection** — auxiliary (default), main, or local HDC DSV server

---

## Architecture

```
Hermes Main Agent (conversation_loop.py)
    │
    │  post-turn trigger (HERMES_SECURE_SELF_IMPROVE=true)
    ▼
improvement_dispatcher.py
    │  resolves LLM client (auxiliary / main / local)
    │  encrypts conversation snapshot (AES-256-GCM)
    │  POST /jobs/self-improve
    ▼
IronClaw Orchestrator (src/orchestrator/)
    │  issues per-job bearer token (scoped to skill_manage + memory)
    │  starts sandbox container (Docker cloud / in-process WASM local)
    ▼
Sandbox Container (Dockerfile.self-improve)
    │  Hermes review fork (read-only wheel, no pip install)
    │  WASM Tool Bridge (ironclaw_hermes_bridge)
    │    ├── skill_manage → writes to /hermes-skills/ (content policy + rate limit)
    │    └── memory → POST /orchestrator/memory-write (proxied to host MemoryManager)
    │  HDC DSV Adapter (ironclaw_hdc_dsv) → scores writes before committing
    ▼
Audit Log (PostgreSQL or libSQL)
    │  immutable INSERT-only rows
    │  PENDING → COMMITTED | ROLLED_BACK
    ▼
Rollback Manager (improvement_rollback.py)
    │  restores before-state on failure
    └── marks audit events ROLLED_BACK
```

---

## File Map

### New Files

#### IronClaw — Orchestrator

| File | Purpose |
|------|---------|
| [`ironclaw/src/orchestrator/self_improvement_job.rs`](../ironclaw/src/orchestrator/self_improvement_job.rs) | `SelfImprovementJob` struct, `LlmClientMode` enum, `SelfImprovementJobType`, `EncryptedBlob`, `MemoryWriteRequest/Response`, `SelfImproveJobRequest/Response`. The canonical job descriptor for all self-improvement work. |

#### IronClaw — WASM Tool Bridge Crate

| File | Purpose |
|------|---------|
| [`ironclaw/crates/ironclaw_hermes_bridge/Cargo.toml`](../ironclaw/crates/ironclaw_hermes_bridge/Cargo.toml) | Crate manifest. Compiles to `cdylib` + `rlib`; WASM target `wasm32-wasip1`. |
| [`ironclaw/crates/ironclaw_hermes_bridge/src/lib.rs`](../ironclaw/crates/ironclaw_hermes_bridge/src/lib.rs) | Crate root — re-exports all public types. |
| [`ironclaw/crates/ironclaw_hermes_bridge/src/types.rs`](../ironclaw/crates/ironclaw_hermes_bridge/src/types.rs) | `BridgeConfig`, `ToolCall`, `ToolResult`, `WritePayload`, `BridgeError`. |
| [`ironclaw/crates/ironclaw_hermes_bridge/src/policy.rs`](../ironclaw/crates/ironclaw_hermes_bridge/src/policy.rs) | `ContentPolicy` — checks credential patterns, prompt injection, size limits. Returns `PolicyVerdict` (Pass / Flagged / Blocked). |
| [`ironclaw/crates/ironclaw_hermes_bridge/src/rate_limiter.rs`](../ironclaw/crates/ironclaw_hermes_bridge/src/rate_limiter.rs) | `RateLimiter` — atomic per-job counters for skill writes (max 10) and memory writes (max 5). |
| [`ironclaw/crates/ironclaw_hermes_bridge/src/skill_manage.rs`](../ironclaw/crates/ironclaw_hermes_bridge/src/skill_manage.rs) | `SkillManageTool` — enforces ownership check, path traversal prevention, content policy, rate limiting. Writes to `/hermes-skills/`. |
| [`ironclaw/crates/ironclaw_hermes_bridge/src/memory_proxy.rs`](../ironclaw/crates/ironclaw_hermes_bridge/src/memory_proxy.rs) | `MemoryProxyTool` — validates payload, then proxies to `POST /orchestrator/memory-write` via raw TCP (works in WASM/WASI and native). |

#### IronClaw — HDC DSV Adapter Crate

| File | Purpose |
|------|---------|
| [`ironclaw/crates/ironclaw_hdc_dsv/Cargo.toml`](../ironclaw/crates/ironclaw_hdc_dsv/Cargo.toml) | Crate manifest. |
| [`ironclaw/crates/ironclaw_hdc_dsv/src/lib.rs`](../ironclaw/crates/ironclaw_hdc_dsv/src/lib.rs) | Crate root. |
| [`ironclaw/crates/ironclaw_hdc_dsv/src/types.rs`](../ironclaw/crates/ironclaw_hdc_dsv/src/types.rs) | `HdcConfig`, `HdcVerdict` (Bootstrap / Pass / Flagged / Blocked / FailClosed / FailOpen), `WriteOutcome` (GoodWrite / BadWrite), `WritePayload`, `HdcError`. |
| [`ironclaw/crates/ironclaw_hdc_dsv/src/adapter.rs`](../ironclaw/crates/ironclaw_hdc_dsv/src/adapter.rs) | `HdcDsvAdapter` — calls `POST /v1/chat/completions` to score writes and `POST /v1/train` for online learning. Bootstrap mode: gate inactive until `bootstrap_min` examples. Fail-closed / fail-open configurable. |

#### IronClaw — Container Image

| File | Purpose |
|------|---------|
| [`ironclaw/Dockerfile.self-improve`](../ironclaw/Dockerfile.self-improve) | Multi-stage hardened image. Stage 1: Rust build (sandbox daemon + WASM bridge). Stage 2: Python wheel build. Stage 3: Final image — non-root UID 65534, read-only rootfs, tmpfs `/tmp`, `/hermes-skills` writable volume only, no memory volume (writes proxied). |

#### IronClaw — Database

| File | Purpose |
|------|---------|
| [`ironclaw/migrations/self_improvement_audit_postgres.sql`](../ironclaw/migrations/self_improvement_audit_postgres.sql) | PostgreSQL DDL for `self_improvement_audit` table. Includes immutability trigger (blocks UPDATE on committed rows) and DELETE rule. |
| [`ironclaw/migrations/self_improvement_audit_libsql.sql`](../ironclaw/migrations/self_improvement_audit_libsql.sql) | libSQL/SQLite-compatible DDL (UUID as TEXT, timestamps as TEXT). Immutability enforced at application layer via `INSERT OR IGNORE`. |
| [`ironclaw/src/db/libsql/self_improvement_audit.rs`](../ironclaw/src/db/libsql/self_improvement_audit.rs) | `LibSqlAuditRepository` implementing `SelfImprovementAuditRepository` trait. `INSERT OR IGNORE` semantics, WAL mode, status transitions only on PENDING rows. |

#### IronClaw — Profiles & Config

| File | Purpose |
|------|---------|
| [`ironclaw/profiles/local-self-improve.toml`](../ironclaw/profiles/local-self-improve.toml) | Full local-first profile: libSQL DB + in-process WASM + HDC DSV server. No Docker, no PostgreSQL, no cloud LLM for self-improvement. |

#### IronClaw — Tests

| File | Purpose |
|------|---------|
| [`ironclaw/tests/self_improvement_sandbox_integration.rs`](../ironclaw/tests/self_improvement_sandbox_integration.rs) | Tool allowlist enforcement (shell blocked, skill_manage allowed), memory write action validation, job defaults. |
| [`ironclaw/tests/self_improvement_rollback.rs`](../ironclaw/tests/self_improvement_rollback.rs) | Atomic rollback: PENDING→ROLLED_BACK, PENDING→COMMITTED, committed rows immutable, cross-job isolation, duplicate insert ignored. |
| [`ironclaw/tests/self_improvement_hdc_gate.rs`](../ironclaw/tests/self_improvement_hdc_gate.rs) | Bootstrap mode (gate inactive), verdict types (Pass/Flagged/Blocked/FailClosed/FailOpen), write outcome labels, config defaults. |
| [`ironclaw/tests/self_improvement_libsql_audit.rs`](../ironclaw/tests/self_improvement_libsql_audit.rs) | INSERT-only semantics, status transitions, HDC score storage, null before_hash, timestamp ordering, cross-job isolation. |

---

#### Hermes Agent — Python Files (Rust-backed thin wrappers)

> **Note:** These files were rewritten as thin wrappers in the Rust security hardening phase.
> Each file tries to import the corresponding Rust PyO3 extension module and falls back to the
> original Python implementation (renamed to `_*_py.py`) with a security warning if the Rust
> extension is not built.

| File | Purpose |
|------|---------|
| [`hermes-agent/agent/improvement_dispatcher.py`](../hermes-agent/agent/improvement_dispatcher.py) | **Thin wrapper** → delegates to [`ironclaw_self_improve_dispatcher`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/lib.rs) (Rust PyO3). Falls back to [`_improvement_dispatcher_py.py`](../hermes-agent/agent/_improvement_dispatcher_py.py). Public API unchanged: `trigger_self_improvement`, `trigger_self_improvement_async`, `should_use_ironclaw`, `JOB_TYPE_*` constants. |
| [`hermes-agent/agent/improvement_audit.py`](../hermes-agent/agent/improvement_audit.py) | **Thin wrapper** → delegates `sha256_hex()` and `record_write_event()` to [`ironclaw_audit_py`](../ironclaw/crates/ironclaw_audit_py/src/lib.rs) (Rust PyO3). `SelfImprovementEvent` dataclass kept as Python DTO. `AuditWriter` delegates DB operations to Rust. |
| [`hermes-agent/agent/improvement_rollback.py`](../hermes-agent/agent/improvement_rollback.py) | **Thin wrapper** → delegates `RollbackManager` to [`ironclaw_self_improve_dispatcher.PyRollbackManager`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/rollback.rs) (Rust, `zeroize::Zeroizing` on content_before). Falls back to [`_improvement_rollback_py.py`](../hermes-agent/agent/_improvement_rollback_py.py). |
| [`hermes-agent/agent/ironclaw_tool_bridge.py`](../hermes-agent/agent/ironclaw_tool_bridge.py) | **Thin wrapper** → delegates to [`ironclaw_tool_bridge_rs`](../ironclaw/crates/ironclaw_tool_bridge_rs/src/lib.rs) (Rust PyO3). Compile-time frozen `phf::Set` for sandboxed tool names. Falls back to [`_ironclaw_tool_bridge_py.py`](../hermes-agent/agent/_ironclaw_tool_bridge_py.py). |
| [`hermes-agent/hdc_dsv_server.py`](../hermes-agent/hdc_dsv_server.py) | **Deprecation shim** → execs `ironclaw-hdc-server` (Rust binary) if found on PATH; otherwise falls back to the Python FastAPI implementation with a deprecation warning. |

#### Hermes Agent — Python Fallback Files (kept during transition)

| File | Purpose |
|------|---------|
| [`hermes-agent/agent/_improvement_dispatcher_py.py`](../hermes-agent/agent/_improvement_dispatcher_py.py) | Original Python dispatcher (renamed). Used as fallback when Rust extension not built. |
| [`hermes-agent/agent/_improvement_rollback_py.py`](../hermes-agent/agent/_improvement_rollback_py.py) | Original Python rollback manager (renamed). Used as fallback. |
| [`hermes-agent/agent/_ironclaw_tool_bridge_py.py`](../hermes-agent/agent/_ironclaw_tool_bridge_py.py) | Original Python tool bridge (renamed). Used as fallback. |

#### Hermes Agent — Tests

| File | Purpose |
|------|---------|
| [`hermes-agent/tests/test_improvement_dispatcher.py`](../hermes-agent/tests/test_improvement_dispatcher.py) | Feature flag, auxiliary/main/local LLM resolution, skip-when-unavailable, no local agent fork, async non-blocking. Public API compatibility with Rust backend. |
| [`hermes-agent/tests/test_improvement_audit.py`](../hermes-agent/tests/test_improvement_audit.py) | `sha256_hex`, event dataclass, libSQL backend (insert/query/commit/rollback/immutability), `record_write_event` helper. |
| [`hermes-agent/tests/test_hdc_dsv_server.py`](../hermes-agent/tests/test_hdc_dsv_server.py) | `HdcDsvModel` scoring/training, persistence (save/load, 0600 permissions), FastAPI endpoints (`/v1/models`, `/v1/chat/completions`, `/v1/train`, `/health`), loopback-only binding. Python fallback path. |

---

### Rust Security Hardening — New Crates (Phase 2 of implementation)

> **See:** [`plans/rust-self-improvement-rewrite.md`](rust-self-improvement-rewrite.md) for the full threat model and design rationale.

#### `ironclaw_self_improve_dispatcher` — Rust rewrite of dispatcher + rollback

**Location:** [`ironclaw/crates/ironclaw_self_improve_dispatcher/`](../ironclaw/crates/ironclaw_self_improve_dispatcher/)

| File | Purpose |
|------|---------|
| [`Cargo.toml`](../ironclaw/crates/ironclaw_self_improve_dispatcher/Cargo.toml) | `crate-type = ["cdylib", "rlib"]`. Deps: `pyo3`, `aes-gcm`, `ring`, `reqwest`, `serde`, `tokio`, `zeroize`, `thiserror`, `tracing`, `uuid`, `base64`. |
| [`src/lib.rs`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/lib.rs) | Crate root. `#[pymodule]` entry point. Re-exports all public types. |
| [`src/types.rs`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/types.rs) | `JobType` enum, `LlmClientMode` enum, `ResolvedLlm`, `EncryptedSnapshot`, `DispatchResult`, `AgentInfo`, `Message`. |
| [`src/config.rs`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/config.rs) | `DispatcherConfig` — reads all env vars with typed defaults. No `getattr` on Python objects. |
| [`src/crypto.rs`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/crypto.rs) | `encrypt_snapshot(payload) -> Result<EncryptedSnapshot, CryptoError>`. AES-256-GCM via `aes-gcm` crate. `zeroize::Zeroizing` on key material. **No plaintext fallback.** |
| [`src/llm_resolver.rs`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/llm_resolver.rs) | `resolve_llm_client(config, agent_info) -> Result<ResolvedLlm, LlmError>`. Typed enum dispatch, no dynamic `getattr` at resolution time. |
| [`src/orchestrator_client.rs`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/orchestrator_client.rs) | `OrchestratorClient` — `reqwest::Client` with `rustls-tls-native-roots`. Methods: `health_check`, `submit_self_improve_job`, `submit_tool_session`, `execute_sandboxed_tool`, `complete_job`. |
| [`src/snapshot.rs`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/snapshot.rs) | `build_minimal_snapshot(agent_info, messages) -> serde_json::Value`. Typed struct, no arbitrary dict access. |
| [`src/rollback.rs`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/rollback.rs) | `RollbackManager` — `Arc<Mutex<Vec<SkillSnapshot>>>`. `zeroize::Zeroizing` on `content_before`. `snapshot_skill`, `commit`, `rollback`. |
| [`src/dispatcher.rs`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/dispatcher.rs) | `trigger_self_improvement`, `trigger_self_improvement_async`, `should_use_ironclaw`. |
| [`src/pyo3_bindings.rs`](../ironclaw/crates/ironclaw_self_improve_dispatcher/src/pyo3_bindings.rs) | `PyDispatcherConfig`, `PyAgentInfo`, `PyDispatchResult`, `PyRollbackManager`. `#[pyfunction]` exports. |

#### `ironclaw_tool_bridge_rs` — Rust rewrite of tool bridge

**Location:** [`ironclaw/crates/ironclaw_tool_bridge_rs/`](../ironclaw/crates/ironclaw_tool_bridge_rs/)

| File | Purpose |
|------|---------|
| [`Cargo.toml`](../ironclaw/crates/ironclaw_tool_bridge_rs/Cargo.toml) | `crate-type = ["cdylib", "rlib"]`. Deps: `pyo3`, `reqwest`, `serde`, `tokio`, `thiserror`, `tracing`, `uuid`, `dashmap`, `once_cell`, `phf`. |
| [`src/lib.rs`](../ironclaw/crates/ironclaw_tool_bridge_rs/src/lib.rs) | Crate root. `#[pymodule]` entry point. |
| [`src/types.rs`](../ironclaw/crates/ironclaw_tool_bridge_rs/src/types.rs) | `ToolBridgeResult` enum: `Ok(String)`, `Fallback`, `Blocked { message }`. |
| [`src/policy.rs`](../ironclaw/crates/ironclaw_tool_bridge_rs/src/policy.rs) | `SANDBOXED_TOOL_NAMES` — `phf::Set` (compile-time frozen). `is_sandboxed_tool(name) -> bool` — checks set + `browser_` prefix + `mcp__` prefix. |
| [`src/session.rs`](../ironclaw/crates/ironclaw_tool_bridge_rs/src/session.rs) | `BridgeSession` — `Arc<Mutex<SessionState>>`. `create_job`, `ensure_job`, `execute_tool`, `close`. Fail-closed on all errors after session established. |
| [`src/registry.rs`](../ironclaw/crates/ironclaw_tool_bridge_rs/src/registry.rs) | `get_or_create_session`, `close_session`, `close_all_sessions`. Global singleton via `once_cell::sync::Lazy<Arc<DashMap<...>>>`. |
| [`src/pyo3_bindings.rs`](../ironclaw/crates/ironclaw_tool_bridge_rs/src/pyo3_bindings.rs) | `PyToolBridgeResult`. `execute_tool_via_ironclaw_py`, `should_sandbox_tool_py`, `get_or_create_session_py`, `close_session_py`, `close_all_sessions_py`. |

#### `ironclaw_audit_py` — Rust PyO3 bindings for audit SHA-256 + write recording

**Location:** [`ironclaw/crates/ironclaw_audit_py/`](../ironclaw/crates/ironclaw_audit_py/)

| File | Purpose |
|------|---------|
| [`Cargo.toml`](../ironclaw/crates/ironclaw_audit_py/Cargo.toml) | `crate-type = ["cdylib"]`. Deps: `pyo3`, `sha2`, `hex`, `tokio`, `reqwest`, `serde_json`, `tracing`. |
| [`src/lib.rs`](../ironclaw/crates/ironclaw_audit_py/src/lib.rs) | `sha256_hex_py(content) -> str` — `sha2::Sha256`, not patchable from Python. `record_write_event_py(...)` — typed struct, submits to orchestrator API. `mark_committed_py`, `mark_rolled_back_py`. |

#### `ironclaw_hdc_server` — Rust Axum binary replacing `hdc_dsv_server.py`

**Location:** [`ironclaw/crates/ironclaw_hdc_server/`](../ironclaw/crates/ironclaw_hdc_server/)

| File | Purpose |
|------|---------|
| [`Cargo.toml`](../ironclaw/crates/ironclaw_hdc_server/Cargo.toml) | `[[bin]]` target `ironclaw-hdc-server`. Deps: `axum`, `tokio`, `serde`, `bincode`, `zeroize`, `sha2`, `subtle`, `tower-http`, `tracing`. |
| [`src/main.rs`](../ironclaw/crates/ironclaw_hdc_server/src/main.rs) | Axum router. Binds to `127.0.0.1:8765` only (hard-coded). Loads model from `IRONCLAW_HDC_MODEL_PATH`. Graceful shutdown on SIGTERM/SIGINT. |
| [`src/model.rs`](../ironclaw/crates/ironclaw_hdc_server/src/model.rs) | `HdcDsvModel` — bag-of-characters hypervector encoding. `score`, `train`, `save` (bincode + 0600 perms), `load` (bincode, no pickle). `SharedModel = Arc<RwLock<HdcDsvModel>>`. |
| [`src/auth.rs`](../ironclaw/crates/ironclaw_hdc_server/src/auth.rs) | `bearer_auth_middleware` — Tower middleware. Reads `IRONCLAW_HDC_SERVER_TOKEN`. Constant-time comparison via `subtle::ConstantTimeEq`. Public: `GET /v1/models`, `GET /health`. Protected: `POST /v1/chat/completions`, `POST /v1/train`. |
| [`src/handlers.rs`](../ironclaw/crates/ironclaw_hdc_server/src/handlers.rs) | `chat_completions`, `train`, `list_models`, `health`. All return typed `axum::Json<T>`. |
| [`src/types.rs`](../ironclaw/crates/ironclaw_hdc_server/src/types.rs) | `ChatCompletionRequest/Response`, `TrainRequest/Response`, `ModelsResponse`, `HealthResponse`, `WriteOutcome`. All `#[derive(Serialize, Deserialize)]`. |

#### Rust Integration Tests

| File | What It Tests |
|------|--------------|
| [`ironclaw/tests/self_improvement_dispatcher_rs.rs`](../ironclaw/tests/self_improvement_dispatcher_rs.rs) | AES-256-GCM encryption (no plaintext fallback), LLM client resolution (typed enum), snapshot serialization (serde_json), rollback manager (commit/rollback/idempotency/file restore), `DispatchResult` variants, config defaults. |
| [`ironclaw/tests/tool_bridge_rs.rs`](../ironclaw/tests/tool_bridge_rs.rs) | Sandboxed tool set (compile-time `phf::Set`), fail-closed semantics (blocked not fallback), `ToolBridgeResult` variants, session registry (create/reuse/concurrent). |
| [`ironclaw/tests/audit_py_bindings.rs`](../ironclaw/tests/audit_py_bindings.rs) | SHA-256 output matches NIST test vectors and Python `hashlib.sha256`, determinism, Unicode content, `mark_committed`/`mark_rolled_back` no-panic on network error. |
| [`ironclaw/tests/hdc_server_rs.rs`](../ironclaw/tests/hdc_server_rs.rs) | Model scoring/training, bincode save/load roundtrip, pickle data rejected, 0600 permissions (Unix), loopback-only binding (source code assertion), concurrent reads/writes thread safety. |

#### Migration Utility

| File | Purpose |
|------|---------|
| [`ironclaw/scripts/migrate_hdc_model.py`](../ironclaw/scripts/migrate_hdc_model.py) | One-time migration: reads old `hdc_model.bin` (Python pickle) and writes a JSON migration file. Run once on upgrade from Python to Rust HDC server. |

---

### Desktop Sandbox — New Files (Phase 3: Safe Desktop App Access)

> **See:** the desktop sandbox architecture section below for the full threat model and design rationale.

#### IronClaw — Desktop Sandbox Core

| File | Purpose |
|------|---------|
| [`ironclaw/src/sandbox/desktop.rs`](../ironclaw/src/sandbox/desktop.rs) | `DesktopSandboxManager` — manages a long-lived Docker container running Xvfb (virtual framebuffer). Methods: `start_session(consent)` (consent gate), `stop_session`, `screenshot` (base64 PNG of Xvfb framebuffer, with credential redaction), `click(x, y, button)` (xdotool mouse), `type_text(text)` (xdotool keyboard), `key_press(key)` (X11 keysym), `open_app(name)` (validated app launch), `accessibility_tree(app_filter, max_depth)` (AT-SPI2 → JSON, with credential redaction). `DesktopSandboxConfig` with defaults. `DesktopError` enum. `credential_zones: SharedCredentialZones` field (shared with tools). |
| [`ironclaw/src/sandbox/credential_zones.rs`](../ironclaw/src/sandbox/credential_zones.rs) | `CredentialZoneConfig` — two-zone credential management. **Hidden zone**: values stored in memory, zeroized on drop; `redact_text()` replaces occurrences with `[HIDDEN]`; `contains_hidden()` for fast checks. **Visible zone**: `CredentialEntry` structs (label, username, password, notes) serialized to JSON for the AI. `redact_accessibility_tree()` recursively redacts JSON trees. `SharedCredentialZones = Arc<RwLock<CredentialZoneConfig>>`. |
| [`ironclaw/src/sandbox/mod.rs`](../ironclaw/src/sandbox/mod.rs) | Added `pub mod desktop`, `pub mod credential_zones` and re-exports: `DesktopError`, `DesktopExecOutput`, `DesktopResult`, `DesktopSandboxConfig`, `DesktopSandboxManager`, `CredentialEntry`, `CredentialZoneConfig`, `SharedCredentialZones`, `new_shared_zones`, `redact_accessibility_tree`. |
| [`ironclaw/src/sandbox/config.rs`](../ironclaw/src/sandbox/config.rs) | Added `SandboxPolicy::DesktopAccess` variant. Updated `allows_writes()`, added `is_desktop_access()`, updated `writable_path()` (returns `/workspace`), updated `FromStr` (accepts `desktop_access`, `desktopaccess`, `desktop`). Updated error message to include `desktop_access`. |
| [`ironclaw/src/config/sandbox.rs`](../ironclaw/src/config/sandbox.rs) | Updated `to_sandbox_config()`: when `policy == DesktopAccess` and the configured image does not contain `"desktop"`, overrides image to `ironclaw-desktop:latest` with an info log. |

#### IronClaw — Desktop Tools

| File | Purpose |
|------|---------|
| [`ironclaw/src/tools/builtin/desktop.rs`](../ironclaw/src/tools/builtin/desktop.rs) | Nine desktop tools, all holding `Arc<DesktopSandboxManager>`: `DesktopSessionStartTool` (`ApprovalRequirement::Always` — consent gate), `DesktopSessionStopTool`, `DesktopScreenshotTool` (returns `image_base64` + `format`, hidden credentials blacked out), `DesktopClickTool` (x/y/button), `DesktopTypeTool` (max 4096 chars), `DesktopKeyPressTool` (X11 keysym), `DesktopOpenAppTool` (validated app name), `DesktopAccessibilityTreeTool` (AT-SPI2 JSON, hidden credentials replaced with `[HIDDEN]`), `DesktopCredentialZoneTool` (credential zone management). `build_desktop_tools(manager)` convenience builder — shares `credential_zones` between manager and tool. |
| [`ironclaw/src/tools/builtin/desktop_credential_zone.rs`](../ironclaw/src/tools/builtin/desktop_credential_zone.rs) | `DesktopCredentialZoneTool` — `ApprovalRequirement::Always`. Actions: `add_hidden` (add value to hidden zone, never echoed back), `clear_hidden` (zeroize all hidden values), `add_visible` (add credential AI can use), `clear_visible`, `list_visible` (labels only, no passwords), `status` (counts only). `sensitive_params: ["value", "password"]` — redacted from logs. |
| [`ironclaw/src/tools/builtin/mod.rs`](../ironclaw/src/tools/builtin/mod.rs) | Added `pub mod desktop`, `pub mod desktop_credential_zone` and re-exports for all nine desktop tool structs, `build_desktop_tools`, and `DesktopCredentialZoneTool`. |

#### IronClaw — Desktop Container Image

| File | Purpose |
|------|---------|
| [`ironclaw/docker/desktop-sandbox.Dockerfile`](../ironclaw/docker/desktop-sandbox.Dockerfile) | Ubuntu 24.04 image. Installs: `xvfb`, `fluxbox`, `x11vnc`, `at-spi2-core`, `xdotool`, `scrot`, `imagemagick`, `tesseract-ocr`, `tesseract-ocr-eng`, `python3-pyatspi`, `firefox`, `libreoffice`, `gedit`. Sets `DISPLAY=:99`. Non-root UID 1000 (`worker`). No host display socket mount. No host clipboard bridge. Copies `desktop-entrypoint.sh`, `desktop-accessibility-query.py`, `desktop-redact-screenshot.py`. |
| [`ironclaw/docker/desktop-entrypoint.sh`](../ironclaw/docker/desktop-entrypoint.sh) | Container entrypoint. Starts Xvfb `:99` (virtual framebuffer, no host DISPLAY connection), waits for display ready, starts D-Bus session, starts AT-SPI2 bus launcher, starts fluxbox. Keeps container alive for `docker exec` commands. Handles SIGTERM/SIGINT for clean shutdown. |
| [`ironclaw/docker/desktop-accessibility-query.py`](../ironclaw/docker/desktop-accessibility-query.py) | Python AT-SPI2 query script. Queries accessibility bus and outputs structured JSON (role, name, description, states, text, value, bounds). Password fields (`ROLE_PASSWORD_TEXT`) are automatically redacted to `[REDACTED]`. Max 500 nodes to prevent runaway output. CLI: `--app APP_NAME`, `--max-depth N`. |
| [`ironclaw/docker/desktop-redact-screenshot.py`](../ironclaw/docker/desktop-redact-screenshot.py) | Screenshot credential redaction script. Uses `tesseract` OCR to locate text regions matching hidden values, then uses `imagemagick convert` to black out those regions with solid rectangles. Hidden values are read from a temp file (never passed as CLI args). Best-effort: unusual fonts may not be caught. CLI: `INPUT.png HIDDEN_VALUES.txt OUTPUT.png`. |

---

### Modified Files

| File | Change |
|------|--------|
| [`ironclaw/src/orchestrator/mod.rs`](../ironclaw/src/orchestrator/mod.rs) | Added `pub mod self_improvement_job` and re-exports for all new types. |
| [`ironclaw/src/orchestrator/api.rs`](../ironclaw/src/orchestrator/api.rs) | Added `POST /jobs/self-improve` handler, `POST /orchestrator/memory-write` handler, `POST /jobs/tool-session` handler (`create_tool_session`), and `POST /worker/{job_id}/tool` handler (`sandbox_tool_handler`). All wired into the axum router. |
| [`ironclaw/src/orchestrator/auth.rs`](../ironclaw/src/orchestrator/auth.rs) | Added `allowed_tools` field to `TokenStore`. New methods: `store_allowed_tools()`, `is_tool_allowed()`, `get_allowed_tools()`. Revoke now clears the allowlist. |
| [`ironclaw/src/orchestrator/job_manager.rs`](../ironclaw/src/orchestrator/job_manager.rs) | Added `sandbox_policy` field to `ContainerHandle`, `tool_timeout_secs` field to `ContainerJobConfig`, and `execute_sandboxed_tool()` method to `ContainerJobManager`. |
| [`ironclaw/src/sandbox/config.rs`](../ironclaw/src/sandbox/config.rs) | Added `SandboxPolicy::SelfImprovementWrite` variant. Updated `allows_writes()`, added `is_self_improvement()` and `writable_path()` methods. Updated `FromStr` parser. |
| [`ironclaw/src/sandbox/manager.rs`](../ironclaw/src/sandbox/manager.rs) | Added `SandboxManager::execute_in_container()` static method — executes a JSON-RPC tool request inside an existing container via Docker exec. |
| [`ironclaw/src/error.rs`](../ironclaw/src/error.rs) | Added `OrchestratorError::JobNotFound`, `OrchestratorError::SandboxError`, and `OrchestratorError::Internal` variants. |
| [`ironclaw/src/db/libsql/mod.rs`](../ironclaw/src/db/libsql/mod.rs) | Added `pub mod self_improvement_audit` and re-exports for `LibSqlAuditRepository`, `SelfImprovementAuditRepository`, `AuditEventStatus`, `AuditEvent`. |
| [`ironclaw/Cargo.toml`](../ironclaw/Cargo.toml) | Added `crates/ironclaw_hermes_bridge`, `crates/ironclaw_hdc_dsv`, `crates/ironclaw_self_improve_dispatcher`, `crates/ironclaw_tool_bridge_rs`, `crates/ironclaw_audit_py`, `crates/ironclaw_hdc_server` to workspace members. Added `pyo3` (optional), `zeroize`, `bincode`, `dashmap`, `once_cell`, `phf`, `ring` as workspace dependencies. |
| [`ironclaw/src/sandbox/config.rs`](../ironclaw/src/sandbox/config.rs) | **(Phase 3)** Added `SandboxPolicy::DesktopAccess` variant with full doc comment (security properties, residual risks, consent gate). Updated `allows_writes()`, `writable_path()`, `FromStr`. Added `is_desktop_access()`. |
| [`ironclaw/src/sandbox/mod.rs`](../ironclaw/src/sandbox/mod.rs) | **(Phase 3)** Added `pub mod desktop`, `pub mod credential_zones`. Re-exports for all desktop sandbox types. |
| [`ironclaw/src/config/sandbox.rs`](../ironclaw/src/config/sandbox.rs) | **(Phase 3)** `to_sandbox_config()` auto-selects `ironclaw-desktop:latest` image when `policy == DesktopAccess` and no custom desktop image is configured. |
| [`ironclaw/src/tools/builtin/mod.rs`](../ironclaw/src/tools/builtin/mod.rs) | **(Phase 3)** Added `pub mod desktop`, `pub mod desktop_credential_zone`. Re-exports for all 9 desktop tools and `build_desktop_tools`. |
| [`ironclaw/src/tools/builtin/desktop.rs`](../ironclaw/src/tools/builtin/desktop.rs) | **(Phase 3)** `build_desktop_tools()` now includes `DesktopCredentialZoneTool` (9 tools total). Shares `credential_zones` between manager and tool. Screenshot and accessibility tree methods apply credential redaction. |
| [`ironclaw/src/sandbox/desktop.rs`](../ironclaw/src/sandbox/desktop.rs) | **(Phase 3)** Added `credential_zones: SharedCredentialZones` field. `screenshot()` applies tesseract+imagemagick redaction for hidden values. `accessibility_tree()` applies `redact_accessibility_tree()`. Added `shell_escape()` helper. |
| [`ironclaw/docker/desktop-sandbox.Dockerfile`](../ironclaw/docker/desktop-sandbox.Dockerfile) | **(Phase 3)** Added `tesseract-ocr`, `tesseract-ocr-eng`. Added `COPY` for `desktop-redact-screenshot.py`. |
| [`ironclaw/.env.example`](../ironclaw/.env.example) | Added all new environment variables including `IRONCLAW_HDC_SERVER_TOKEN` (required for Rust HDC server `/v1/train` auth). |
| [`hermes-agent/agent/improvement_dispatcher.py`](../hermes-agent/agent/improvement_dispatcher.py) | **Rewritten as thin wrapper** → delegates to `ironclaw_self_improve_dispatcher` Rust PyO3 extension. Falls back to `_improvement_dispatcher_py.py` with security warning. Public API unchanged. |
| [`hermes-agent/agent/improvement_audit.py`](../hermes-agent/agent/improvement_audit.py) | **Rewritten as thin wrapper** → delegates `sha256_hex()` and `record_write_event()` to `ironclaw_audit_py` Rust extension. `SelfImprovementEvent` kept as Python DTO. |
| [`hermes-agent/agent/improvement_rollback.py`](../hermes-agent/agent/improvement_rollback.py) | **Rewritten as thin wrapper** → delegates `RollbackManager` to `ironclaw_self_improve_dispatcher.PyRollbackManager` (Rust, `zeroize` on content). Falls back to `_improvement_rollback_py.py`. |
| [`hermes-agent/agent/ironclaw_tool_bridge.py`](../hermes-agent/agent/ironclaw_tool_bridge.py) | **Rewritten as thin wrapper** → delegates to `ironclaw_tool_bridge_rs` Rust PyO3 extension. Compile-time frozen `phf::Set` for sandboxed tool names. Falls back to `_ironclaw_tool_bridge_py.py`. |
| [`hermes-agent/hdc_dsv_server.py`](../hermes-agent/hdc_dsv_server.py) | **Deprecation shim added** → execs `ironclaw-hdc-server` Rust binary if found on PATH; otherwise falls back to Python FastAPI implementation with deprecation warning. |
| [`hermes-agent/agent/conversation_loop.py`](../hermes-agent/agent/conversation_loop.py) | Post-turn background review block now calls `should_use_ironclaw()` and routes through `trigger_self_improvement_async()` whenever IronClaw is available — no longer requires `HERMES_SECURE_SELF_IMPROVE=true`. Falls back to `_spawn_background_review()` only when IronClaw is genuinely unavailable or opted out. |
| [`hermes-agent/agent/curator.py`](../hermes-agent/agent/curator.py) | `maybe_run_curator()` now calls `should_use_ironclaw()` and dispatches `CURATOR_RUN` to the IronClaw sandbox whenever the orchestrator is reachable — no longer requires `HERMES_SECURE_SELF_IMPROVE=true`. Falls back to `run_curator_review()` only when IronClaw is unavailable or opted out. |
| [`hermes-agent/agent/tool_executor.py`](../hermes-agent/agent/tool_executor.py) | Both sequential and concurrent tool execution paths now attempt `execute_tool_via_ironclaw()` before falling back to `handle_function_call()` / `_invoke_tool()`. Mutating tools (`terminal`, `write_file`, `patch`, `memory`, `skill_manage`, `browser_*`) are routed through the IronClaw sandbox when the orchestrator is reachable. |

---

## Features Added

### 1. Sandboxed Self-Improvement (Cloud Mode)

IronClaw is now the **default** handler for all self-improvement work whenever the orchestrator is reachable. No feature flag is required — Hermes probes `GET /health` on the orchestrator at the start of each review cycle and routes through IronClaw automatically. The legacy in-process Hermes fork is used only when the orchestrator is unreachable.

The `HERMES_SECURE_SELF_IMPROVE=true` flag is still supported as an explicit opt-in that skips the reachability probe (useful when you want a hard failure instead of a silent fallback if the orchestrator URL is misconfigured). Set `HERMES_PREFER_LOCAL_SELF_IMPROVE=true` to force the local Hermes fork regardless of orchestrator availability.

When IronClaw handles a review cycle, every post-turn background review and curator run is submitted to the IronClaw orchestrator as a `SelfImprovementJob`. The orchestrator:

1. Issues a per-job bearer token scoped to `[skill_manage, memory]` only
2. Starts a hardened Docker container (`ironclaw/sandbox:self-improve`)
3. Injects the job token via env (no API keys, no memory credentials)
4. Proxies all LLM calls through the orchestrator (container never holds API keys)
5. Monitors the container for timeout and non-zero exit
6. Triggers rollback on failure

### 2. In-Process WASM Sandbox (Local Mode)

When `IRONCLAW_PROFILE=local-self-improve` (or `[sandbox] enabled = false`), the Docker container is replaced by the in-process wasmtime runtime. The same WASM tool bridge binary runs with:
- Fuel metering (hard CPU cap)
- Memory limit (64 MB default)
- Same tool allowlist enforcement
- Same safety layer integration

### 3. WASM Tool Bridge (`ironclaw_hermes_bridge`)

The bridge enforces all security invariants for the two allowed tools:

**`skill_manage` tool:**
- Path traversal prevention (rejects `../`, shell metacharacters)
- Ownership check (only agent-created skills)
- Content policy (credential patterns, prompt injection detection)
- Size limit: 64 KB per file
- Rate limit: 10 writes per job

**`memory` tool:**
- Proxies all writes to `POST /orchestrator/memory-write` (container never touches memory backend)
- Content policy check before forwarding
- Size limit: 256 KB per write
- Rate limit: 5 writes per job

### 4. HDC DSV Quality Gate (`ironclaw_hdc_dsv`)

A local neuromorphic learning model that:
- **Scores** each proposed write before committing (quality gate)
- **Learns** from every committed/rolled-back write (online training)
- **Bootstrap mode**: gate inactive until `SELF_IMPROVE_HDC_BOOTSTRAP_MIN` examples (default: 50)
- **Fail-closed**: blocks writes when server unreachable + `SELF_IMPROVE_HDC_BLOCK=true`
- **Fail-open**: logs warning and allows writes when server unreachable + block mode off

### 5. HDC DSV Local Server (`hdc_dsv_server.py`)

A FastAPI server exposing an OpenAI-compatible API for the HDC DSV model:
- `POST /v1/chat/completions` — score a write payload
- `POST /v1/train` — online update with labeled example
- `GET /v1/models` — model discovery
- Binds to `127.0.0.1:8765` only (no external interface)
- Model state persisted to `~/.ironclaw/hdc_model.bin` (0600 permissions)

### 6. Immutable Audit Log

Every self-modification is recorded as an immutable `SelfImprovementEvent`:
- `event_id`, `job_id`, `job_type`, `timestamp`, `action`, `target`
- `before_hash` (SHA-256 of content before), `after_hash` (SHA-256 after)
- `safety_verdict` (PASS / FLAGGED / BLOCKED)
- `hdc_score` (optional quality score)
- `llm_model`, `container_id`, `status` (PENDING → COMMITTED | ROLLED_BACK)

Stored in PostgreSQL (cloud) or libSQL (local). Never deleted.

### 7. Atomic Rollback

All writes within a job are treated as a transaction:
- Before each write: snapshot the before-state
- On success: `mark_committed()` — all audit events → COMMITTED
- On failure: restore all files in reverse order, `mark_rolled_back()` — all audit events → ROLLED_BACK

Trigger conditions:
- Safety layer blocks content → automatic rollback + job abort
- Job exceeds `max_wall_seconds` → container killed, partial writes rolled back
- Container exits non-zero → all writes rolled back atomically
- Manual: `rollback_job(job_id)` API

### 8. LLM Client Selection

| Mode | LLM Used | When |
|------|----------|------|
| `auxiliary` (default) | Resolved by `get_text_auxiliary_client("self_improve")` | Always, unless overridden |
| `main` | Same provider/model as the parent agent turn | `SELF_IMPROVE_LLM_CLIENT=main` |
| `local` | OpenAI-compatible server at `SELF_IMPROVE_LLM_BASE_URL` | `SELF_IMPROVE_LLM_CLIENT=local` |

If auxiliary mode is selected and no provider is configured, the cycle is **silently skipped** (never falls back to main model to avoid surprise token spend).

### 9. Local-First Profile (`local-self-improve.toml`)

A complete local stack with zero cloud dependencies:
- libSQL embedded database (no PostgreSQL)
- In-process WASM sandbox (no Docker)
- HDC DSV server at `localhost:8765` (no cloud LLM)
- All data stays on the local machine

### 10. Safe Desktop App Access (`SandboxPolicy::DesktopAccess`)

Desktop app access via virtual display (Xvfb), following the industry-standard architecture used by Anthropic Computer Use, OpenAI, and others.

#### Architecture

```
Host
  └── Docker container (ironclaw-desktop:latest)
        ├── Xvfb :99  — virtual framebuffer (NO connection to host DISPLAY)
        ├── fluxbox   — minimal window manager
        ├── Desktop apps (Firefox, LibreOffice, etc.)
        ├── xdotool   — input injection (mouse/keyboard) inside virtual display only
        ├── scrot     — screenshot (captures Xvfb framebuffer, not host screen)
        └── AT-SPI2   — accessibility bus → structured JSON (not raw X events)
```

#### Security properties

| Risk | Mitigated by | Residual risk |
|------|-------------|---------------|
| AI sees host screen | Xvfb virtual display (`:99`, no host DISPLAY) | None — completely isolated |
| AI reads host files | Container filesystem isolation | None |
| AI exfiltrates data via network | Domain allowlist proxy | Low — only allowlisted domains |
| AI reads clipboard | Isolated clipboard (no host bridge) | None |
| AI sees sensitive content user opens | Cannot prevent | **High** — user must not open secrets |
| Prompt injection via app UI | Content policy on accessibility tree | Medium — hard to fully prevent |
| Container escape via X11 | Xvfb has no host X socket | Low — Xvfb is well-isolated |

#### Consent gate

Every desktop session requires **explicit user approval** before starting (`ApprovalRequirement::Always` on `desktop_session_start`). This cannot be bypassed by session auto-approve. The user must acknowledge:
- The AI will be able to see everything rendered in the virtual display.
- The AI can inject keyboard and mouse input into the virtual display.
- The user must not open documents containing secrets in this session.

#### Tools

| Tool | Approval | Description |
|------|----------|-------------|
| `desktop_session_start` | **Always** (consent gate) | Start session; `consent: true` required |
| `desktop_session_stop` | UnlessAutoApproved | Stop session and remove container |
| `desktop_screenshot` | Never | Capture Xvfb framebuffer as base64 PNG (hidden credentials blacked out) |
| `desktop_click` | UnlessAutoApproved | Click at (x, y) with mouse button |
| `desktop_type` | UnlessAutoApproved | Type text (max 4096 chars) |
| `desktop_key_press` | UnlessAutoApproved | Press X11 keysym (e.g. `ctrl+c`) |
| `desktop_open_app` | UnlessAutoApproved | Launch app (validated name only) |
| `desktop_accessibility_tree` | Never | AT-SPI2 tree as JSON (hidden credentials → `[HIDDEN]`) |
| `desktop_credential_zone` | **Always** | Manage hidden/visible credential zones |

#### Credential zones

Two separate zones allow fine-grained control over what the AI can see:

**Hidden zone** (`add_hidden` action):
- Values are stored in memory only, never written to disk or logs.
- Any occurrence in screenshots is blacked out using tesseract OCR + imagemagick.
- Any occurrence in the accessibility tree is replaced with `[HIDDEN]`.
- Values are zeroized on drop (`clear_hidden` or session end).
- The AI is told redaction occurred but never sees the values.

**Visible zone** (`add_visible` action):
- Credentials the AI is allowed to use (e.g. test accounts, demo logins).
- Passed to the AI as structured JSON: `{label, username, password, notes}`.
- NOT redacted from screenshots or accessibility tree.
- Use for: test account credentials, demo passwords, staging environment logins.

**Example workflow:**
```
1. User calls desktop_credential_zone(action="add_hidden", value="my-real-password")
   → AI will never see "my-real-password" in any screenshot or accessibility tree

2. User calls desktop_credential_zone(action="add_visible", label="Test account",
                                       username="test@example.com", password="demo123")
   → AI can see and use "demo123" for the test account

3. AI calls desktop_screenshot() → "my-real-password" is blacked out; "demo123" is visible
4. AI calls desktop_accessibility_tree() → "my-real-password" → "[HIDDEN]"; "demo123" visible
```

**Limitations:**
- Screenshot redaction is best-effort (OCR-based). Unusual fonts may not be caught.
- Accessibility tree redaction is exact string match — more reliable.
- Neither prevents inference from context clues.

#### Configuration

Set `SANDBOX_POLICY=desktop_access` (or `desktop`, `desktopaccess`) to enable.
The image defaults to `ironclaw-desktop:latest`; override with `SANDBOX_IMAGE=my-desktop-image`.

Build the image:
```bash
docker build -f ironclaw/docker/desktop-sandbox.Dockerfile -t ironclaw-desktop:latest ironclaw/
```

---

## Workflow: Post-Turn Self-Improvement

```
1. User sends message → Hermes processes turn → delivers response

2. conversation_loop.py checks:
   - HERMES_SECURE_SELF_IMPROVE=true?
   - _should_review_memory or _should_review_skills?

3. improvement_dispatcher.py:
   a. Resolves LLM client (auxiliary / main / local)
   b. If no client available → log warning, skip cycle (no error)
   c. Encrypts conversation snapshot (AES-256-GCM)
   d. POST /jobs/self-improve → IronClaw orchestrator
   e. Returns immediately (non-blocking)

4. IronClaw orchestrator:
   a. Issues per-job bearer token (scoped to skill_manage + memory)
   b. Starts sandbox container (Docker or in-process WASM)
   c. Injects job token via env (no API keys)

5. Sandbox container:
   a. Hermes review fork reads conversation snapshot
   b. Calls LLM via orchestrator proxy (resolved model)
   c. For each proposed write:
      i.  HDC DSV adapter scores the write (if enabled)
      ii. Content policy checks the write
      iii. Rate limiter checks the write
      iv. skill_manage → writes to /hermes-skills/
          memory → POST /orchestrator/memory-write → host MemoryManager
      v.  Audit event recorded (PENDING)

6. On job completion:
   - Success → mark_committed() → all events COMMITTED
   - Failure → rollback() → restore files → mark_rolled_back()

7. HDC DSV training (if enabled):
   - Committed write → train(GOOD_WRITE)
   - Rolled-back write → train(BAD_WRITE)
```

---

## Configuration Reference

All new environment variables (additive, no breaking changes):

| Variable | Default | Description |
|----------|---------|-------------|
| `HERMES_SECURE_SELF_IMPROVE` | `false` | **Explicit opt-in**: always use IronClaw, skip the reachability probe. Useful when you want a hard failure (not a silent fallback) if the orchestrator URL is misconfigured. No longer required — IronClaw is used automatically when reachable. |
| `HERMES_PREFER_LOCAL_SELF_IMPROVE` | `false` | **Explicit opt-out**: always use the local Hermes review fork, even when the IronClaw orchestrator is reachable. |
| `IRONCLAW_ORCHESTRATOR_URL` | `http://localhost:8080` | IronClaw orchestrator internal URL (also used for the reachability probe) |
| `IRONCLAW_ORCHESTRATOR_TOKEN` | — | Bearer token for dispatcher → orchestrator auth |
| `SELF_IMPROVE_LLM_CLIENT` | `auxiliary` | LLM client: `auxiliary`, `main`, or `local` |
| `SELF_IMPROVE_LLM_BASE_URL` | — | When `local`: base URL of local model server |
| `SELF_IMPROVE_LLM_MODEL` | — | When `local`: model name |
| `SELF_IMPROVE_MAX_TURNS` | `10` | Hard cap on review agent turns per job |
| `SELF_IMPROVE_MAX_WALL_SECS` | `120` | Hard timeout per job (seconds) |
| `SELF_IMPROVE_MAX_SKILL_WRITES` | `10` | Max skill writes per job |
| `SELF_IMPROVE_MAX_MEMORY_WRITES` | `5` | Max memory writes per job |
| `SELF_IMPROVE_ROLLBACK_ON_VIOLATION` | `true` | Auto-rollback on safety violation |
| `SELF_IMPROVE_HDC_ENABLED` | `false` | Enable HDC DSV quality gate |
| `SELF_IMPROVE_HDC_BLOCK` | `false` | Block writes below threshold |
| `SELF_IMPROVE_HDC_TRAIN` | `false` | Enable online learning |
| `SELF_IMPROVE_HDC_THRESHOLD` | `0.4` | Minimum HDC quality score |
| `SELF_IMPROVE_HDC_BOOTSTRAP_MIN` | `50` | Min training examples before gate is active |
| `IRONCLAW_HDC_MODEL_PATH` | `~/.ironclaw/hdc_model.bin` | HDC DSV model state file |
| `IRONCLAW_HDC_SERVER_URL` | `http://localhost:8765/v1` | HDC DSV server URL |
| `IRONCLAW_HDC_SERVER_TOKEN` | — | **Required for Rust HDC server.** Bearer token for `POST /v1/train` and `POST /v1/chat/completions`. Generate with `openssl rand -hex 32`. Without this, all write requests return 401. |
| `IRONCLAW_DB_ENCRYPTION_KEY` | — | libSQL encryption key (auto-generated if unset) |
| `IRONCLAW_AUDIT_BACKEND` | same as `DATABASE_BACKEND` | Override DB backend for audit log |

---

## Quick Start

### Build the Rust Security Extensions (recommended)

The Rust PyO3 extensions and the `ironclaw-hdc-server` binary eliminate the security
vulnerabilities in the Python fallback implementations. Build them before running Hermes:

```bash
# Build all four Rust crates
cd ironclaw
cargo build --release \
  -p ironclaw_self_improve_dispatcher \
  -p ironclaw_tool_bridge_rs \
  -p ironclaw_audit_py \
  -p ironclaw_hdc_server

# Install PyO3 extension modules into the Hermes virtualenv
PYTHON_SITE=$(python3 -c "import site; print(site.getsitepackages()[0])")
cp target/release/libironclaw_self_improve_dispatcher.so \
   "${PYTHON_SITE}/ironclaw_self_improve_dispatcher.so"
cp target/release/libironclaw_tool_bridge_rs.so \
   "${PYTHON_SITE}/ironclaw_tool_bridge_rs.so"
cp target/release/libironclaw_audit_py.so \
   "${PYTHON_SITE}/ironclaw_audit_py.so"

# Verify the extensions are importable
python3 -c "import ironclaw_self_improve_dispatcher; import ironclaw_tool_bridge_rs; import ironclaw_audit_py; print('Rust extensions OK')"
```

### Cloud Mode (Docker + PostgreSQL)

IronClaw is used automatically when the orchestrator is reachable — no feature flag required.

```bash
# 1. Set the orchestrator connection (IronClaw is preferred by default)
export IRONCLAW_ORCHESTRATOR_URL=http://localhost:8080
export IRONCLAW_ORCHESTRATOR_TOKEN=<your-token>

# 2. (Optional) Use auxiliary LLM for review (default)
export SELF_IMPROVE_LLM_CLIENT=auxiliary

# 3. Run Hermes — self-improvement routes through IronClaw sandbox automatically
hermes
```

> **Explicit opt-in (skip reachability probe):** Set `HERMES_SECURE_SELF_IMPROVE=true` if you
> want a hard failure instead of a silent fallback when the orchestrator URL is misconfigured.
>
> **Explicit opt-out (force local fork):** Set `HERMES_PREFER_LOCAL_SELF_IMPROVE=true` to
> always use the in-process Hermes review fork regardless of orchestrator availability.

### Local Mode (No Docker, No Cloud)

```bash
# 1. Start the Rust HDC DSV server (recommended over Python fallback)
export IRONCLAW_HDC_SERVER_TOKEN=$(openssl rand -hex 32)
export IRONCLAW_HDC_MODEL_PATH=~/.ironclaw/hdc_model.bin
ironclaw-hdc-server &
# (or: python hermes-agent/hdc_dsv_server.py & — will auto-exec Rust binary if on PATH)

# 2. Set the local profile (IronClaw auto-detected via localhost:8080 health probe)
export IRONCLAW_PROFILE=local-self-improve
export SELF_IMPROVE_LLM_CLIENT=local
export SELF_IMPROVE_LLM_BASE_URL=http://localhost:8765/v1
export SELF_IMPROVE_LLM_MODEL=hdc-dsv-local

# 3. Enable HDC DSV quality gate with online learning
export SELF_IMPROVE_HDC_ENABLED=true
export SELF_IMPROVE_HDC_BLOCK=true
export SELF_IMPROVE_HDC_TRAIN=true

# 4. Run Hermes
hermes
```

### Migrating from Python HDC Server to Rust HDC Server

If you have an existing `hdc_model.bin` from the Python server (pickle format):

```bash
# Run the one-time migration script
python ironclaw/scripts/migrate_hdc_model.py \
  --input ~/.ironclaw/hdc_model.bin \
  --output ~/.ironclaw/hdc_model_migrated.json

# Follow the printed instructions to complete the migration.
# After verifying the Rust server works, delete the old pickle file:
rm ~/.ironclaw/hdc_model.bin
```

### Running Tests

```bash
# Rust tests (all new security hardening tests)
cd ironclaw
cargo test self_improvement_dispatcher_rs
cargo test tool_bridge_rs
cargo test audit_py_bindings
cargo test hdc_server_rs

# Or run all self-improvement tests at once
cargo test self_improvement

# Python tests (public API compatibility — must pass with both Rust and Python backends)
cd hermes-agent
pytest tests/test_improvement_dispatcher.py tests/test_improvement_audit.py tests/test_hdc_dsv_server.py -v
```

---

## Rust Security Hardening Summary

The following table shows the security properties gained by the Rust rewrite vs the original Python implementation:

| Component | Before (Python) | After (Rust) |
|-----------|----------------|--------------|
| **Encryption** | AES-256-GCM **or** base64 fallback if `cryptography` not installed | AES-256-GCM only — hard dependency, no fallback |
| **Key material in memory** | Python GC, not zeroed | `zeroize::Zeroizing` on drop |
| **Snapshot deserialization** | `json.loads` + pickle risk in import chain | `serde_json` typed deserialization |
| **Fail-closed guarantee** | Python import error silently disables bridge | Rust `.so` crash = hard fail, no silent fallback |
| **Sandboxed tool set** | Runtime `frozenset` (mutable before freeze) | Compile-time `phf::Set` (baked into binary) |
| **HDC model file format** | Python pickle (RCE on load) | `bincode` typed schema (no code execution on load) |
| **HDC server auth** | None on `/v1/train` (model poisoning risk) | Bearer token, constant-time comparison (`subtle`) |
| **SHA-256 hashing** | `hashlib.sha256` (monkey-patchable) | `sha2::Sha256` (not patchable from Python) |
| **Regex engine** | Python `re` (catastrophic backtracking possible) | Rust `regex` crate (linear-time NFA) |
| **Thread safety** | Python GIL | `Arc<Mutex<>>` explicit critical sections |
| **Session registry** | `dict` + `threading.Lock` | `DashMap` (lock-free concurrent HashMap) |

### Threats eliminated by the Rust rewrite

| Threat | Eliminated by |
|--------|--------------|
| Prompt injection → host RCE via Python `eval`/`exec` in import chain | Rust binary has no interpreter |
| Pickle RCE from crafted orchestrator response or `hdc_model.bin` | Rust uses `bincode` with typed deserialization |
| AES-256-GCM silent fallback to base64 | `aes-gcm` crate is a hard dependency |
| HDC model poisoning via unauthenticated `POST /v1/train` | Rust server requires bearer token on all write endpoints |
| SHA-256 hash lying via Python monkey-patching | Hashing moved into Rust `sha2` crate |
| Fail-closed bypass via Python import error | Rust `.so` import failure is a hard crash |
| TOCTOU race in snapshot/encrypt path | `Arc<Mutex<>>` with explicit critical sections |
| Secrets in memory not zeroed | `zeroize::Zeroizing` on key material |

---

## Security Boundaries

**What the sandbox CAN do:**
- Call the LLM via the orchestrator proxy (no direct API key access)
- Write to `/hermes-skills/` (agent-created skills only, content-policy checked)
- Proxy memory writes to the orchestrator host via `POST /orchestrator/memory-write`
- Read the conversation snapshot (decrypted inside the container by the daemon)

**What the sandbox CANNOT do:**
- Access the internet directly (no outbound network except orchestrator bridge)
- Read or write the host filesystem (memory backend is never mounted)
- Access any credentials (injected at host boundary, never passed into container)
- Execute shell commands (`terminal` tool not in allowed list)
- Modify user-created skills or delete anything
- Spawn child processes (seccomp `clone` restriction)
- Directly call any memory provider API (all memory writes go through the host proxy)

---

## Security Scope: What Is and Is Not Sandboxed

This is the most important section for understanding the actual security posture of this implementation.

### What IS sandboxed by this implementation

| Scope | Sandboxed? | Mechanism |
|-------|-----------|-----------|
| **Self-improvement background review** (post-turn skill/memory review) | ✅ Yes | `improvement_dispatcher.py` → IronClaw orchestrator → Docker container or in-process WASM |
| **Curator runs** (`maybe_run_curator`) | ✅ Yes | Same dispatcher path whenever IronClaw orchestrator is reachable |
| **Skill writes inside the sandbox** | ✅ Yes | `SkillManageTool` in `ironclaw_hermes_bridge` — content policy, ownership check, rate limit |
| **Memory writes inside the sandbox** | ✅ Yes | `MemoryProxyTool` — proxied to host `MemoryManager`, container never touches backend |
| **LLM calls inside the sandbox** | ✅ Yes | Orchestrator proxy — container never holds API keys |
| **Audit trail for self-improvement writes** | ✅ Yes | `LibSqlAuditRepository` / PostgreSQL — immutable INSERT-only rows |
| **Rollback on failure** | ✅ Yes | `RollbackManager` — restores before-state atomically |

### What is sandboxed for regular tool calls (via `ironclaw_tool_bridge.py`)

| Scope | Sandboxed? | Mechanism |
|-------|-----------|-----------|
| **`terminal` tool** (shell command execution) | ✅ Yes (when orchestrator reachable) | `ironclaw_tool_bridge.py` → `POST /worker/{job_id}/tool` → IronClaw sandbox container |
| **`write_file` / `patch` tool** | ✅ Yes (when orchestrator reachable) | Same bridge path — `WorkspaceWrite` policy enforces filesystem scope |
| **`memory` tool during normal conversation** | ✅ Yes (when orchestrator reachable) | Same bridge path — proxied through orchestrator |
| **`skill_manage` during normal conversation** | ✅ Yes (when orchestrator reachable) | Same bridge path — content policy + ownership check enforced |
| **`browser_*` tools** | ✅ Yes (when orchestrator reachable) | Same bridge path — network isolation via orchestrator proxy |
| **MCP tool calls** (`mcp__*` prefix) | ✅ Yes (when orchestrator reachable) | Same bridge path — MCP servers run inside the sandbox container, not as host processes |

### Fully fail-closed semantics — no host execution fallback

**There is no fallback to direct host execution for sandboxed tools.** Every sandboxed tool call either succeeds inside the IronClaw sandbox or is blocked with a diagnostic error message. This applies in all cases:

- **Sandbox execution failure** (container crash, network error, tool error) → `ToolBridgeResult(blocked=True)` with error message surfaced to the model.
- **Orchestrator unreachable** (session creation failed) → `ToolBridgeResult(blocked=True)` with a diagnostic message explaining that the IronClaw orchestrator is required and how to start it.
- **Bridge module unavailable** (import error) → same fail-closed treatment for sandboxed tools.

`ToolBridgeResult(fallback=True)` is **only** returned for tools that are not in the sandboxed set (read-only tools like `read_file`, `grep`, etc.) — those are never sandboxed and always execute directly. Only mutating tools are subject to the fail-closed policy.

The model sees the diagnostic error as the tool result and can inform the user. The user can then start the IronClaw orchestrator and retry.

### What is NOT sandboxed by this implementation

| Scope | Sandboxed? | Current mechanism | Gap |
|-------|-----------|-------------------|-----|
| **Regular agent tool calls when orchestrator is unreachable** | ❌ No (fallback) | `tool_executor.py` falls back to `handle_function_call()` / `_invoke_tool()` in the same process | Fallback path runs with full host privileges — same as before |
| **Read-only tools** (`read_file`, `list_dir`, `grep`, etc.) | ❌ Not routed (intentional) | Execute directly — no write risk, routing would add latency | By design: only mutating tools are sandboxed |

### How IronClaw handles regular tool calls (when Hermes runs as an IronClaw worker)

When Hermes Agent runs **as a worker inside IronClaw** (i.e., the IronClaw orchestrator starts a Hermes container via `JobMode::Worker`), the situation is different:

- The entire Hermes process runs inside a Docker container managed by IronClaw's `ContainerJobManager`
- The `SandboxManager` in [`ironclaw/src/sandbox/manager.rs`](../ironclaw/src/sandbox/manager.rs) handles `terminal` tool execution inside a nested container
- Network access is proxied through IronClaw's HTTP proxy with an allowlist
- The `SandboxPolicy` (ReadOnly / WorkspaceWrite) controls filesystem access

**In that configuration, regular tool calls ARE sandboxed** — but by IronClaw's existing worker container infrastructure, not by this self-improvement implementation.

### The gap: standalone Hermes usage

When Hermes is run **standalone** (not as an IronClaw worker), regular tool calls execute in the host process with no container isolation. The existing safety mechanisms are:

1. **[`tool_guardrails.py`](../hermes-agent/agent/tool_guardrails.py)** — tracks mutating tool calls, detects loops, enforces per-turn budgets. Does NOT provide container isolation.
2. **[`file_safety.py`](../hermes-agent/agent/file_safety.py)** — path validation for file writes. Does NOT prevent all dangerous writes.
3. **User approval callbacks** — `terminal` tool can require user approval before execution. Configurable.
4. **`tool_guardrails` MUTATING_TOOL_NAMES** — tracks `terminal`, `write_file`, `patch`, `memory`, `skill_manage`, `browser_*`, etc. for loop detection and budget enforcement.

These are **process-level guardrails**, not kernel-level isolation. A prompt injection attack that bypasses the guardrails can execute arbitrary shell commands on the host.

### Recommended next steps to fully sandbox regular tool calls

To achieve the same level of isolation for regular tool calls as IronClaw provides for worker jobs:

1. **Run Hermes as an IronClaw worker** — use `JobMode::Worker` so all Hermes tool calls go through IronClaw's `SandboxManager`. This is the intended production deployment model.

2. **Extend `SandboxPolicy::SelfImprovementWrite`** — the new policy variant added in this implementation can be extended to cover regular tool calls with a broader allowlist (e.g., add `terminal` with `WorkspaceWrite` policy).

3. **Wire `tool_executor.py` through the IronClaw sandbox** — instead of calling `handle_function_call()` directly, route mutating tool calls through `POST /worker/{job_id}/tool` on the orchestrator, which then executes them inside the container. This is a significant refactor.

4. **Use IronClaw's existing `SandboxManager` for terminal** — IronClaw already has [`src/sandbox/manager.rs`](../ironclaw/src/sandbox/manager.rs) which runs commands in Docker containers with network proxying. Hermes's `terminal` tool could call this instead of `sh -c` directly.

### Summary table

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Security Isolation by Execution Context                   │
├──────────────────────────┬──────────────────────────┬───────────────────────┤
│ Context                  │ Tool Calls               │ Isolation Level       │
├──────────────────────────┼──────────────────────────┼───────────────────────┤
│ Hermes standalone        │ Mutating tools           │ Docker container      │
│ + IronClaw reachable     │ (terminal, write_file,   │ + network proxy       │
│                          │ patch, memory,           │ + SandboxPolicy       │
│                          │ skill_manage, browser_*, │ (via tool bridge)     │
│                          │ mcp__*)                  │                       │
│                          ├──────────────────────────┼───────────────────────┤
│                          │ Read-only tools          │ Direct (no overhead)  │
├──────────────────────────┼──────────────────────────┼───────────────────────┤
│ Hermes standalone        │ Mutating tools           │ BLOCKED — diagnostic  │
│ (IronClaw unreachable)   │ (terminal, write_file,   │ error returned to     │
│                          │ patch, memory, etc.)     │ model. No host exec.  │
│                          ├──────────────────────────┼───────────────────────┤
│                          │ Read-only tools          │ Direct (no risk)      │
├──────────────────────────┼──────────────────────────┼───────────────────────┤
│ Hermes as IronClaw       │ All tools (terminal etc) │ Docker container      │
│ worker (JobMode::Worker) │                          │ + network proxy       │
│                          │                          │ + SandboxPolicy       │
├──────────────────────────┼──────────────────────────┼───────────────────────┤
│ Self-improvement fork    │ skill_manage + memory    │ Docker container      │
│ (this implementation)    │ ONLY (allowlist enforced)│ + WASM bridge         │
│                          │                          │ + content policy      │
│                          │                          │ + audit + rollback    │
└──────────────────────────┴──────────────────────────┴───────────────────────┘
```

**Bottom line:** Mutating tool calls (`terminal`, `write_file`, `patch`, `memory`, `skill_manage`, `browser_*`, `mcp__*`) are **always** routed through the IronClaw sandbox. If the orchestrator is unreachable, the tool is blocked with a diagnostic error — it is never executed directly on the host. Read-only tools (`read_file`, `grep`, etc.) execute directly with no overhead. There is no opt-out for sandboxed tools.
