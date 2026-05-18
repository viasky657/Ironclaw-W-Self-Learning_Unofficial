# Rust Rewrite: Self-Improvement Security Hardening Plan

**Status:** Proposed  
**Date:** 2026-05-18  
**Supersedes:** [`plans/IMPLEMENTATION_README.md`](IMPLEMENTATION_README.md) (additive — does not remove existing work)

---

## Executive Summary

The current self-improvement pipeline has four Python components that run with **host-level privileges** or handle **security-critical logic** in an interpreted language with no memory safety, dynamic import chains, and pickle-based serialization risks. This plan replaces those components with Rust, using **PyO3** to expose thin Python wrappers so the Hermes agent codebase requires minimal changes.

The in-sandbox review fork (the Hermes Python wheel running inside the Docker container) is **not** rewritten — the Rust WASM bridge already enforces all write invariants regardless of what the Python code inside the container does.

---

## Threat Model: What We Are Defending Against

| Threat | Current Exposure | After This Plan |
|--------|-----------------|-----------------|
| **Prompt injection to host RCE** via Python `eval`/`exec` in import chain | High — dispatcher runs in host process | Eliminated — Rust binary has no interpreter |
| **Pickle RCE** from crafted orchestrator HTTP response or `hdc_model.bin` | High — `hdc_model.bin` uses Python pickle serialization | Eliminated — Rust uses `bincode`/`serde` with typed deserialization |
| **AES-256-GCM fallback to plaintext** when `cryptography` package missing | High — `_encrypt_snapshot()` silently degrades to base64 | Eliminated — Rust `aes-gcm` crate is a hard dependency, no fallback |
| **HDC model poisoning** via unauthenticated `POST /v1/train` | Medium — FastAPI server has no auth on `/v1/train` | Eliminated — Rust server requires bearer token on all write endpoints |
| **SHA-256 hash lying** in Python audit shim | Medium — Python `sha256_hex()` could be monkey-patched | Eliminated — hashing moves into Rust audit crate |
| **Fail-closed bypass** via Python import error | Medium — if `ironclaw_tool_bridge` fails to import, tool_executor falls back | Eliminated — Rust `.so` loaded via PyO3; import failure is a hard crash |
| **TOCTOU race** in snapshot/encrypt path | Low — Python GIL does not protect cross-thread state | Eliminated — Rust `Arc<Mutex<>>` with explicit critical sections |
| **Regex catastrophic backtracking DoS** | Low — Python `re` module in any content-policy equivalent | Eliminated — Rust `regex` crate uses linear-time NFA engine |
| **Secrets in memory not zeroed** | Low — Python GC does not zero memory | Improved — Rust `zeroize` crate on key material |

---

## Scope

### Rewrite in Rust (this plan)

| Component | Current File | Replacement |
|-----------|-------------|-------------|
| Dispatcher + AES-256-GCM encryption + HTTP client | [`hermes-agent/agent/improvement_dispatcher.py`](../hermes-agent/agent/improvement_dispatcher.py) | New crate `ironclaw_self_improve_dispatcher` |
| Fail-closed tool bridge + session lifecycle | [`hermes-agent/agent/ironclaw_tool_bridge.py`](../hermes-agent/agent/ironclaw_tool_bridge.py) | New crate `ironclaw_tool_bridge_rs` |
| Rollback manager (snapshot + restore) | [`hermes-agent/agent/improvement_rollback.py`](../hermes-agent/agent/improvement_rollback.py) | Merged into `ironclaw_self_improve_dispatcher` |
| Audit SHA-256 hashing + write recording | [`hermes-agent/agent/improvement_audit.py`](../hermes-agent/agent/improvement_audit.py) | Extended `ironclaw/src/db/libsql/self_improvement_audit.rs` + new `ironclaw_audit_py` PyO3 crate |
| HDC DSV local server | [`hermes-agent/hdc_dsv_server.py`](../hermes-agent/hdc_dsv_server.py) | New crate `ironclaw_hdc_server` (Axum binary) |

### Keep in Python (out of scope)

