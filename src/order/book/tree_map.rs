//! Order book implementation backed by BTreeMap price levels.
//!
//! This module provides a simple price-time priority limit order book using two
//! BTreeMaps (one for bids descending, one for asks ascending). Each price level
//! maintains a FIFO queue of orders via indices into a Slab, avoiding frequent
//! allocations and allowing O(1) insertion/removal within a level. Matching is
//! performed by crossing the best bid and best ask while prices overlap.

use crate::order::book::{Book, Depth, DepthItem, Error};
use crate::order::{Id, Order, Price, Side, Volume};
use crate::trade::Trade;
use slab::Slab;
use std::cmp;
use std::collections::{BTreeMap, HashMap};
use time::OffsetDateTime;

/// Aggregated state for a single price level.
///
/// Keeps the head/tail of a doubly-linked list of orders (by slab index), as
/// well as cumulative volume and order count for quick depth queries.
#[derive(Debug, Default)]
struct PriceLevel {
    head: Option<usize>,
    tail: Option<usize>,
    total_volume: Volume,
    total_orders: usize,
}

impl PriceLevel {
    /// Append an order node to the back of the level's FIFO queue and update
    /// aggregates. The `order_idx` must reference a valid entry in `orders`.
    fn push(&mut self, orders: &mut Slab<OrderNode>, order_idx: usize) {
        match self.tail {
            Some(tail) => {
                orders[tail].next = Some(order_idx);
                orders[order_idx].prev = Some(tail);
                self.tail = Some(order_idx);
            }
            None => {
                self.head = Some(order_idx);
                self.tail = Some(order_idx);
                orders[order_idx].prev = None;
            }
        }

        self.total_volume += orders[order_idx].order.remaining_volume();
        self.total_orders += 1;
    }

    /// Remove a specific order node from the level's queue and update
    /// aggregates. The node must be currently linked in this level.
    fn remove(&mut self, orders: &mut Slab<OrderNode>, order_idx: usize) {
        let prev = orders[order_idx].prev;
        let next = orders[order_idx].next;

        if let Some(p) = prev {
            orders[p].next = next;
        } else {
            self.head = next;
        }
        if let Some(n) = next {
            orders[n].prev = prev;
        } else {
            self.tail = prev;
        }
        self.total_orders -= 1;
        self.total_volume -= orders[order_idx].order.remaining_volume();
        orders[order_idx].prev = None;
        orders[order_idx].next = None;
    }
}

/// Node representing an individual order stored in a slab and linked within a
/// price level's FIFO queue.
#[derive(Debug)]
struct OrderNode {
    order: Order,
    /// Sequence number assigned on insertion for time-priority comparison.
    seq: usize,
    next: Option<usize>,
    prev: Option<usize>,
}

/// BTreeMap-backed order book implementing price-time priority.
#[derive(Debug, Default)]
pub struct TreeMap {
    bids: BTreeMap<Price, PriceLevel>,
    asks: BTreeMap<Price, PriceLevel>,
    orders: Slab<OrderNode>,
    order_indexes: HashMap<Id, usize>,
    last_seq: usize,
}

impl TreeMap {
    /// Create a new, empty TreeMap order book.
    pub fn new() -> Self {
        TreeMap::default()
    }

    /// Remove an order (by slab index) from its corresponding price level and
    /// delete it from the book, cleaning up empty price levels.
    fn remove_order_from_level(&mut self, idx: usize) {
        let price = self.orders[idx].order.price;
        let side = self.orders[idx].order.side;
        self.order_indexes.remove(&self.orders[idx].order.id);
        let level = match side {
            Side::Bid => self.bids.get_mut(&price).unwrap(),
            Side::Ask => self.asks.get_mut(&price).unwrap(),
        };
        level.remove(&mut self.orders, idx);
        self.orders.remove(idx);
        if level.total_orders == 0 {
            match side {
                Side::Bid => self.bids.remove(&price),
                Side::Ask => self.asks.remove(&price),
            };
        }
    }

