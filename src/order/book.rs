//! Order book traits and shared types.
//!
//! This module defines the minimal interface expected from an order book
//! implementation and the common types used to represent market depth.

pub mod tree_map;

use crate::order::{Id, Order, Price, Volume};

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
#[derive(Debug)]
pub enum Error {
    /// Tried to operate on an order that does not exist.
    OrderNotFound,
    /// Tried to add an order with an ID that already exists.
    OrderExists,
}

// TODO: add prometheus metrics
// TODO: think more about error handling

/// The core order book interface. Implementors must provide basic operations
/// for adding, canceling, obtaining depth, and matching orders.
pub trait Book {
    /// Add a new order to the book. Returns an error if the ID already exists.
    fn add(&mut self, order: Order) -> Result<(), Error>;
    /// Cancel an existing order by its ID.
    fn cancel(&mut self, id: Id) -> Result<(), Error>;
    /// Returns a depth snapshot for the requested number of price levels per side.
    fn depth(&self, limit: usize) -> Depth;
    /// Matches orders until no more crossing prices remain, returning generated trades.
    fn match_orders(&mut self) -> Vec<crate::trade::Trade>;
}