| Component | Reason |
|-----------|--------|
| Hermes review fork inside the sandbox container | Rust WASM bridge enforces all write invariants; rewriting requires rewriting the entire Hermes agent |
| `conversation_loop.py`, `curator.py`, `tool_executor.py` call sites | These become one-line calls into the PyO3 bindings — no logic change needed |
| Read-only tools (`read_file`, `grep`, etc.) | No write risk; no security value in sandboxing |

---

## Architecture After Rewrite

```
Hermes Main Agent (conversation_loop.py)
    |
    |  post-turn trigger
    v
ironclaw_self_improve_dispatcher (Rust, loaded via PyO3)
    |  LLM client resolution (typed enum, no getattr)
    |  AES-256-GCM encryption (ring/aes-gcm, no fallback)
    |  Snapshot serialization (serde_json, no pickle)
    |  POST /jobs/self-improve  (reqwest + rustls)
    |  RollbackManager (Arc<Mutex<Vec<SkillSnapshot>>>)
    v
IronClaw Orchestrator (existing Rust, unchanged)
    |
    v
Sandbox Container (Docker / WASM)
    |  Hermes review fork (Python wheel, read-only)
    |  ironclaw_hermes_bridge (Rust WASM -- existing, unchanged)
    v
ironclaw_audit_py (Rust, PyO3 bindings)
    |  sha256_hex() -- constant-time, no monkey-patch risk
    |  record_write_event() -- typed, no dict shim
    v
libSQL / PostgreSQL (existing, unchanged)

---

Hermes tool_executor.py
    |
    v
ironclaw_tool_bridge_rs (Rust, loaded via PyO3)
    |  SANDBOXED_TOOL_NAMES (frozen Rust HashSet, compile-time)
    |  BridgeSession (Arc<Mutex<SessionState>>)
    |  Fail-closed: blocked=True on any failure
    |  POST /worker/{job_id}/tool  (reqwest + rustls)
    v
IronClaw Orchestrator sandbox container

---

ironclaw_hdc_server (Rust Axum binary, replaces hdc_dsv_server.py)
    |  POST /v1/chat/completions  (bearer token required)
    |  POST /v1/train             (bearer token required)
    |  GET  /v1/models            (public)
    |  GET  /health               (public)
    |  Model state: bincode + zeroize, 0600 permissions
    +- Binds to 127.0.0.1:8765 only
```

---

## New Files

### Phase 1 — `ironclaw_self_improve_dispatcher` crate

**Location:** `ironclaw/crates/ironclaw_self_improve_dispatcher/`

| File | Purpose |
|------|---------|
| `Cargo.toml` | Crate manifest. `crate-type = ["cdylib", "rlib"]`. Dependencies: `pyo3`, `aes-gcm`, `ring`, `reqwest`, `serde`, `serde_json`, `tokio`, `zeroize`, `thiserror`, `tracing`, `uuid`. |
| `src/lib.rs` | Crate root. Re-exports all public types. Declares `#[pymodule]` entry point `ironclaw_self_improve_dispatcher`. |
| `src/types.rs` | `JobType` enum (MemoryReview / SkillReview / CuratorRun / SweTask). `LlmClientMode` enum (Auxiliary / Main / Local). `ResolvedLlm` struct (provider, model, base_url). `EncryptedSnapshot` struct (ciphertext, nonce, key_id). `DispatchResult` struct (job_id, skipped, error). |
| `src/config.rs` | `DispatcherConfig` — reads all env vars (`IRONCLAW_ORCHESTRATOR_URL`, `IRONCLAW_ORCHESTRATOR_TOKEN`, `SELF_IMPROVE_LLM_CLIENT`, `SELF_IMPROVE_MAX_TURNS`, etc.) with typed defaults. No `getattr` on Python objects. |
| `src/crypto.rs` | `encrypt_snapshot(payload: &[u8]) -> Result<EncryptedSnapshot, CryptoError>`. Uses `aes-gcm` crate with `ring::rand::SystemRandom` for key + nonce generation. `zeroize::Zeroize` on key material on drop. **No plaintext fallback.** |
| `src/llm_resolver.rs` | `resolve_llm_client(mode: LlmClientMode, agent_info: &AgentInfo) -> Result<ResolvedLlm, LlmError>`. `AgentInfo` is a plain Rust struct populated from Python via PyO3 (provider: String, model: String, base_url: Option<String>) — no dynamic `getattr` at resolution time. |
| `src/orchestrator_client.rs` | `OrchestratorClient` — `reqwest::Client` with `rustls-tls-native-roots`. Methods: `health_check()`, `submit_self_improve_job()`, `submit_tool_session()`, `execute_sandboxed_tool()`, `complete_job()`. All return typed `Result<T, OrchestratorError>`. |
| `src/snapshot.rs` | `build_minimal_snapshot(agent_info: &AgentInfo, messages: &[Message]) -> serde_json::Value`. `Message` is a typed struct (role: String, content: String). No arbitrary dict access. |
| `src/rollback.rs` | `RollbackManager` struct. `snapshot_skill(skill_name, content_before, event_id)`. `commit() -> Result`. `rollback(reason) -> Result`. Uses `Arc<Mutex<Vec<SkillSnapshot>>>`. `SkillSnapshot` has `zeroize` on content_before. |
| `src/dispatcher.rs` | `trigger_self_improvement(config, agent_info, job_type, messages) -> DispatchResult`. `trigger_self_improvement_async(...)` — spawns `tokio::task::spawn_blocking`. `should_use_ironclaw(config) -> bool` — probes `/health` with `reqwest`. |
| `src/pyo3_bindings.rs` | PyO3 `#[pyclass]` wrappers: `PyDispatcherConfig`, `PyAgentInfo`, `PyDispatchResult`. `#[pyfunction]` exports: `trigger_self_improvement_py`, `trigger_self_improvement_async_py`, `should_use_ironclaw_py`. |