    /// Reduce (and possibly remove) orders from the given price level by a
    /// target `volume`, walking from the head to preserve FIFO. Panics if
    /// `volume` exceeds the level's total available volume.
    fn remove_by_volume(&mut self, level: &mut PriceLevel, volume: Volume) {
        assert!(volume <= level.total_volume);

        let mut remaining_volume = volume;
        while remaining_volume > 0 && level.total_volume > 0 {
            let idx = level.head.unwrap();

            // If the desired reduction is greater than or equal to the order's
            // remaining volume, remove the order completely and continue.
            if self.orders[idx].order.remaining_volume() <= remaining_volume {
                remaining_volume -= self.orders[idx].order.remaining_volume();
                self.remove_order_from_level(idx);
                continue;
            }

            // Otherwise, partially execute the order and stop.
            level.total_volume -= self.orders[idx].order.remaining_volume();
            self.orders[idx].order.executed_volume += remaining_volume;
            break;
        }
    }
}

impl Book for TreeMap {
    /// Insert a new order into the book at its price level.
    fn add(&mut self, order: Order) -> Result<(), Error> {
        if self.order_indexes.contains_key(&order.id) {
            return Err(Error::OrderExists);
        }

        let idx = self.orders.insert(OrderNode {
            order,
            seq: self.last_seq,
            next: None,
            prev: None,
        });
        self.order_indexes.insert(self.orders[idx].order.id, idx);
        let level = match self.orders[idx].order.side {
            Side::Bid => self.bids.entry(self.orders[idx].order.price).or_default(),
            Side::Ask => self.asks.entry(self.orders[idx].order.price).or_default(),
        };
        level.push(&mut self.orders, idx);
        self.last_seq += 1;

        Ok(())
    }

    /// Cancel an existing order by id.
    fn cancel(&mut self, id: Id) -> Result<(), Error> {
        let idx = self.order_indexes.get(&id);
        if idx.is_none() {
            return Err(Error::OrderNotFound);
        }

        self.remove_order_from_level(*idx.unwrap());

        Ok(())
    }

    /// Return a snapshot of top-of-book depth up to `limit` levels per side.
    fn depth(&self, limit: usize) -> Depth {
        Depth {
            bids: self
                .bids
                .iter()
                .rev()
                .take(limit)
                .map(|(price, level)| DepthItem {
                    price: *price,
                    volume: level.total_volume,
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .take(limit)
                .map(|(price, level)| DepthItem {
                    price: *price,
                    volume: level.total_volume,
                })
                .collect(),
        }
    }

    /// Match the best bid and best ask while there is price overlap, producing
    /// trades and updating order states. The trade price follows maker-taker
    /// precedence: the earlier order at the top levels sets the price.
    fn match_orders(&mut self) -> Vec<Trade> {
        let asks_ptr: *mut BTreeMap<Price, PriceLevel> = &mut self.asks;
        let bids_ptr: *mut BTreeMap<Price, PriceLevel> = &mut self.bids;
        let mut trades = Vec::new();

        loop {
            let (top_bid_price, top_bid_level) = unsafe {
                match (*bids_ptr).last_entry() {
                    Some(e) => (*e.key(), e.into_mut()),
                    None => break,
                }
            };
            let (top_ask_price, top_ask_level) = unsafe {
                match (*asks_ptr).first_entry() {
                    Some(e) => (*e.key(), e.into_mut()),
                    None => break,
                }
            };

            if top_ask_price > top_bid_price {
                break;
            }

            let mut volume = cmp::min(top_bid_level.total_volume, top_ask_level.total_volume);
            while volume > 0 {
                let bid_idx = top_bid_level.head.unwrap();
                let ask_idx = top_ask_level.head.unwrap();
                let trade_volume = cmp::min(self.orders[bid_idx].order.remaining_volume(), self.orders[ask_idx].order.remaining_volume());
                let (trade_price, is_bid_maker) = if self.orders[bid_idx].seq < self.orders[ask_idx].seq {
                    (self.orders[bid_idx].order.price, true)
                } else {
                    (self.orders[ask_idx].order.price, false)
                };
                trades.push(Trade{
                    id: 0,
                    bid_order_id: self.orders[bid_idx].order.id,
                    ask_order_id: self.orders[ask_idx].order.id,
                    is_bid_maker,
                    price: trade_price,
                    volume: trade_volume,
                    timestamp: OffsetDateTime::now_utc(),
                });
                self.remove_by_volume(top_bid_level, volume);
                self.remove_by_volume(top_ask_level, volume);
                volume -= trade_volume
            }
        }

        trades
    }
}
