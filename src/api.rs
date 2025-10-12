//! Public HTTP API for the matching engine.
//!
//! This module wires together the Axum router, middleware layers, and OpenAPI docs
//! to expose the versioned REST API under the "/api/v1" prefix, a Prometheus
//! metrics endpoint under "/metrics", a simple health check under "/health",
//! and Swagger UI under "/docs".

use crate::config;
use axum::Router;
use axum::routing::get;
use metrics_exporter_prometheus::PrometheusBuilder;
use std::net::SocketAddr;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tower_http::request_id::{PropagateRequestIdLayer, SetRequestIdLayer};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

mod error;
pub mod layers;
mod orders;
mod utils;
mod validation;

/// OpenAPI documentation declaration for the public API.
#[derive(OpenApi)]
#[openapi(
    info(title = "Matching Engine API", version = "1.0.0"),
    nest(
        (path = "/api/v1", api = orders::OrdersApi)
    )
)]
pub struct ApiDoc;

/// Errors that may occur while bootstrapping the API service.
#[derive(Error, Debug)]
pub enum ApiError {
    /// Prometheus metrics recorder initialization failed.
    #[error("Failed to setup Prometheus recorder: {0}")]
    PrometheusSetup(#[from] metrics_exporter_prometheus::BuildError),

    /// A generic IO error while binding or serving.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Start the HTTP API server.
///
/// - Binds on cfg.host:cfg.port
/// - Serves Swagger UI at /docs backed by OpenAPI JSON at /api-docs/openapi.json
/// - Exposes health at /health and Prometheus metrics at /metrics
/// - Nests versioned routes under /api/v1
pub async fn start(
    cfg: &config::ApiConfig,
    cancellation_token: CancellationToken,
) -> Result<(), ApiError> {
    let api_router = Router::new().merge(orders::router());

    let prom_handle = PrometheusBuilder::new().install_recorder()?;
    let app = Router::new()
        .merge(SwaggerUi::new("/docs").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/health", get(health))
        .route("/metrics", get(|| async move { prom_handle.render() }))
        .nest("/api/v1", api_router)
        .layer(layers::cors())
        .layer(axum_metrics::MetricLayer::default())
        .layer(layers::tracing())
        .layer(PropagateRequestIdLayer::new(
            layers::REQUEST_ID_HEADER.clone(),
        ))
        .layer(SetRequestIdLayer::new(
            layers::REQUEST_ID_HEADER.clone(),
            layers::MakeRequestUuid,
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