### Phase 2 — `ironclaw_tool_bridge_rs` crate

**Location:** `ironclaw/crates/ironclaw_tool_bridge_rs/`

| File | Purpose |
|------|---------|
| `Cargo.toml` | `crate-type = ["cdylib", "rlib"]`. Dependencies: `pyo3`, `reqwest`, `serde`, `serde_json`, `tokio`, `thiserror`, `tracing`, `uuid`, `once_cell`, `dashmap`. |
| `src/lib.rs` | Crate root. `#[pymodule]` entry point `ironclaw_tool_bridge_rs`. |
| `src/types.rs` | `ToolBridgeResult` enum: `Ok(String)`, `Fallback`, `Blocked { message: String }`. `SandboxedToolSet` — `phf::Set` of tool names (compile-time frozen, no runtime mutation). |
| `src/session.rs` | `BridgeSession` struct. `Arc<Mutex<SessionState>>` where `SessionState` holds `job_id: Option<String>`, `job_token: Option<String>`, `closed: bool`. `create_job()`, `ensure_job()`, `execute_tool()`, `close()`. All fail-closed: `Blocked` on any error after session is established. |
| `src/registry.rs` | `SessionRegistry` — `Arc<DashMap<String, Arc<BridgeSession>>>`. `get_or_create(session_id)`, `close_session(session_id)`, `close_all()`. Global singleton via `once_cell::sync::Lazy`. |
| `src/policy.rs` | `is_sandboxed_tool(name: &str) -> bool` — checks `SANDBOXED_TOOL_NAMES` set + `browser_` prefix + `mcp__` prefix. Compile-time `phf` set, no runtime string allocation. |
| `src/pyo3_bindings.rs` | `#[pyclass] PyToolBridgeResult`. `#[pyfunction]` exports: `execute_tool_via_ironclaw_py`, `should_sandbox_tool_py`, `get_or_create_session_py`, `close_session_py`, `close_all_sessions_py`. |

### Phase 3 — `ironclaw_audit_py` crate

**Location:** `ironclaw/crates/ironclaw_audit_py/`

| File | Purpose |
|------|---------|
| `Cargo.toml` | `crate-type = ["cdylib"]`. Thin PyO3 wrapper around the existing `LibSqlAuditRepository`. Dependencies: `pyo3`, `tokio`, `sha2`, `ironclaw` (workspace dep for `LibSqlAuditRepository`). |
| `src/lib.rs` | `#[pymodule]` entry point `ironclaw_audit_py`. `#[pyfunction] sha256_hex_py(content: &str) -> String` — calls `sha2::Sha256` directly, no Python hashlib. `#[pyfunction] record_write_event_py(...)` — constructs `AuditEvent` and calls `LibSqlAuditRepository::insert`. `#[pyfunction] mark_committed_py(job_id: &str)`. `#[pyfunction] mark_rolled_back_py(job_id: &str)`. |

