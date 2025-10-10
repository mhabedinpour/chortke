use crate::order;
use axum::Json;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use tracing::{Level, enabled, error};
use validify::ValidationErrors;

pub type Code = String;
pub type Message = String;

#[derive(Debug)]
pub enum Error {
    NotFound(Code, Message),
    BadRequest(Code, Message),
    Validation(ValidationErrors),
    Internal(Box<dyn std::error::Error>),
}

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

