//! Common HTTP middleware layers used by the API service.

use crate::api::utils;
use axum::body::Body;
use axum::response::Response;
use http::{HeaderName, Request};
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestId, RequestId};
use tower_http::trace::{
    DefaultOnBodyChunk, DefaultOnEos, DefaultOnFailure, DefaultOnRequest, HttpMakeClassifier,
    TraceLayer,
};
use tracing::{Level, Span};
use uuid::Uuid;

/// Create a permissive CORS layer allowing any origin and HTTP method.
pub fn cors() -> CorsLayer {
    CorsLayer::new().allow_methods(Any {}).allow_origin(Any {})
}

/// Header used to propagate a request-id across services.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Generator of UUIDv4 request IDs used by SetRequestIdLayer.
#[derive(Clone, Default)]
pub struct MakeRequestUuid;

impl MakeRequestId for MakeRequestUuid {
    fn make_request_id<B>(&mut self, _: &Request<B>) -> Option<RequestId> {
        let id = Uuid::new_v4().to_string();
        Some(RequestId::from(http::HeaderValue::from_str(&id).ok()?))
    }
}

/// Configure request/response tracing with structured spans and logs.
#[allow(clippy::type_complexity)]
pub fn tracing() -> TraceLayer<
    HttpMakeClassifier,
    impl Fn(&Request<Body>) -> Span + Clone,
    DefaultOnRequest,
    impl Fn(&Response<Body>, Duration, &Span) + Clone,
> {
    TraceLayer::new_for_http()
        .make_span_with(|req: &Request<Body>| {
            let id = req
                .headers()
                .get(&REQUEST_ID_HEADER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-");

            tracing::span!(
                Level::INFO,
                "request",
                method = ?req.method(),
                uri = %req.uri(),
                id,
                ip = utils::remote_ip(req)
            )
        })
        .on_request(DefaultOnRequest::new().level(Level::INFO))
        .on_response(
            |res: &http::Response<Body>, latency: Duration, _span: &Span| {
                tracing::info!(
                    status = %res.status(),
                    latency=%latency.as_millis(),
                    "finished processing request"
                );
            },
        )
        .on_body_chunk(DefaultOnBodyChunk::new())
        .on_eos(DefaultOnEos::new().level(Level::DEBUG))
        .on_failure(DefaultOnFailure::new().level(Level::ERROR))
}
