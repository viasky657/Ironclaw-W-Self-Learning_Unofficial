//! HDC DSV (Hyperdimensional Computing Distributed Sparse Vector) adapter.
//!
//! Provides a quality gate and online learning adapter for the IronClaw
//! self-improvement loop. The HDC DSV model runs locally as a FastAPI server
//! (`hdc_dsv_server.py`) and is accessed via an OpenAI-compatible HTTP API.
//!
//! ## Roles
//!
//! 1. **Quality gate**: Scores each proposed skill/memory write before it is
//!    committed. Writes below `quality_threshold` are flagged or blocked.
//!
//! 2. **Online learner**: After each committed write, sends a training update
//!    to the local HDC DSV server so the model learns from the outcome.
//!
//! ## Bootstrap mode
//!
//! The quality gate is disabled until `bootstrap_min` training examples have
//! been accumulated. Before that threshold, all writes pass through (the gate
//! is a no-op). This prevents the model from blocking writes before it has
//! learned anything meaningful.
//!
//! ## Deployment modes
//!
//! | `SELF_IMPROVE_HDC_ENABLED` | `SELF_IMPROVE_HDC_BLOCK` | Behavior |
//! |---|---|---|
//! | `false` (default) | any | Adapter not loaded; all writes pass |
//! | `true` | `false` | Scores logged but writes not blocked |
//! | `true` | `true` | Writes below threshold are blocked |
//!
//! ## Local-only guarantee
//!
//! The HDC DSV server binds to `127.0.0.1` only. The model state file
//! (`hdc_model.bin`) is stored locally. No telemetry, no cloud sync.

pub mod adapter;
pub mod types;

pub use adapter::HdcDsvAdapter;
pub use types::{HdcConfig, HdcError, HdcVerdict, WriteOutcome, WritePayload};
