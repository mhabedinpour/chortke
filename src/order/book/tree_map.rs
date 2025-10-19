//! Order book implementation backed by BTreeMap price levels.
//!
//! This module provides a simple price-time priority limit order book using two
//! BTreeMaps (one for bids descending, one for asks ascending). Each price level
//! maintains a FIFO queue of orders via indices into a Slab, avoiding frequent
//! allocations and allowing O(1) insertion/removal within a level. Matching is
//! performed by crossing the best bid and best ask while prices overlap.

use crate::collections::slab::SnapshotableSlab;
use crate::order::book::{Depth, DepthItem, Error, HotBook};
use crate::order::{Id, Order, Price, Side, Status, Volume};
use crate::seq;
use crate::trade::Trade;
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
    fn push(&mut self, orders: &mut SnapshotableSlab<OrderNode>, order_idx: usize) {
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
    fn remove(&mut self, orders: &mut SnapshotableSlab<OrderNode>, order_idx: usize) {
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
#[derive(Debug, Clone)]
struct OrderNode {
    order: Order,
    next: Option<usize>,
    prev: Option<usize>,
}

/// BTreeMap-backed order book implementing price-time priority.
#[derive(Debug, Default)]
pub struct TreeMap {
    bids: BTreeMap<Price, PriceLevel>,
    asks: BTreeMap<Price, PriceLevel>,
    orders: SnapshotableSlab<OrderNode>,
    order_indexes: HashMap<Id, usize>,
}

impl TreeMap {
    /// Create a new, empty TreeMap order book.
    pub fn new() -> Self {
        TreeMap::default()
    }

    /// Remove an order (by slab index) from its corresponding price level and
    /// delete it from the book, cleaning up empty price levels.
    fn remove_order_from_level(&mut self, idx: usize) -> Order {
        let side = self.orders[idx].order.side;
        let price = self.orders[idx].order.price;

        let level = match side {
            Side::Bid => self.bids.get_mut(&price).unwrap(),
            Side::Ask => self.asks.get_mut(&price).unwrap(),
        };
        level.remove(&mut self.orders, idx);
        if level.total_orders == 0 {
            match side {
                Side::Bid => self.bids.remove(&price),
                Side::Ask => self.asks.remove(&price),
            };
        }

        let node = self.orders.remove(idx);
        self.order_indexes.remove(&node.order.id);
        node.order
    }

    /// Reduce (and possibly remove) orders from the given price level by a
    /// target `volume`, walking from the head to preserve FIFO. Panics if
    /// `volume` exceeds the level's total available volume.
    fn remove_by_volume(
        &mut self,
        level: &mut PriceLevel,
        volume: Volume,
        closer_seq: seq::Seq,
    ) -> Vec<Order> {
        assert!(volume <= level.total_volume);

        let mut remaining_volume = volume;
        let mut closed_orders = Vec::new();
        while remaining_volume > 0 && level.total_volume > 0 {
            let idx = level.head.unwrap();

            // If the desired reduction is greater than or equal to the order's
            // remaining volume, remove the order completely and continue.
            if self.orders[idx].order.remaining_volume() <= remaining_volume {
                remaining_volume -= self.orders[idx].order.remaining_volume();
                let mut closed_order = self.remove_order_from_level(idx);
                closed_order.executed_volume = closed_order.volume;
                closed_order.status = Status::Executed;
                closed_order.closed_by = Some(closer_seq);
                closed_orders.push(closed_order);
                continue;
            }

            // Otherwise, partially execute the order by `remaining_volume` and stop.
            level.total_volume -= remaining_volume;
            self.orders[idx].order.executed_volume += remaining_volume;
            break;
        }

        closed_orders
    }
}

impl HotBook for TreeMap {
    /// Insert a new order into the book at its price level.
    fn add(&mut self, order: Order) -> Result<(), Error> {
        if self.order_indexes.contains_key(&order.id) {
            return Err(Error::OrderIdExists(order.id));
        }

        let idx = self.orders.insert(OrderNode {
            order,
            next: None,
            prev: None,
        });
        self.order_indexes.insert(self.orders[idx].order.id, idx);
        let level = match self.orders[idx].order.side {
            Side::Bid => self.bids.entry(self.orders[idx].order.price).or_default(),
            Side::Ask => self.asks.entry(self.orders[idx].order.price).or_default(),
        };
        level.push(&mut self.orders, idx);

        Ok(())
    }

    /// Cancel an existing order by id.
    fn cancel(&mut self, id: Id, seq: seq::Seq) -> Result<Order, Error> {
        let idx = self.order_indexes.get(&id);
        if idx.is_none() {
            return Err(Error::OrderIdNotFound(id));
        }

        let mut order = self.remove_order_from_level(*idx.unwrap());
        order.status = Status::Canceled;
        order.closed_by = Some(seq);
        Ok(order)
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
    fn match_orders(&mut self) -> (Vec<Trade>, Vec<Order>) {
        let asks_ptr: *mut BTreeMap<Price, PriceLevel> = &mut self.asks;
        let bids_ptr: *mut BTreeMap<Price, PriceLevel> = &mut self.bids;
        let mut trades = Vec::new();
        let mut closed_orders = Vec::new();

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
                let trade_volume = cmp::min(
                    self.orders[bid_idx].order.remaining_volume(),
                    self.orders[ask_idx].order.remaining_volume(),
                );
                let (trade_price, is_bid_maker) =
                    if self.orders[bid_idx].order.id < self.orders[ask_idx].order.id {
                        (self.orders[bid_idx].order.price, true)
                    } else {
                        (self.orders[ask_idx].order.price, false)
                    };
                trades.push(Trade {
                    bid_order_id: self.orders[bid_idx].order.id,
                    ask_order_id: self.orders[ask_idx].order.id,
                    is_bid_maker,
                    price: trade_price,
                    volume: trade_volume,
                    timestamp: OffsetDateTime::now_utc(),
                });
                let closer_index = if is_bid_maker {
                    self.orders[bid_idx].order.id
                } else {
                    self.orders[ask_idx].order.id
                };
                closed_orders.append(
                    self.remove_by_volume(top_bid_level, trade_volume, closer_index)
                        .as_mut(),
                );
                closed_orders.append(
                    self.remove_by_volume(top_ask_level, trade_volume, closer_index)
                        .as_mut(),
                );
                volume -= trade_volume;
            }
        }

        (trades, closed_orders)
    }

    // Gets an order by its ID.
    fn lookup(&self, id: Id) -> Option<&Order> {
        let idx = self.order_indexes.get(&id)?;

        Some(&self.orders[*idx].order)
    }
}

#[cfg(test)]
mod tests {
    use super::TreeMap;
    use crate::order::Id;
    use crate::order::book::{DepthItem, Error, HotBook};
    use crate::order::{Order, Side};

    fn o(id: Id, side: Side, price: u64, vol: u64) -> Order {
        Order::new(id, "c".to_string(), format!("c{}", id), side, price, vol)
    }

    #[test]
    fn test_add_and_depth_orders() {
        let mut book = TreeMap::new();

        // Bids at 100 and 101, Asks at 102 and 103
        book.add(o(1, Side::Bid, 100, 5)).unwrap();
        book.add(o(2, Side::Bid, 101, 1)).unwrap();
        book.add(o(3, Side::Ask, 102, 7)).unwrap();
        book.add(o(4, Side::Ask, 103, 2)).unwrap();

        let d = book.depth(10);

        // Bids should be in descending order by price.
        assert_eq!(
            d.bids.len(),
            2,
            "bids length mismatch: got {} bids: {:?}",
            d.bids.len(),
            d.bids
        );
        assert_eq!(
            d.bids[0],
            DepthItem {
                price: 101,
                volume: 1
            },
            "top bid depth item mismatch: got {:?}",
            d.bids.get(0)
        );
        assert_eq!(
            d.bids[1],
            DepthItem {
                price: 100,
                volume: 5
            },
            "second bid depth item mismatch: got {:?}",
            d.bids.get(1)
        );

        // Asks should be in ascending order by price.
        assert_eq!(
            d.asks.len(),
            2,
            "asks length mismatch: got {} asks: {:?}",
            d.asks.len(),
            d.asks
        );
        assert_eq!(
            d.asks[0],
            DepthItem {
                price: 102,
                volume: 7
            },
            "top ask depth item mismatch: got {:?}",
            d.asks.get(0)
        );
        assert_eq!(
            d.asks[1],
            DepthItem {
                price: 103,
                volume: 2
            },
            "second ask depth item mismatch: got {:?}",
            d.asks.get(1)
        );
    }

    #[test]
    fn test_duplicate_id_and_cancel_not_found() {
        let mut book = TreeMap::new();

        book.add(o(10, Side::Bid, 100, 5)).unwrap();
        // Duplicate id should fail
        let err = book.add(o(10, Side::Ask, 101, 1)).unwrap_err();
        match err {
            Error::OrderIdExists(_) => {}
            _ => panic!("expected Error::OrderExists for duplicate id, got different error"),
        }

        // Cancel unknown id should fail
        let err = book.cancel(999, 0).unwrap_err();
        match err {
            Error::OrderIdNotFound(_) => {}
            _ => panic!("expected Error::OrderNotFound for unknown id, got different error"),
        }
    }

    #[test]
    fn test_depth_limit() {
        let mut book = TreeMap::new();

        // Build multiple levels on each side
        book.add(o(1, Side::Bid, 100, 1)).unwrap();
        book.add(o(2, Side::Bid, 101, 2)).unwrap();
        book.add(o(3, Side::Bid, 102, 3)).unwrap();

        book.add(o(4, Side::Ask, 103, 4)).unwrap();
        book.add(o(5, Side::Ask, 104, 5)).unwrap();
        book.add(o(6, Side::Ask, 105, 6)).unwrap();

        // Limit to top 2 levels per side
        let d = book.depth(2);
        assert_eq!(
            d.bids.len(),
            2,
            "bids length with limit=2 mismatch: {:?}",
            d.bids
        );
        assert_eq!(
            d.asks.len(),
            2,
            "asks length with limit=2 mismatch: {:?}",
            d.asks
        );

        // Bids descending: 102, 101
        assert_eq!(
            d.bids[0],
            DepthItem {
                price: 102,
                volume: 3
            },
            "top bid level mismatch: got {:?}",
            d.bids.get(0)
        );
        assert_eq!(
            d.bids[1],
            DepthItem {
                price: 101,
                volume: 2
            },
            "second bid level mismatch: got {:?}",
            d.bids.get(1)
        );

        // Asks ascending: 103, 104
        assert_eq!(
            d.asks[0],
            DepthItem {
                price: 103,
                volume: 4
            },
            "top ask level mismatch: got {:?}",
            d.asks.get(0)
        );
        assert_eq!(
            d.asks[1],
            DepthItem {
                price: 104,
                volume: 5
            },
            "second ask level mismatch: got {:?}",
            d.asks.get(1)
        );
    }

    #[test]
    fn test_match_cross_full_bid_maker() {
        let mut book = TreeMap::new();
        // Bid arrives first (maker), Ask arrives later (taker)
        book.add(o(1, Side::Bid, 101, 5)).unwrap();
        book.add(o(2, Side::Ask, 100, 5)).unwrap();

        let (trades, closed) = book.match_orders();
        assert_eq!(
            trades.len(),
            1,
            "expected exactly one trade, got {}: {:?}",
            trades.len(),
            trades
        );
        let t = &trades[0];
        assert_eq!(
            t.bid_order_id, 1,
            "bid order id mismatch: got {}",
            t.bid_order_id
        );
        assert_eq!(
            t.ask_order_id, 2,
            "ask order id mismatch: got {}",
            t.ask_order_id
        );
        assert!(
            t.is_bid_maker,
            "maker side mismatch: expected bid maker, got ask maker"
        );
        assert_eq!(
            t.price, 101,
            "trade price mismatch (should be maker price): got {}",
            t.price
        ); // maker's price
        assert_eq!(t.volume, 5, "trade volume mismatch: got {}", t.volume);

        // Closed orders should include both fully filled orders (bid then ask)
        assert_eq!(
            closed.len(),
            2,
            "expected 2 closed orders, got {:?}",
            closed
        );
        assert_eq!(
            closed[0].id, 1,
            "first closed should be bid id=1, got {}",
            closed[0].id
        );
        assert_eq!(
            closed[1].id, 2,
            "second closed should be ask id=2, got {}",
            closed[1].id
        );
        assert_eq!(closed[0].remaining_volume(), 0);
        assert_eq!(closed[1].remaining_volume(), 0);
        // closed_by should be set to maker's id (bid id=1)
        assert_eq!(
            closed[0].closed_by,
            Some(1),
            "closed_by for bid should be maker=1, got {:?}",
            closed[0].closed_by
        );
        assert_eq!(
            closed[1].closed_by,
            Some(1),
            "closed_by for ask should be maker=1, got {:?}",
            closed[1].closed_by
        );

        // Book should be empty after full cross
        let d = book.depth(10);
        assert!(
            d.bids.is_empty(),
            "expected no resting bids, got: {:?}",
            d.bids
        );
        assert!(
            d.asks.is_empty(),
            "expected no resting asks, got: {:?}",
            d.asks
        );
    }

    #[test]
    fn test_match_cross_full_ask_maker() {
        let mut book = TreeMap::new();
        // Ask arrives first (maker), Bid later (taker)
        book.add(o(10, Side::Ask, 100, 4)).unwrap();
        book.add(o(11, Side::Bid, 101, 4)).unwrap();

        let (trades, closed) = book.match_orders();
        assert_eq!(
            trades.len(),
            1,
            "expected exactly one trade, got {}: {:?}",
            trades.len(),
            trades
        );
        let t = &trades[0];
        assert_eq!(
            t.bid_order_id, 11,
            "bid order id mismatch: got {}",
            t.bid_order_id
        );
        assert_eq!(
            t.ask_order_id, 10,
            "ask order id mismatch: got {}",
            t.ask_order_id
        );
        assert!(
            !t.is_bid_maker,
            "maker side mismatch: expected ask maker, got bid maker"
        );
        assert_eq!(
            t.price, 100,
            "trade price mismatch (should be maker price): got {}",
            t.price
        ); // maker's price
        assert_eq!(t.volume, 4, "trade volume mismatch: got {}", t.volume);

        // Both orders fully filled; closed should be [bid 11, ask 10]
        assert_eq!(
            closed.len(),
            2,
            "expected 2 closed orders, got {:?}",
            closed
        );
        assert_eq!(closed[0].id, 11);
        assert_eq!(closed[1].id, 10);
        assert_eq!(closed[0].remaining_volume(), 0);
        assert_eq!(closed[1].remaining_volume(), 0);

        let d = book.depth(10);
        assert!(
            d.bids.is_empty(),
            "expected no resting bids, got: {:?}",
            d.bids
        );
        assert!(
            d.asks.is_empty(),
            "expected no resting asks, got: {:?}",
            d.asks
        );
    }

    #[test]
    fn test_match_partial_and_fifo_across_multiple_orders() {
        let mut book = TreeMap::new();
        // Two bid orders at same price, FIFO: id=1 then id=2
        book.add(o(1, Side::Bid, 100, 2)).unwrap();
        book.add(o(2, Side::Bid, 100, 3)).unwrap();
        // One ask that will partially consume both
        book.add(o(3, Side::Ask, 99, 4)).unwrap();

        let (trades, closed) = book.match_orders();
        assert_eq!(
            trades.len(),
            2,
            "expected two trades, got {}: {:?}",
            trades.len(),
            trades
        );
        // First trade fully fills bid id=1 with 2
        assert_eq!(
            trades[0].bid_order_id, 1,
            "first trade bid id mismatch: got {}",
            trades[0].bid_order_id
        );
        assert_eq!(
            trades[0].ask_order_id, 3,
            "first trade ask id mismatch: got {}",
            trades[0].ask_order_id
        );
        assert_eq!(
            trades[0].volume, 2,
            "first trade volume mismatch: got {}",
            trades[0].volume
        );
        assert!(
            trades[0].is_bid_maker,
            "first trade maker mismatch: expected bid maker"
        ); // bid was earlier than ask
        assert_eq!(
            trades[0].price, 100,
            "first trade price mismatch: got {}",
            trades[0].price
        );
        // Second trade partially fills bid id=2 with remaining 2
        assert_eq!(
            trades[1].bid_order_id, 2,
            "second trade bid id mismatch: got {}",
            trades[1].bid_order_id
        );
        assert_eq!(
            trades[1].ask_order_id, 3,
            "second trade ask id mismatch: got {}",
            trades[1].ask_order_id
        );
        assert_eq!(
            trades[1].volume, 2,
            "second trade volume mismatch: got {}",
            trades[1].volume
        );
        assert!(
            trades[1].is_bid_maker,
            "second trade maker mismatch: expected bid maker"
        );
        assert_eq!(
            trades[1].price, 100,
            "second trade price mismatch: got {}",
            trades[1].price
        );

        // Closed orders: bid id=1 fully filled first, ask id=3 fully filled second
        assert_eq!(
            closed.len(),
            2,
            "expected 2 closed orders, got {:?}",
            closed
        );
        assert_eq!(
            closed[0].id, 1,
            "first closed should be bid id=1, got {}",
            closed[0].id
        );
        assert_eq!(
            closed[1].id, 3,
            "second closed should be ask id=3, got {}",
            closed[1].id
        );
        assert_eq!(closed[0].remaining_volume(), 0);
        assert_eq!(closed[1].remaining_volume(), 0);

        // Remaining depth: bid at 100 with volume 1; no asks
        let d = book.depth(10);
        assert_eq!(
            d.bids,
            vec![DepthItem {
                price: 100,
                volume: 1
            }],
            "remaining bids mismatch: got {:?}",
            d.bids
        );
        assert!(
            d.asks.is_empty(),
            "expected no resting asks, got: {:?}",
            d.asks
        );
    }

    #[test]
    fn test_depth_limit_zero() {
        let mut book = TreeMap::new();
        book.add(o(1, Side::Bid, 100, 5)).unwrap();
        book.add(o(2, Side::Ask, 101, 5)).unwrap();
        let d = book.depth(0);
        assert!(
            d.bids.is_empty(),
            "limit=0 should return no bid levels, got: {:?}",
            d.bids
        );
        assert!(
            d.asks.is_empty(),
            "limit=0 should return no ask levels, got: {:?}",
            d.asks
        );
    }

    #[test]
    fn test_no_cross_no_trades() {
        let mut book = TreeMap::new();
        book.add(o(1, Side::Bid, 100, 5)).unwrap();
        book.add(o(2, Side::Ask, 101, 5)).unwrap();
        let d_before = book.depth(10);
        let (trades, closed) = book.match_orders();
        assert!(
            trades.is_empty(),
            "expected no trades when no price overlap, got: {:?}",
            trades
        );
        assert!(
            closed.is_empty(),
            "expected no closed orders, got: {:?}",
            closed
        );
        let d_after = book.depth(10);
        assert_eq!(
            d_before.bids, d_after.bids,
            "bids changed despite no trades: before={:?}, after={:?}",
            d_before.bids, d_after.bids
        );
        assert_eq!(
            d_before.asks, d_after.asks,
            "asks changed despite no trades: before={:?}, after={:?}",
            d_before.asks, d_after.asks
        );
    }

    #[test]
    fn test_cancel_removes_level_when_last_order() {
        let mut book = TreeMap::new();
        book.add(o(1, Side::Bid, 100, 3)).unwrap();
        let d = book.depth(10);
        assert_eq!(
            d.bids.len(),
            1,
            "expected exactly one bid level before cancel, got: {:?}",
            d.bids
        );
        book.cancel(1, 0).unwrap();
        let d2 = book.depth(10);
        assert!(
            d2.bids.is_empty(),
            "expected bid level removed after cancel, got: {:?}",
            d2.bids
        );
        assert!(
            d2.asks.is_empty(),
            "expected no asks either, got: {:?}",
            d2.asks
        );
    }

    #[test]
    fn test_fifo_after_cancel_head() {
        let mut book = TreeMap::new();
        // Two bids at the same price. Cancel the head, leaving the second as maker.
        book.add(o(1, Side::Bid, 100, 2)).unwrap();
        book.add(o(2, Side::Bid, 100, 3)).unwrap();
        book.cancel(1, 0).unwrap();
        // Now cross with an ask that takes 2. It should trade against id=2.
        book.add(o(3, Side::Ask, 99, 2)).unwrap();
        let (trades, closed) = book.match_orders();
        assert_eq!(
            trades.len(),
            1,
            "expected one trade after crossing, got {}: {:?}",
            trades.len(),
            trades
        );
        let t = &trades[0];
        assert_eq!(
            t.bid_order_id, 2,
            "maker bid id should be 2 after canceling head; got {}",
            t.bid_order_id
        );
        assert_eq!(
            t.ask_order_id, 3,
            "taker ask id mismatch: got {}",
            t.ask_order_id
        );
        assert!(
            t.is_bid_maker,
            "expected bid to be maker after canceling head"
        );
        assert_eq!(
            t.price, 100,
            "trade price should be maker (100), got {}",
            t.price
        );
        assert_eq!(t.volume, 2, "trade volume mismatch, got {}", t.volume);
        // Closed orders: only the ask (id=3) should be fully filled
        assert_eq!(closed.len(), 1, "expected 1 closed order, got {:?}", closed);
        assert_eq!(closed[0].id, 3);
        assert_eq!(closed[0].remaining_volume(), 0);
        // Remaining at 100: 1
        let d = book.depth(10);
        assert_eq!(
            d.bids,
            vec![DepthItem {
                price: 100,
                volume: 1
            }],
            "remaining bid depth mismatch: got {:?}",
            d.bids
        );
        assert!(
            d.asks.is_empty(),
            "expected no remaining asks, got: {:?}",
            d.asks
        );
    }

    #[test]
    fn test_sweep_multiple_ask_levels_with_single_bid() {
        let mut book = TreeMap::new();
        // Makers: two ask levels arrive first
        book.add(o(10, Side::Ask, 101, 2)).unwrap();
        book.add(o(11, Side::Ask, 102, 3)).unwrap();
        // Taker: one large bid that sweeps them
        book.add(o(12, Side::Bid, 103, 10)).unwrap();

        let (trades, closed) = book.match_orders();
        assert_eq!(
            trades.len(),
            2,
            "expected two trades when sweeping two ask levels, got {}: {:?}",
            trades.len(),
            trades
        );
        // First trade hits best ask 101
        assert_eq!(
            trades[0].ask_order_id, 10,
            "first trade ask id mismatch: got {}",
            trades[0].ask_order_id
        );
        assert_eq!(
            trades[0].bid_order_id, 12,
            "first trade bid id mismatch: got {}",
            trades[0].bid_order_id
        );
        assert!(
            !trades[0].is_bid_maker,
            "first trade maker side mismatch: expected ask maker"
        );
        assert_eq!(
            trades[0].price, 101,
            "first trade price mismatch: got {}",
            trades[0].price
        );
        assert_eq!(
            trades[0].volume, 2,
            "first trade volume mismatch: got {}",
            trades[0].volume
        );
        // Second trade hits next ask 102
        assert_eq!(
            trades[1].ask_order_id, 11,
            "second trade ask id mismatch: got {}",
            trades[1].ask_order_id
        );
        assert_eq!(
            trades[1].bid_order_id, 12,
            "second trade bid id mismatch: got {}",
            trades[1].bid_order_id
        );
        assert!(
            !trades[1].is_bid_maker,
            "second trade maker side mismatch: expected ask maker"
        );
        assert_eq!(
            trades[1].price, 102,
            "second trade price mismatch: got {}",
            trades[1].price
        );
        assert_eq!(
            trades[1].volume, 3,
            "second trade volume mismatch: got {}",
            trades[1].volume
        );

        // Closed orders: both ask orders should be closed in price-time order
        assert_eq!(
            closed.len(),
            2,
            "expected 2 closed orders, got {:?}",
            closed
        );
        assert_eq!(closed[0].id, 10);
        assert_eq!(closed[1].id, 11);
        assert_eq!(closed[0].remaining_volume(), 0);
        assert_eq!(closed[1].remaining_volume(), 0);

        // Remaining bid should rest at 103 with volume 5
        let d = book.depth(10);
        assert_eq!(
            d.bids,
            vec![DepthItem {
                price: 103,
                volume: 5
            }],
            "remaining bids after sweep mismatch: got {:?}",
            d.bids
        );
        assert!(
            d.asks.is_empty(),
            "expected all asks consumed, got: {:?}",
            d.asks
        );
    }

    #[test]
    fn test_equal_price_partial_fill() {
        let mut book = TreeMap::new();
        // Bid maker at 100, ask taker at 100 partially fills
        book.add(o(1, Side::Bid, 100, 5)).unwrap();
        book.add(o(2, Side::Ask, 100, 3)).unwrap();
        let (trades, closed) = book.match_orders();
        assert_eq!(
            trades.len(),
            1,
            "expected one trade at equal price, got {}: {:?}",
            trades.len(),
            trades
        );
        let t = &trades[0];
        assert_eq!(t.bid_order_id, 1, "bid id mismatch: got {}", t.bid_order_id);
        assert_eq!(t.ask_order_id, 2, "ask id mismatch: got {}", t.ask_order_id);
        assert!(
            t.is_bid_maker,
            "maker side mismatch: expected bid maker at equal price"
        );
        assert_eq!(
            t.price, 100,
            "trade price mismatch at equal price: got {}",
            t.price
        );
        assert_eq!(
            t.volume, 3,
            "trade volume mismatch at equal price: got {}",
            t.volume
        );
        // Closed orders: only the ask (id=2) should be fully closed
        assert_eq!(closed.len(), 1, "expected 1 closed order, got {:?}", closed);
        assert_eq!(closed[0].id, 2);
        assert_eq!(closed[0].remaining_volume(), 0);
        // Remaining on bid: 2 at 100
        let d = book.depth(10);
        assert_eq!(
            d.bids,
            vec![DepthItem {
                price: 100,
                volume: 2
            }],
            "remaining bids mismatch after partial fill: got {:?}",
            d.bids
        );
        assert!(
            d.asks.is_empty(),
            "expected no resting asks after partial fill, got: {:?}",
            d.asks
        );
    }

    #[test]
    fn test_lookup_existing_returns_order() {
        let mut book = TreeMap::new();
        // Add two orders and verify lookup returns correct references
        book.add(o(1, Side::Bid, 100, 5)).unwrap();
        book.add(o(2, Side::Ask, 101, 7)).unwrap();

        let o1 = book.lookup(1).expect("lookup should find id=1");
        assert_eq!(o1.id, 1);
        assert_eq!(o1.price, 100);
        assert!(matches!(o1.side, Side::Bid));
        assert_eq!(o1.remaining_volume(), 5);

        let o2 = book.lookup(2).expect("lookup should find id=2");
        assert_eq!(o2.id, 2);
        assert_eq!(o2.price, 101);
        assert!(matches!(o2.side, Side::Ask));
        assert_eq!(o2.remaining_volume(), 7);
    }

    #[test]
    fn test_lookup_not_found_returns_none() {
        let mut book = TreeMap::new();
        // Empty book -> None
        assert!(book.lookup(42).is_none(), "empty book should return None");

        // After adding different id
        book.add(o(1, Side::Bid, 100, 5)).unwrap();
        assert!(book.lookup(2).is_none(), "unknown id should return None");
    }

    #[test]
    fn test_lookup_after_match_and_cancel() {
        let mut book = TreeMap::new();
        // Two orders that will partially fill one and fully close the other
        book.add(o(10, Side::Bid, 100, 5)).unwrap(); // maker
        book.add(o(11, Side::Ask, 99, 3)).unwrap(); // taker, will fully execute

        let (_trades, closed) = book.match_orders();
        // Ask 11 should be fully closed and no longer found
        assert!(closed.iter().any(|c| c.id == 11));
        assert!(
            book.lookup(11).is_none(),
            "fully executed order should be gone"
        );

        // Bid 10 should remain with 2 units left
        let remaining = book
            .lookup(10)
            .expect("partially filled order should remain");
        assert_eq!(
            remaining.remaining_volume(),
            2,
            "remaining volume after partial fill should be 2"
        );

        // Cancel the remaining order and verify it disappears from lookup
        let canceled = book.cancel(10, 0).unwrap();
        assert_eq!(canceled.id, 10);
        assert!(book.lookup(10).is_none(), "canceled order should be gone");
    }
}
