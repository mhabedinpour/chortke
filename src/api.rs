use crate::config;
use axum::routing::get;
use axum::Router;
use std::io;
use tokio::select;
use tokio::signal::unix::signal;
use tracing::info;

pub async fn start(cfg: &config::ApiConfig) -> io::Result<()> {
    let app = Router::new().route("/health", get(health));

    let listener = tokio::net::TcpListener::bind((cfg.host.clone(), cfg.port)).await?;
    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut terminate = signal(tokio::signal::unix::SignalKind::terminate()).unwrap();

    select! {
        _ = ctrl_c => { },
        _ = terminate.recv() => {  }
    }

    info!("shutdown signal received, exiting")
}

// TODO: logging, metrics, CORS, request ID