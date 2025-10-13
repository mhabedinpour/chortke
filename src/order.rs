use crate::user;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The side of the order: Bid to buy, Ask to sell.
#[derive(Debug, Copy, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Bid,
    Ask,
}

/// The current lifecycle status of an order.
#[derive(Debug, Copy, Clone, Serialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    /// The order is active on the book and can be matched.
    Open,
    /// The order has been completely executed.
    Executed,
    /// The order was canceled by the client and removed from the book.
    Canceled,
}

/// Unique order identifier.
pub type Id = u64;
/// Arbitrary client-provided identifier.
pub type ClientId = String;
/// Price in integer ticks (no decimals at this layer).
pub type Price = u64; // ticks
/// Quantity of the asset (base units).
pub type Volume = u64;

/// An order submitted by a client to buy or sell a quantity at a limit price.
#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct Order {
    pub id: Id,
    pub user_id: user::Id,
    pub client_id: ClientId,
    pub side: Side,
    /// Limit price for this order, in ticks.
    pub price: Price,
    /// Average execution price if executed; None if never traded.
    pub executed_price: Option<Price>,
    /// Total requested volume when the order was placed.
    pub volume: Volume,
    /// Cumulative executed volume so far.
    pub executed_volume: Volume,
    pub status: Status,
}

impl Order {
    /// Construct a new limit order with the given parameters.
    pub fn new(
        id: Id,
        user_id: user::Id,
        client_id: ClientId,
        side: Side,
        price: Price,
        volume: Volume,
    ) -> Self {
        Order {
            id,
            user_id,
            client_id,
            side,
            price,
            executed_price: None,
            volume,
            executed_volume: 0,
            status: Status::Open,
        }
    }

    /// Returns the unfilled portion of the order's volume.
    pub fn remaining_volume(&self) -> Volume {
        self.volume - self.executed_volume
    }

    /// Returns a unique identifier for this order using its client id and user id.
    pub fn user_client_id(&self) -> String {
        Order::format_user_client_id(&self.user_id, &self.client_id)
    }

    pub fn format_user_client_id(user_id: &user::Id, client_id: &ClientId) -> String {
        format!("{}:{}", user_id, client_id)
    }
}

/// Order book interfaces and implementations.
pub mod book;