### Phase 4 — `ironclaw_hdc_server` binary crate

**Location:** `ironclaw/crates/ironclaw_hdc_server/`

| File | Purpose |
|------|---------|
| `Cargo.toml` | `[[bin]]` target `ironclaw-hdc-server`. Dependencies: `axum`, `tokio`, `serde`, `serde_json`, `thiserror`, `tracing`, `tracing-subscriber`, `bincode`, `zeroize`, `sha2`, `subtle`, `tower-http`. |
| `src/main.rs` | Axum router setup. Binds to `127.0.0.1:8765` only (hard-coded, not configurable to prevent accidental exposure). Loads model state from `IRONCLAW_HDC_MODEL_PATH`. Registers signal handler for graceful shutdown. |
| `src/model.rs` | `HdcDsvModel` struct. Bag-of-characters hypervector encoding (same algorithm as Python version). `score(content: &str) -> f32`. `train(content: &str, label: WriteOutcome)`. `save(path: &Path) -> Result` — `bincode::serialize` + `fs::set_permissions(0o600)`. `load(path: &Path) -> Result` — `bincode::deserialize` with typed schema (no pickle, no arbitrary code execution on load). |
| `src/auth.rs` | `BearerAuthLayer` — Tower middleware. Reads `IRONCLAW_HDC_SERVER_TOKEN` env var. Rejects requests to `/v1/train` and `/v1/chat/completions` without a valid bearer token. `/v1/models` and `/health` are public. Constant-time comparison via `subtle::ConstantTimeEq`. |
| `src/handlers.rs` | `POST /v1/chat/completions` — score handler. `POST /v1/train` — online learning handler. `GET /v1/models` — model discovery. `GET /health` — liveness. All handlers return typed `axum::Json<T>` responses. |
| `src/types.rs` | `ChatCompletionRequest`, `ChatCompletionResponse`, `TrainRequest`, `TrainResponse`, `ModelsResponse`, `WriteOutcome` (GoodWrite / BadWrite). All `#[derive(Serialize, Deserialize)]`. |

---

## Modified Files

| File | Change |
|------|--------|
| [`ironclaw/Cargo.toml`](../ironclaw/Cargo.toml) | Add `crates/ironclaw_self_improve_dispatcher`, `crates/ironclaw_tool_bridge_rs`, `crates/ironclaw_audit_py`, `crates/ironclaw_hdc_server` to workspace `members`. Add `pyo3` as optional workspace dependency. |
| [`hermes-agent/agent/improvement_dispatcher.py`](../hermes-agent/agent/improvement_dispatcher.py) | Replace all logic with a thin wrapper that imports `ironclaw_self_improve_dispatcher` (PyO3 `.so`) and delegates. Keep the same public API (`trigger_self_improvement`, `trigger_self_improvement_async`, `should_use_ironclaw`, `JOB_TYPE_*` constants) for backward compatibility. |
| [`hermes-agent/agent/ironclaw_tool_bridge.py`](../hermes-agent/agent/ironclaw_tool_bridge.py) | Replace all logic with a thin wrapper that imports `ironclaw_tool_bridge_rs` (PyO3 `.so`) and delegates. Keep `ToolBridgeResult`, `SANDBOXED_TOOL_NAMES`, `execute_tool_via_ironclaw`, `should_sandbox_tool`, `get_or_create_session`, `close_session`, `close_all_sessions` as the public API. |
| [`hermes-agent/agent/improvement_audit.py`](../hermes-agent/agent/improvement_audit.py) | Replace `sha256_hex()` and `record_write_event()` with calls into `ironclaw_audit_py`. Keep `SelfImprovementEvent` dataclass as a Python-side DTO for backward compatibility with existing callers. `AuditWriter` delegates all DB operations to the Rust crate. |
| [`hermes-agent/agent/improvement_rollback.py`](../hermes-agent/agent/improvement_rollback.py) | Replace `RollbackManager` with a thin wrapper around `ironclaw_self_improve_dispatcher.PyRollbackManager`. Keep the same `snapshot_skill()`, `commit()`, `rollback()` API. |
| [`hermes-agent/hdc_dsv_server.py`](../hermes-agent/hdc_dsv_server.py) | Deprecate. Add a shim that prints a deprecation warning and exec's the Rust binary `ironclaw-hdc-server` if available, otherwise falls back to the Python implementation. |
| [`ironclaw/Dockerfile.self-improve`](../ironclaw/Dockerfile.self-improve) | Stage 1: build all four new Rust crates. Stage 2: copy only the `.so` files and the `ironclaw-hdc-server` binary into the final image. Remove `hdc_dsv_server.py` from the container image. Strip Python stdlib modules not needed by the review fork (see Dockerfile hardening section). |
| [`ironclaw/.env.example`](../ironclaw/.env.example) | Add `IRONCLAW_HDC_SERVER_TOKEN` (required for `/v1/train` auth). |

