//! Trade representation produced by the matching engine.
//!
//! A Trade links a bid and an ask order that were matched, along with the
//! execution price, volume, side that provided liquidity (maker), and a
//! timestamp.

use crate::order;
use time::OffsetDateTime;

/// Unique identifier for trades.
pub type Id = u64;

/// A single execution between a bid and an ask.
#[derive(Debug)]
pub struct Trade {
    /// The resting bid order involved in the trade (or taker if crossed).
    pub bid_order_id: order::Id,
    /// The resting ask order involved in the trade (or taker if crossed).
    pub ask_order_id: order::Id,
    /// Whether the bid side was the maker (i.e., provided resting liquidity).
    pub is_bid_maker: bool,
    /// Execution price of the trade.
    pub price: order::Price,
    /// Executed volume (base quantity) for this trade.
    pub volume: order::Volume,
    /// UTC timestamp when the trade was generated.
    pub timestamp: OffsetDateTime,
}
