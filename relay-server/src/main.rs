//! Thin binary entry point for the relay/signaling server.
//!
//! Configuration is read entirely from environment variables (see README.md):
//!   - `RELAY_BIND_ADDR`   socket address to bind to (default `0.0.0.0:8080`)
//!   - `RELAY_PUBLIC_URL`  externally-reachable URL, logging only
//!   - `TURN_SHARED_SECRET` coturn `use-auth-secret` shared secret; if unset,
//!     `/turn-credentials` responds 404 (TURN not configured)
//!   - `TURN_URLS`         comma-separated list of TURN server URLs handed
//!     back to clients alongside minted credentials
//!   - `RUST_LOG`          tracing filter (e.g. `info`, `relay_server=debug`)

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use relay_server::AppState;
use tracing_subscriber::EnvFilter;

/// Self-hosted relay/signaling server for Plenum's internet (NAT-traversal)
/// transfers. All configuration is via environment variables; this CLI only
/// exposes `--help`/`--version`.
#[derive(Parser, Debug)]
#[command(name = "relay-server", version, about)]
struct Cli;

#[tokio::main]
async fn main() {
    let _ = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let bind_addr = std::env::var("RELAY_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let public_url = std::env::var("RELAY_PUBLIC_URL").unwrap_or_else(|_| "(not set)".to_string());
    let turn_secret = std::env::var("TURN_SHARED_SECRET")
        .ok()
        .filter(|s| !s.is_empty());
    let turn_urls: Vec<String> = std::env::var("TURN_URLS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    if turn_secret.is_none() {
        tracing::warn!(
            "TURN_SHARED_SECRET not set; /turn-credentials will respond 404 (STUN-only NAT traversal)"
        );
    }

    let addr: SocketAddr = match bind_addr.parse() {
        Ok(addr) => addr,
        Err(err) => {
            tracing::error!("invalid RELAY_BIND_ADDR {bind_addr:?}: {err}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(AppState::new(turn_secret, turn_urls));
    let app = relay_server::build_router(state);

    tracing::info!("relay-server listening on {addr} (public url: {public_url})");

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!("failed to bind {addr}: {err}");
            std::process::exit(1);
        }
    };

    if let Err(err) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        tracing::error!("server error: {err}");
        std::process::exit(1);
    }
}

async fn shutdown_signal() {
    match tokio::signal::ctrl_c().await {
        Ok(()) => tracing::info!("received shutdown signal"),
        Err(err) => tracing::error!("failed to listen for shutdown signal: {err}"),
    }
}