---

## PyO3 Binding Strategy

Each Rust crate that needs to be called from Python is compiled as a `cdylib` with PyO3. The resulting `.so` file is installed into the Hermes agent's Python environment via `maturin` (or copied directly in the Dockerfile).

### Build approach

```toml
# In ironclaw/crates/ironclaw_self_improve_dispatcher/Cargo.toml
[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
pyo3 = { version = "0.22", features = ["extension-module"] }
```

```bash
# Build all PyO3 extension modules
cd ironclaw
cargo build --release -p ironclaw_self_improve_dispatcher
cargo build --release -p ironclaw_tool_bridge_rs
cargo build --release -p ironclaw_audit_py

# Install into Hermes virtualenv
cp target/release/libironclaw_self_improve_dispatcher.so \
   ../hermes-agent/.venv/lib/python3.*/site-packages/ironclaw_self_improve_dispatcher.so
# (repeat for other crates)
```

### Python wrapper pattern

Every Python file that wraps a Rust crate follows this pattern:

```python
# hermes-agent/agent/improvement_dispatcher.py (after rewrite)
try:
    from ironclaw_self_improve_dispatcher import (
        trigger_self_improvement_py as trigger_self_improvement,
        trigger_self_improvement_async_py as trigger_self_improvement_async,
        should_use_ironclaw_py as should_use_ironclaw,
    )
    _RUST_BACKEND = True
except ImportError:
    # Rust extension not built -- fall back to pure-Python implementation.
    # Log a security warning: the Python fallback has known vulnerabilities.
    import logging
    logging.getLogger(__name__).critical(
        "ironclaw_self_improve_dispatcher Rust extension not found. "
        "Running with the Python fallback which has known security vulnerabilities. "
        "Build the Rust crates with: cd ironclaw && cargo build --release"
    )
    from agent._improvement_dispatcher_py import (  # renamed original
        trigger_self_improvement,
        trigger_self_improvement_async,
        should_use_ironclaw,
    )
    _RUST_BACKEND = False
```

The original Python files are renamed to `_improvement_dispatcher_py.py`, `_ironclaw_tool_bridge_py.py`, etc. and kept as fallbacks during the transition period. They are removed in Phase 5 cleanup.

### PyO3 type mapping

| Python type | Rust type | Notes |
|-------------|-----------|-------|
| `str` | `&str` / `String` | Zero-copy for `&str` inputs |
| `dict` | `serde_json::Value` via `pyo3-serde` | Typed deserialization at the boundary |
| `Optional[str]` | `Option<String>` | |
| `bool` | `bool` | |
| `None` return | `()` | |
| Agent object | `PyAgentInfo` struct | Extract only the fields we need (provider, model, session_id, base_url) at the Python/Rust boundary — no `getattr` inside Rust |

---

## Dockerfile Hardening

The [`ironclaw/Dockerfile.self-improve`](../ironclaw/Dockerfile.self-improve) is updated to strip the Python attack surface inside the sandbox container:

