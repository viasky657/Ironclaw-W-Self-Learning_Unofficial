//! WASM Tool Bridge for Hermes Agent self-improvement sandbox.
//!
//! This crate implements the IronClaw tool interface for the two tools that
//! are allowed inside a self-improvement sandbox container:
//!
//! - [`SkillManageTool`]: Create/update agent-created skills only.
//!   Enforces ownership check (`agent_created=true`), content policy,
//!   size limits, and rate limiting.
//!
//! - [`MemoryProxyTool`]: Proxy memory writes to the orchestrator host.
//!   The container never directly touches the memory backend — all writes
//!   go through `POST /orchestrator/memory-write` on the trusted host.
//!
//! ## Security Properties
//!
//! - **Ownership check**: only skills tagged `agent_created=true` can be modified
//! - **Content policy**: runs safety checks on every write payload before committing
//! - **Size limits**: max 64 KB per skill file, max 256 KB total per job
//! - **Rate limiting**: max 10 skill writes per job, max 5 memory writes per job
//! - **Memory isolation**: container never receives memory provider credentials
//! - **Tool allowlist**: any tool call not in `[skill_manage, memory]` is rejected
//!
//! ## Compilation Targets
//!
//! - `wasm32-wasip1`: runs inside wasmtime (in-process WASM sandbox, local mode)
//! - Native: runs in Docker container (cloud mode)

pub mod memory_proxy;
pub mod policy;
pub mod rate_limiter;
pub mod skill_manage;
pub mod types;

pub use memory_proxy::MemoryProxyTool;
pub use policy::{ContentPolicy, PolicyVerdict};
pub use rate_limiter::RateLimiter;
pub use skill_manage::SkillManageTool;
pub use types::{BridgeConfig, BridgeError, ToolCall, ToolResult, WritePayload};
