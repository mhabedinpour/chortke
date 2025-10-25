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
            order::book::Error::OrderIdNotFound(id) => Error::NotFound(
                "ORDER_NOT_FOUND".into(),
                format!("order with id {} not found", id),
            ),
            order::book::Error::OrderIdExists(id) => Error::BadRequest(
                "ORDER_ALREADY_EXISTS".into(),
                format!("order with id {} already exists", id),
            ),
            order::book::Error::OrderClientIdNotFound(client_id) => Error::NotFound(
                "ORDER_NOT_FOUND".into(),
                format!("order with client id {} not found", client_id),
            ),
            order::book::Error::OrderClientIdExists(client_id) => Error::BadRequest(
                "ORDER_ALREADY_EXISTS".into(),
                format!("order with client id {} already exists", client_id),
            ),
            order::book::Error::AnotherSnapshotAlreadyTaken => Error::Internal(value.into()),
            order::book::Error::NoSnapshotTaken => Error::Internal(value.into()),
            order::book::Error::NotReady => Error::Internal(value.into()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;

    #[test]
    fn maps_order_errors() {
        let e1: Error = order::book::Error::OrderClientIdNotFound("abc".into()).into();
        match e1 {
            Error::NotFound(code, msg) => {
                assert_eq!(code, "ORDER_NOT_FOUND");
                assert!(msg.contains("abc"));
            }
            _ => panic!("expected NotFound"),
        }

        let e2: Error = order::book::Error::OrderClientIdExists("xyz".into()).into();
        match e2 {
            Error::BadRequest(code, msg) => {
                assert_eq!(code, "ORDER_ALREADY_EXISTS");
                assert!(msg.contains("xyz"));
            }
            _ => panic!("expected BadRequest"),
        }
    }

    #[test]
    fn into_response_basic_shapes() {
        // NotFound
        let res = Error::NotFound("NOT".into(), "missing".into()).into_response();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = body_to_json(res);
        assert_eq!(body["error"]["code"], "NOT");
        assert_eq!(body["error"]["message"], "missing");

        // BadRequest
        let res = Error::BadRequest("BAD".into(), "oops".into()).into_response();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = body_to_json(res);
        assert_eq!(body["error"]["code"], "BAD");
        assert_eq!(body["error"]["message"], "oops");

        // Internal (message content may vary with log level)
        let err = std::io::Error::new(std::io::ErrorKind::Other, "boom");
        let res = Error::Internal(Box::new(err)).into_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_to_json(res);
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
        assert!(body["error"]["message"].is_string());
    }

    #[test]
    fn into_response_validation() {
        let ve = ValidationErrors::new();
        // create a fake field error shape via serde-json since constructing real
        // validify errors is cumbersome for unit testing the response shape
        let res = Error::Validation(ve).into_response();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = body_to_json(res);
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
        assert!(body["error"].get("errors").is_some());
    }

    fn body_to_json(res: Response) -> serde_json::Value {
        use axum::body::to_bytes;
        use tokio::runtime::Runtime;

        let rt = Runtime::new().unwrap();
        let bytes = rt.block_on(async move { to_bytes(res.into_body(), 64 * 1024).await.unwrap() });
        serde_json::from_slice(&bytes).unwrap()
    }
}
