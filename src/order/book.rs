//! Order book traits and shared types.
//!
//! This module defines the minimal interface expected from an order book
//! implementation and the common types used to represent market depth.

pub mod tree_map;
pub mod warm_book;

use crate::order::{ClientId, Id, Order, Price, Volume};
use thiserror::Error;

/// Aggregated depth at a single price level.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct DepthItem {
    /// Price level.
    pub price: Price,
    /// Total visible volume at this price level.
    pub volume: Volume,
}

/// A snapshot of the best price levels on both sides of the book.
#[derive(Debug, Clone)]
pub struct Depth {
    /// Best bids in descending price order.
    pub bids: Vec<DepthItem>,
    /// Best asks in ascending price order.
    pub asks: Vec<DepthItem>,
}

/// Generic order-book errors.
#[derive(Error, Debug)]
pub enum Error {
    #[error("could not find order with id #{0}")]
    /// Tried to operate on an order that does not exist.
    OrderIdNotFound(Id),
    #[error("another order with the same id #{0} already exists")]
    /// Tried to add an order with an ID that already exists.
    OrderIdExists(Id),
    #[error("could not find order with client id #{0}")]
    /// Tried to operate on an order that does not exist.
    OrderClientIdNotFound(ClientId),
    #[error("another order with the same client id #{0} already exists")]
    /// Tried to add an order with a client ID that already exists.
    OrderClientIdExists(ClientId),
}

// TODO: add prometheus metrics

/// The core order book interface. Implementors must provide basic operations
/// for adding, canceling, obtaining depth, and matching orders.
pub trait HotBook {
    /// Add a new order to the book. Returns an error if the ID already exists.
    fn add(&mut self, order: Order) -> Result<(), Error>;
    /// Cancel an existing order by its ID.
    fn cancel(&mut self, id: Id) -> Result<Order, Error>;
    /// Returns a depth snapshot for the requested number of price levels per side.
    fn depth(&self, limit: usize) -> Depth;
    /// Matches orders until no more crossing prices remain, returning generated trades and closed orders.
    fn match_orders(&mut self) -> (Vec<crate::trade::Trade>, Vec<Order>);
    // Gets an order by its ID.
    fn lookup(&self, id: Id) -> Option<&Order>;
}
