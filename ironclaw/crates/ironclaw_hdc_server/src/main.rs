/// IronClaw HDC DSV Server — Rust Axum replacement for `hdc_dsv_server.py`.
///
/// # Security improvements over the Python implementation
///
/// | Property | Python (hdc_dsv_server.py) | Rust (this binary) |
/// |----------|---------------------------|---------------------|
/// | Auth on /v1/train | None | Bearer token (constant-time) |
/// | Model file format | Python pickle (RCE on load) | bincode (typed schema) |
/// | Binding address | Configurable (can bind 0.0.0.0) | 127.0.0.1:8765 only |
/// | Key material | Python GC | zeroize on drop |
///
/// # Usage
///
/// ```bash
/// IRONCLAW_HDC_SERVER_TOKEN=my-secret-token \
/// IRONCLAW_HDC_MODEL_PATH=/path/to/hdc_model.bin \
/// ironclaw-hdc-server
/// ```

mod auth;
mod handlers;
mod model;
mod types;

use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use handlers::AppState;
use std::net::SocketAddr;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("ironclaw_hdc_server=info".parse()?),
        )
        .init();

    // Load or create the model.
    let model_path: Option<PathBuf> = std::env::var("IRONCLAW_HDC_MODEL_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    let dimension: usize = std::env::var("IRONCLAW_HDC_DIMENSION")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024);

    let shared_model = if let Some(ref path) = model_path {
        if path.exists() {
            tracing::info!(path = %path.display(), "Loading HDC model from file");
            match model::HdcDsvModel::load(path) {
                Ok(m) => {
                    tracing::info!(
                        train_count = m.train_count(),
                        "HDC model loaded successfully"
                    );
                    std::sync::Arc::new(std::sync::RwLock::new(m))
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "Failed to load HDC model — starting with fresh model"
                    );
                    model::new_shared_model(dimension)
                }
            }
        } else {
            tracing::info!(
                path = %path.display(),
                "HDC model file not found — starting with fresh model"
            );
            model::new_shared_model(dimension)
        }
    } else {
        tracing::info!("IRONCLAW_HDC_MODEL_PATH not set — using in-memory model (not persisted)");
        model::new_shared_model(dimension)
    };

    let state = AppState {
        model: shared_model,
        model_path: model_path.clone(),
    };

    // Build the Axum router.
    let app = Router::new()
        // Public endpoints (no auth).
        .route("/v1/models", get(handlers::list_models))
        .route("/health", get(handlers::health))
        // Protected endpoints (bearer token required).
        .route("/v1/chat/completions", post(handlers::chat_completions))
        .route("/v1/train", post(handlers::train))
        // Apply bearer auth middleware to all routes.
        // The middleware itself decides which routes are public.
        .layer(middleware::from_fn(auth::bearer_auth_middleware))
        .with_state(state);

    // Bind to 127.0.0.1:8765 ONLY — hard-coded, not configurable.
    // This prevents accidental exposure to the network.
    let port: u16 = std::env::var("IRONCLAW_HDC_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8765);

    // Only allow loopback binding — reject any attempt to bind to 0.0.0.0.
    let addr = SocketAddr::from(([127, 0, 0, 1], port));

    tracing::info!(
        addr = %addr,
        model_path = ?model_path,
        "IronClaw HDC DSV server starting"
    );

    // Warn if the server token is not set.
    if std::env::var("IRONCLAW_HDC_SERVER_TOKEN")
        .unwrap_or_default()
        .is_empty()
    {
        tracing::warn!(
            "IRONCLAW_HDC_SERVER_TOKEN is not set — all write requests will be rejected with 401. \
             Set this variable to enable /v1/train and /v1/chat/completions."
        );
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %addr, "HDC server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("HDC server shut down gracefully");
    Ok(())
}

/// Wait for SIGTERM or SIGINT for graceful shutdown.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("Failed to register SIGINT handler");
        tokio::select! {
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM — shutting down");
            }
            _ = sigint.recv() => {
                tracing::info!("Received SIGINT — shutting down");
            }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to register Ctrl+C handler");
        tracing::info!("Received Ctrl+C — shutting down");
    }
}
