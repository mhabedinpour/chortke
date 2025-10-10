use crate::api::layers::{MakeRequestUuid, REQUEST_ID_HEADER, cors, tracing};
use crate::config;
use axum::Router;
use axum::routing::get;
use metrics_exporter_prometheus::PrometheusBuilder;
use std::net::SocketAddr;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tower_http::request_id::{PropagateRequestIdLayer, SetRequestIdLayer};

mod error;
pub mod layers;
mod orders;
mod utils;
mod validation;

#[derive(Error, Debug)]
pub enum ApiError {
    #[error("Failed to setup Prometheus recorder: {0}")]
    PrometheusSetup(#[from] metrics_exporter_prometheus::BuildError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub async fn start(
    cfg: &config::ApiConfig,
    cancellation_token: CancellationToken,
) -> Result<(), ApiError> {
    let api_router = Router::new().merge(orders::router());

    let prom_handle = PrometheusBuilder::new().install_recorder()?;
    let app = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(|| async move { prom_handle.render() }))
        .nest("/api/v1", api_router)
        .layer(cors())
        .layer(axum_metrics::MetricLayer::default())
        .layer(tracing())
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER.clone()))
        .layer(SetRequestIdLayer::new(
            REQUEST_ID_HEADER.clone(),
            MakeRequestUuid,
        ));

    let listener = tokio::net::TcpListener::bind((cfg.host.clone(), cfg.port)).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        cancellation_token.cancelled().await;
    })
    .await?;

    Ok(())
}

async fn health() -> &'static str {
    // TODO
    "ok"
}