```dockerfile
# Stage 3: Final hardened image (additions to existing Dockerfile.self-improve)

# Copy Rust extension modules into the Python environment
COPY --from=rust-build /app/target/release/libironclaw_self_improve_dispatcher.so \
    /hermes-venv/lib/python3.12/site-packages/ironclaw_self_improve_dispatcher.so
COPY --from=rust-build /app/target/release/libironclaw_tool_bridge_rs.so \
    /hermes-venv/lib/python3.12/site-packages/ironclaw_tool_bridge_rs.so
COPY --from=rust-build /app/target/release/libironclaw_audit_py.so \
    /hermes-venv/lib/python3.12/site-packages/ironclaw_audit_py.so

# Remove high-risk Python stdlib modules from the container image.
# These are not needed by the review fork and reduce the attack surface.
RUN find /hermes-venv/lib/python3.* -name "pickle.py" -delete && \
    find /hermes-venv/lib/python3.* -name "_pickle*.so" -delete && \
    find /hermes-venv/lib/python3.* -name "shelve.py" -delete && \
    find /usr/lib/python3.* -name "pickle.py" -delete && \
    find /usr/lib/python3.* -name "_pickle*.so" -delete

# Remove the Python HDC server (replaced by Rust binary)
RUN rm -f /app/hermes-agent/hdc_dsv_server.py

# Verify the Rust extensions are importable before finalizing the image
RUN python3 -c "import ironclaw_self_improve_dispatcher; import ironclaw_tool_bridge_rs"
```

**Why remove `pickle`:** The `hdc_model.bin` file previously used Python pickle serialization. The Rust `ironclaw_hdc_server` uses `bincode` instead. Removing `pickle` from the container image prevents any code path — including a compromised LLM output that reaches the Python review fork — from triggering pickle deserialization RCE.

---

## Migration Path

The migration is designed to be incremental — each phase is independently deployable and testable.

```mermaid
graph LR
    P1[Phase 1: Dispatcher + Rollback Rust crate] --> P2[Phase 2: Tool Bridge Rust crate]
    P2 --> P3[Phase 3: Audit PyO3 bindings]
    P3 --> P4[Phase 4: HDC Server Rust binary]
    P4 --> P5[Phase 5: Dockerfile hardening + cleanup]
```

### Phase 1 — Dispatcher + Rollback (`ironclaw_self_improve_dispatcher`)

**Goal:** Replace the highest-risk component — the host-side dispatcher that handles AES-256-GCM encryption and submits jobs to the orchestrator.

**Steps:**
1. Create `ironclaw/crates/ironclaw_self_improve_dispatcher/` with all source files listed above
2. Add to workspace `members` in [`ironclaw/Cargo.toml`](../ironclaw/Cargo.toml)
3. Rename [`hermes-agent/agent/improvement_dispatcher.py`](../hermes-agent/agent/improvement_dispatcher.py) to `hermes-agent/agent/_improvement_dispatcher_py.py`
4. Rename [`hermes-agent/agent/improvement_rollback.py`](../hermes-agent/agent/improvement_rollback.py) to `hermes-agent/agent/_improvement_rollback_py.py`
5. Write new thin-wrapper `hermes-agent/agent/improvement_dispatcher.py` (imports Rust, falls back to `_improvement_dispatcher_py`)
6. Write new thin-wrapper `hermes-agent/agent/improvement_rollback.py`
7. Add `maturin` build step to `hermes-agent/pyproject.toml` or Dockerfile
8. Write Rust unit tests: `ironclaw/tests/self_improvement_dispatcher_rs.rs`
9. Confirm existing Python tests still pass: `hermes-agent/tests/test_improvement_dispatcher.py`

**Verification:** Run existing Python test suite — all tests pass because the public API is unchanged.

### Phase 2 — Tool Bridge (`ironclaw_tool_bridge_rs`)

**Goal:** Replace the fail-closed tool bridge so the sandboxed-tool guarantee cannot be bypassed by a Python import error.

**Steps:**
1. Create `ironclaw/crates/ironclaw_tool_bridge_rs/` with all source files listed above
2. Add to workspace `members`
3. Rename [`hermes-agent/agent/ironclaw_tool_bridge.py`](../hermes-agent/agent/ironclaw_tool_bridge.py) to `hermes-agent/agent/_ironclaw_tool_bridge_py.py`
4. Write new thin-wrapper `hermes-agent/agent/ironclaw_tool_bridge.py`
5. Write Rust unit tests: `ironclaw/tests/tool_bridge_rs.rs`
6. Verify existing `tool_executor.py` integration: `ToolBridgeResult.blocked=True` (not `fallback=True`) when orchestrator is unreachable for all sandboxed tool names

