use crate::api::error::Error;
use crate::api::validation::ValidatedJson;
use crate::order;
use axum::extract::Path;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use utoipa::{OpenApi, ToSchema};
use validify::{Payload, Validify};

#[derive(Debug, Deserialize, Validify, Payload, ToSchema)]
pub struct PlaceOrderRequest {
    pub client_id: order::ClientId,
    pub side: order::Side,
    #[validate(range(min = 1.0))]
    pub price: order::Price,
    #[validate(range(min = 1.0))]
    pub volume: order::Volume,
}

impl From<PlaceOrderRequest> for order::Order {
    fn from(value: PlaceOrderRequest) -> Self {
        order::Order::new(0, value.client_id, value.side, value.price, value.volume)
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(place_order, cancel_order, order_by_client_id),
    components(schemas())
)]
pub struct OrdersApi;

pub fn router() -> Router {
    Router::new()
        .route("/orders", post(place_order))
        .route("/orders/{client_id}", delete(cancel_order))
        .route("/orders/{client_id}", get(order_by_client_id))
}

/// Place a new order
#[utoipa::path(
    post,
    path = "/orders",
    request_body = PlaceOrderRequest,
    responses(
        (status = 200, description = "Order placed", body = order::Order),
        (status = 400, description = "Bad request or validation error"),
        (status = 500, description = "Internal error"),
    )
)]
async fn place_order(
    ValidatedJson(order): ValidatedJson<PlaceOrderRequest>,
) -> Result<Json<order::Order>, Error> {
    Ok(Json(order.into()))
}

/// Cancel an order by client id
#[utoipa::path(
    delete,
    path = "/orders/{client_id}",
    params(
        ("client_id" = String, Path, description = "Client assigned identifier"),
    ),
    responses(
        (status = 200, description = "Order canceled", body = order::Order),
        (status = 404, description = "Order not found"),
        (status = 400, description = "Bad request"),
    )
)]
async fn cancel_order(Path(client_id): Path<order::ClientId>) -> Result<Json<order::Order>, Error> {
    Err(order::book::Error::OrderNotFound(client_id).into())
}

/// Get an order by client id
#[utoipa::path(
    get,
    path = "/orders/{client_id}",
    params(
        ("client_id" = String, Path, description = "Client assigned identifier"),
    ),
    responses(
        (status = 200, description = "Order returned", body = order::Order),
        (status = 404, description = "Order not found"),
    )
)]
async fn order_by_client_id(
    Path(client_id): Path<order::ClientId>,
) -> Result<Json<order::Order>, Error> {
    Err(Error::Internal(Box::new(
        order::book::Error::OrderNotFound(client_id),
    )))
}
