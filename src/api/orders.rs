use crate::api::error::Error;
use crate::api::validation::ValidatedJson;
use crate::order;
use axum::extract::Path;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use validify::{Payload, Validify};

#[derive(Debug, Deserialize, Validify, Payload)]
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

pub fn router() -> Router {
    Router::new()
        .route("/orders", post(place_order))
        .route("/orders/{client_id}", delete(cancel_order))
        .route("/orders/{client_id}", get(order_by_client_id))
}

async fn place_order(
    ValidatedJson(order): ValidatedJson<PlaceOrderRequest>,
) -> Result<Json<order::Order>, Error> {
    Ok(Json(order.into()))
}

async fn cancel_order(Path(client_id): Path<order::ClientId>) -> Result<Json<order::Order>, Error> {
    Err(order::book::Error::OrderNotFound(client_id).into())
}

async fn order_by_client_id(
    Path(client_id): Path<order::ClientId>,
) -> Result<Json<order::Order>, Error> {
    Err(Error::Internal(Box::new(
        order::book::Error::OrderNotFound(client_id),
    )))
}