**Verification:** Confirm fail-closed semantics hold for `terminal`, `write_file`, `patch`, `memory`, `skill_manage`, `browser_*`, `mcp__*`.

### Phase 3 — Audit PyO3 bindings (`ironclaw_audit_py`)

**Goal:** Move `sha256_hex()` and `record_write_event()` into Rust so the audit trail cannot be tampered with by Python monkey-patching.

**Steps:**
1. Create `ironclaw/crates/ironclaw_audit_py/` with PyO3 bindings to existing [`LibSqlAuditRepository`](../ironclaw/src/db/libsql/self_improvement_audit.rs)
2. Update [`hermes-agent/agent/improvement_audit.py`](../hermes-agent/agent/improvement_audit.py) to delegate `sha256_hex` and `record_write_event` to Rust
3. Keep `SelfImprovementEvent` dataclass as a Python DTO (no logic, just data)
4. Write tests: verify SHA-256 output matches between Python `hashlib.sha256` and Rust `sha2::Sha256`

### Phase 4 — HDC Server (`ironclaw_hdc_server`)

**Goal:** Replace the FastAPI Python server with a Rust Axum binary that requires authentication on write endpoints and uses `bincode` instead of pickle.

**Steps:**
1. Create `ironclaw/crates/ironclaw_hdc_server/` with all source files listed above
2. Add `[[bin]]` target to workspace
3. Add `IRONCLAW_HDC_SERVER_TOKEN` to [`ironclaw/.env.example`](../ironclaw/.env.example) and documentation
4. Write migration script `ironclaw/scripts/migrate_hdc_model.py` — reads old `hdc_model.bin` (Python pickle) and writes new `hdc_model.bin` (bincode). Run once on upgrade.
5. Update [`plans/IMPLEMENTATION_README.md`](IMPLEMENTATION_README.md) Quick Start section to use `ironclaw-hdc-server` binary instead of `python hermes-agent/hdc_dsv_server.py`
6. Write Rust tests: `ironclaw/tests/hdc_server_rs.rs`

### Phase 5 — Dockerfile hardening + cleanup

**Goal:** Remove Python fallback files and harden the container image.

**Steps:**
1. Update [`ironclaw/Dockerfile.self-improve`](../ironclaw/Dockerfile.self-improve) with Rust extension copy steps and `pickle` removal (see Dockerfile Hardening section above)
2. Delete `hermes-agent/agent/_improvement_dispatcher_py.py` (Python fallback no longer needed)
3. Delete `hermes-agent/agent/_ironclaw_tool_bridge_py.py`
4. Delete [`hermes-agent/hdc_dsv_server.py`](../hermes-agent/hdc_dsv_server.py) (replaced by Rust binary)
5. Update `hermes-agent/pyproject.toml` to remove `cryptography` and `fastapi` from required dependencies (now only needed as optional fallbacks during transition)
6. Run full integration test suite

---

## New Test Files

| File | What It Tests |
|------|--------------|
| `ironclaw/tests/self_improvement_dispatcher_rs.rs` | AES-256-GCM encryption (no plaintext fallback), LLM client resolution (typed enum), orchestrator HTTP client (wiremock), snapshot serialization, rollback manager (commit/rollback/idempotency) |
| `ironclaw/tests/tool_bridge_rs.rs` | Sandboxed tool set (compile-time frozen), fail-closed semantics (blocked not fallback when orchestrator unreachable), session lifecycle (create/reuse/close), concurrent tool calls (no race on job creation) |
| `ironclaw/tests/audit_py_bindings.rs` | SHA-256 output matches `hashlib.sha256`, `record_write_event` inserts correct row, `mark_committed`/`mark_rolled_back` transitions |
| `ironclaw/tests/hdc_server_rs.rs` | Bearer token auth (401 without token, 200 with), model scoring, online training, `bincode` save/load (no pickle), loopback-only binding, graceful shutdown |
| `hermes-agent/tests/test_improvement_dispatcher.py` | Existing tests — must pass unchanged (public API compatibility) |
| `hermes-agent/tests/test_improvement_audit.py` | Existing tests — must pass unchanged |
| `hermes-agent/tests/test_hdc_dsv_server.py` | Existing tests — must pass unchanged (Python fallback path) |

