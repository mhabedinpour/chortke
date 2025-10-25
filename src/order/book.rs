//! Order book traits and shared types.
//!
//! This module defines the minimal interface expected from an order book
//! implementation and the common types used to represent market depth.

pub mod tree_map;
pub mod warm_book;

use crate::order::{ClientId, Id, Order, Price, Volume};
use crate::seq;
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
    #[error("another snapshot is already in progress")]
    // Tried to take a new snapshot while another is in progress.
    AnotherSnapshotAlreadyTaken,
    #[error("no snapshot is currently in progress")]
    // Tried to end a snapshot or get a batch when no snapshot is taken.
    NoSnapshotTaken,
    #[error("matcher/book is not ready to process operations yet")]
    // Tried an operation before initializing the book.
    NotReady,
}

// TODO: add prometheus metrics

/// The core order book interface. Implementors must provide basic operations
/// for adding, canceling, obtaining depth, and matching orders.
pub trait HotBook {
    /// Add a new order to the book. Returns an error if the ID already exists.
    fn add(&mut self, order: Order) -> Result<(), Error>;
    /// Cancel an existing order by its ID.
    fn cancel(&mut self, id: Id, seq: seq::Seq) -> Result<Order, Error>;
    /// Returns a depth snapshot for the requested number of price levels per side.
    fn depth(&self, limit: usize) -> Depth;
    /// Matches orders until no more crossing prices remain, returning generated trades and closed orders.
    fn match_orders(&mut self) -> (Vec<crate::trade::Trade>, Vec<Order>);
    // Gets an order by its ID.
    fn lookup(&self, id: Id) -> Option<&Order>;
}

/// Interface for streaming snapshots of the order book state and restoring from them.
///
/// Snapshotting is expected to work in batches so very large books can be
/// serialized without holding all orders in memory at once. A typical usage is:
///
/// - Call `take_snapshot()` to start a new snapshot session. Only one session
///   can be active at a time. Starting another while one is active returns
///   `Error::AnotherSnapshotAlreadyTaken`.
/// - Repeatedly call `snapshot_batch(limit)` until it returns `Ok(None)`, which
///   signals that there are no more orders to stream.
/// - Finally, call `end_snapshot()` to close the session and free any resources.
///
/// Restoring follows the reverse direction: feed batches of orders obtained from
/// a snapshot into `restore_snapshot_batch` until the whole snapshot has been
/// applied.
pub trait SnapshotableBook {
    /// Start a new snapshot session. Fails if another snapshot is already in progress.
    fn take_snapshot(&mut self) -> Result<(), Error>;
    /// Return up to `limit` open orders from the ongoing snapshot.
    ///
    /// When there are no more orders to stream, returns `Ok(None)`. If no
    /// snapshot is currently in progress, returns `Error::NoSnapshotTaken`.
    fn snapshot_batch(&mut self, limit: usize) -> Result<Option<Vec<Order>>, Error>;
    /// End the current snapshot session, releasing any temporary state.
    ///
    /// Returns `Error::NoSnapshotTaken` if called without an active snapshot.
    fn end_snapshot(&mut self) -> Result<(), Error>;

    /// Restore a batch of orders that belong to a previously taken snapshot.
    /// Implementations should accept multiple calls and apply orders idempotently
    /// within the context of the ongoing restore process.
    fn restore_snapshot_batch(&mut self, orders: Vec<Order>) -> Result<(), Error>;
}
