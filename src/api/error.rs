//! Error types and conversions used by the public API layer.
//!
//! Provides a lightweight Error enum that maps application errors into
//! HTTP responses with a consistent JSON body shape.

use crate::order;
use axum::Json;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use tracing::{Level, enabled, error};
use validify::ValidationErrors;

/// Machine-readable error code used in API responses.
pub type Code = String;
/// Human-readable error message used in API responses.
pub type Message = String;

/// API error which can be converted into an HTTP response.
#[derive(Debug)]
pub enum Error {
    /// Resource not found. Returns 404.
    NotFound(Code, Message),
    /// Client error. Returns 400.
    BadRequest(Code, Message),
    /// Validation error containing field-level errors. Returns 400 with structured payload.
    Validation(ValidationErrors),
    /// Unexpected internal error. Returns 500.
    Internal(Box<dyn std::error::Error>),
}

/// Convert domain-level order book errors into API errors.
impl From<order::book::Error> for Error {
    fn from(value: order::book::Error) -> Self {
        match value {
            order::book::Error::OrderNotFound(client_id) => Error::NotFound(
                "ORDER_NOT_FOUND".into(),
                format!("order with client_id {} not found", client_id),
            ),
            order::book::Error::OrderExists(client_id) => Error::BadRequest(
                "ORDER_ALREADY_EXISTS".into(),
                format!("order with client_id {} already exists", client_id),
            ),
        }
    }
}

impl IntoResponse for Error {
    /// Convert Error into an Axum Response with JSON body of shape:
    /// { "error": { "code": <code>, "message"?: <message>, "errors"?: <validation> } }
    fn into_response(self) -> Response {
        let (status, code, msg) = match self {
            Error::NotFound(code, msg) => (StatusCode::NOT_FOUND, code, msg),
            Error::BadRequest(code, msg) => (StatusCode::BAD_REQUEST, code, msg),
            Error::Validation(validation_errors) => {
                let body = Json(serde_json::json!({
                    "error": { "code": "VALIDATION_ERROR", "errors": validation_errors }
                }));

                return (StatusCode::BAD_REQUEST, body).into_response();
            }
            Error::Internal(err) => {
                error!("internal error: {}", err);

                match enabled!(Level::DEBUG) {
                    true => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "INTERNAL_ERROR".into(),
                        err.to_string(),
                    ),
                    false => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "INTERNAL_ERROR".into(),
                        "an internal error happened during processing your request".into(),
                    ),
                }
            }
        };

        let body = Json(serde_json::json!({
            "error": { "code": code, "message": msg }
        }));

        (status, body).into_response()
    }
}