---

## Security Properties Gained vs Current State

```
Component                    Before (Python)              After (Rust)
---------------------------  ---------------------------  ---------------------------
Encryption                   AES-256-GCM OR base64        AES-256-GCM only (hard dep)
Key material in memory       Python GC, not zeroed        zeroize::Zeroize on drop
Deserialization              json.loads + pickle risk     serde typed deserialization
Fail-closed guarantee        Python import can fail       Rust .so crash = hard fail
Sandboxed tool set           Runtime frozenset            Compile-time phf::Set
HDC model file format        Python pickle (RCE on load)  bincode (typed schema)
HDC server auth              None on /v1/train            Bearer token, constant-time
SHA-256 hashing              Python hashlib (patchable)   sha2 crate (not patchable)
Regex engine                 Python re (backtracking)     Rust regex (linear NFA)
Thread safety                Python GIL                   Arc<Mutex<>> explicit
```

---

## Dependencies to Add to `ironclaw/Cargo.toml` Workspace

```toml
# New workspace-level optional dependencies
pyo3       = { version = "0.22", features = ["extension-module"], optional = true }
aes-gcm    = "0.10"
zeroize    = { version = "1", features = ["derive"] }
subtle     = "2"
bincode    = "1"
dashmap    = "6"
once_cell  = "1"
phf        = { version = "0.11", features = ["macros"] }
axum       = { version = "0.7", features = ["json"] }
tower-http = { version = "0.5", features = ["auth"] }
```

Most of these are already present in the workspace for other crates (`serde`, `serde_json`, `reqwest`, `tokio`, `thiserror`, `tracing`, `sha2`). Only `pyo3`, `aes-gcm`, `zeroize`, `subtle`, `bincode`, `dashmap`, `phf`, and `axum` are new additions.

---

## Summary: File Map

### New Rust crate files

```
ironclaw/crates/
  ironclaw_self_improve_dispatcher/
    Cargo.toml
    src/lib.rs
    src/types.rs
    src/config.rs
    src/crypto.rs
    src/llm_resolver.rs
    src/orchestrator_client.rs
    src/snapshot.rs
    src/rollback.rs
    src/dispatcher.rs
    src/pyo3_bindings.rs

  ironclaw_tool_bridge_rs/
    Cargo.toml
    src/lib.rs
    src/types.rs
    src/session.rs
    src/registry.rs
    src/policy.rs
    src/pyo3_bindings.rs

  ironclaw_audit_py/
    Cargo.toml
    src/lib.rs

  ironclaw_hdc_server/
    Cargo.toml
    src/main.rs
    src/model.rs
    src/auth.rs
    src/handlers.rs
    src/types.rs
```

### New Rust test files

```
ironclaw/tests/
  self_improvement_dispatcher_rs.rs
  tool_bridge_rs.rs
  audit_py_bindings.rs
  hdc_server_rs.rs
```

### New utility script

```
ironclaw/scripts/migrate_hdc_model.py   (one-time pickle -> bincode migration)
```

### Modified Python files (thin wrappers)

```
hermes-agent/agent/improvement_dispatcher.py   (delegates to Rust)
hermes-agent/agent/ironclaw_tool_bridge.py     (delegates to Rust)
hermes-agent/agent/improvement_audit.py        (delegates sha256 + DB ops to Rust)
hermes-agent/agent/improvement_rollback.py     (delegates to Rust)
hermes-agent/hdc_dsv_server.py                 (deprecation shim -> Rust binary)
```

### Renamed Python files (kept as fallbacks during transition, deleted in Phase 5)

```
hermes-agent/agent/_improvement_dispatcher_py.py
hermes-agent/agent/_improvement_rollback_py.py
hermes-agent/agent/_ironclaw_tool_bridge_py.py
```

### Modified config/build files

```
ironclaw/Cargo.toml              (add 4 new workspace members)
ironclaw/.env.example            (add IRONCLAW_HDC_SERVER_TOKEN)
ironclaw/Dockerfile.self-improve (copy .so files, remove pickle, verify imports)
hermes-agent/pyproject.toml      (cryptography + fastapi become optional)
plans/IMPLEMENTATION_README.md   (update Quick Start for Rust HDC server)
```
