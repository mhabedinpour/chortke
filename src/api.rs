use crate::config;
use axum::routing::get;
use axum::Router;
use http::{HeaderName, Request};
use std::io;
use std::time::Duration;
use axum_prometheus::PrometheusMetricLayer;
use tokio::select;
use tokio::signal::unix::signal;
use tower::ServiceBuilder;
use tower_http::request_id::RequestId;
use tower_http::{
    request_id::{MakeRequestId, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::{DefaultOnBodyChunk, DefaultOnEos, DefaultOnFailure, DefaultOnRequest, TraceLayer},
};
use tracing::{info, Level};
use uuid::Uuid;

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone, Default)]
struct MakeRequestUuid;
impl MakeRequestId for MakeRequestUuid {
    fn make_request_id<B>(&mut self, _: &Request<B>) -> Option<RequestId> {
        let id = Uuid::new_v4().to_string();
        Some(RequestId::from(http::HeaderValue::from_str(&id).ok()?))
    }
}

pub async fn start(cfg: &config::ApiConfig) -> io::Result<()> {
    let middleware = ServiceBuilder::new()
        // set a request id if missing
        .layer(SetRequestIdLayer::new(REQUEST_ID_HEADER.clone(), MakeRequestUuid))
        // make sure the id flows back in the response
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER.clone()))
        // structured request/response logs
        .layer(
            TraceLayer::new_for_http()
                // make sure the id flows back in the response
                // include request-id in the span
                .make_span_with(|req: &Request<axum::body::Body>| {
                    let id = req.headers().get(&REQUEST_ID_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("-");
                    tracing::span!(Level::INFO, "request", method = ?req.method(), uri = %req.uri(), id)
                })
                .on_request(DefaultOnRequest::new().level(Level::INFO))
                .on_response(|res: &http::Response<_>, latency: Duration, _span: &tracing::Span| {
                    info!(
                        status = %res.status(),
                        latency=%latency.as_millis(),
                        "finished processing request"
                    );
                })
                .on_body_chunk(DefaultOnBodyChunk::new())
                .on_eos(DefaultOnEos::new().level(Level::DEBUG))
                .on_failure(DefaultOnFailure::new().level(Level::ERROR)),
        )
        .into_inner();
    let (prometheus_layer, metric_handle) = PrometheusMetricLayer::pair();
    let app = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(|| async move { metric_handle.render() }))
        .layer(middleware)
        .layer(prometheus_layer);

    let listener = tokio::net::TcpListener::bind((cfg.host.clone(), cfg.port)).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

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
