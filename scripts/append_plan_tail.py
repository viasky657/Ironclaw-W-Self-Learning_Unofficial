#!/usr/bin/env python3
"""Append the tail section to plans/rust-self-improvement-rewrite.md."""

import pathlib

PLAN_PATH = pathlib.Path("plans/rust-self-improvement-rewrite.md")

TAIL = """reuse/close), concurrent tool calls (no race on job creation) |
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
pyo3          = { version = "0.22", features = ["extension-module"], optional = true }
aes-gcm       = "0.10"
zeroize       = { version = "1", features = ["derive"] }
subtle        = "2"
bincode       = "1"
dashmap       = "6"
once_cell     = "1"
phf           = { version = "0.11", features = ["macros"] }
axum          = { version = "0.7", features = ["json"] }
tower-http    = { version = "0.5", features = ["auth"] }
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
"""

current = PLAN_PATH.read_text(encoding="utf-8")

# Find the truncation point — the last complete line
last_pipe = current.rfind("| `ironclaw/tests/tool_bridge_rs.rs`")
if last_pipe == -1:
    # File ends mid-table-row; find the last newline
    truncation = current.rfind("\n")
    base = current[:truncation + 1]
else:
    # Keep everything up to and including that row, then append
    end_of_row = current.find("\n", last_pipe)
    base = current[:end_of_row + 1]

combined = base + TAIL
PLAN_PATH.write_text(combined, encoding="utf-8")
