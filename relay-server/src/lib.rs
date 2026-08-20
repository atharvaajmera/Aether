//! Relay/signaling server library.
//!
//! Exposes [`build_router`] so both the `main.rs` binary and integration
//! tests (including the root `plenum` crate's dev-dependency tests) can
//! stand up the exact same axum app in-process.

pub mod state;
pub mod turn_creds;
pub mod ws;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;

pub use state::AppState;

/// Builds the axum [`Router`] for the relay server, wired up with the given
/// shared [`AppState`].
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/ws", get(ws::ws_handler))
        .route(
            "/turn-credentials",
            get(turn_creds::turn_credentials_handler),
        )
        .route("/healthz", get(healthz))
        .route("/room/:session_id", get(room_exists))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "OK"
}

async fn room_exists(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> axum::http::StatusCode {
    if state.room_exists(&session_id) {
        axum::http::StatusCode::NO_CONTENT
    } else {
        axum::http::StatusCode::NOT_FOUND
    }
}
