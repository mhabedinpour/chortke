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

#[derive(OpenApi)]
#[openapi(
    info(title = "Matching Engine API", version = "1.0.0"),
    nest(
        (path = "/api/v1", api = orders::OrdersApi)
    )
)]
pub struct ApiDoc;

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
