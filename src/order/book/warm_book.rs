//! Warm book wrapper for an in-memory order book.
//!
//! WarmBook sits on top of a HotBook implementation and augments it with a
//! small, in-memory cache of recently closed orders. This allows lookups of
//! orders that are no longer active (e.g., fully filled or canceled) without
//! having to query a slower archival storage.
//!
//! Lifecycle and retention:
//! - Active orders live solely in the underlying `HotBook`.
//! - When orders are closed (either by cancellation or matching), they are
//!   copied into `WarmBook::closed_orders` for a short period, so they can be
//!   retrieved via `lookup`/`lookup_by_client_id` after they leave the active
//!   book.
//! - Closed orders should eventually be archived to a persistent storage system
//!   (not implemented here) and then removed from memory to bound RAM usage.

use crate::collections::hash_map::SnapshotableHashMap;
use crate::order::book::{Depth, Error, HotBook, SnapshotableBook};
use crate::order::{ClientId, Id, Order};
use crate::{seq, user};
use std::cmp;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::collections::HashMap;

struct SeqClosedOrders {
    close_seq: seq::Seq,
    orders: Vec<Id>,
}

impl PartialEq for SeqClosedOrders {
    fn eq(&self, other: &Self) -> bool {
        self.close_seq == other.close_seq
    }
}
impl Eq for SeqClosedOrders {}

impl PartialOrd for SeqClosedOrders {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for SeqClosedOrders {
    fn cmp(&self, other: &Self) -> Ordering {
        self.close_seq.cmp(&other.close_seq)
    }
}

/// A lightweight iterator-like state used during full order book snapshots.
#[derive(Debug)]
struct Snapshot {
    /// Frozen list of order ids captured at the moment `take_snapshot` was
    /// called.
    keys: Vec<Id>,
    /// Current position within `keys` indicating how many orders have already
    /// been emitted via `snapshot_batch`.
    cursor: usize,
}

/// A wrapper around a `HotBook` that tracks recently closed orders in memory
/// for quick lookup by ID or by user/client ID pair.
///
/// Type parameter `T` is any order book implementation that satisfies the
/// `HotBook` trait.
pub struct WarmBook<T: HotBook> {
    hot_book: T,
    /// Maps formatted `user_id:client_id` strings to internal order IDs to
    /// support client-side identifiers.
    client_id_to_order_id: HashMap<String, Id>,
    /// In-memory store of recently closed orders (canceled or fully filled).
    /// These entries are intended to be short-lived.
    closed_orders: SnapshotableHashMap<Id, Order>,
    /// A min-heap of batches of closed orders, grouped by the sequence at
    /// which they were closed.
    ///
    /// Implemented as `BinaryHeap<Reverse<SeqClosedOrders>>` so that the smallest
    /// closing seq is always at the top (min-heap behavior). Each heap node
    /// contains the closure seq and the list of order IDs that closed at
    /// that seq. This structure is consumed by `evict_closed_orders` to
    /// efficiently remove all closed orders up to and including a given archived
    /// seq, after they have been persisted to durable storage.
    closed_orders_by_seq: BinaryHeap<Reverse<SeqClosedOrders>>,
    snapshot: Option<Snapshot>,
}

impl<T: HotBook> WarmBook<T> {
    /// Create a new warm book wrapper over the provided hot book.
    pub fn new(hot_book: T) -> Self {
        WarmBook {
            hot_book,
            client_id_to_order_id: HashMap::new(),
            closed_orders: SnapshotableHashMap::default(),
            closed_orders_by_seq: BinaryHeap::new(),
            snapshot: None,
        }
    }

    /// Add a new order to the underlying hot book.
    ///
    /// Returns an error if the `(user_id, client_id)` pair already exists.
    /// On success, the mapping from client ID to order ID is recorded for
    /// later lookup.
    pub fn add(&mut self, order: Order) -> Result<(), Error> {
        let user_client_id = order.user_client_id();
        let order_id = order.id;
        if self.client_id_to_order_id.contains_key(&user_client_id) {
            return Err(Error::OrderClientIdExists(order.client_id));
        }

        let res = self.hot_book.add(order);
        if res.is_ok() {
            self.client_id_to_order_id.insert(user_client_id, order_id);
        }

        res
    }

    /// Cancel an active order by its internal `Id`.
    ///
    /// If successful, the canceled order is moved to the warm store of
    /// closed orders for subsequent lookups.
    pub fn cancel(&mut self, id: Id, seq: seq::Seq) -> Result<Order, Error> {
        match self.hot_book.cancel(id, seq) {
            Ok(order) => {
                self.closed_orders.insert(id, order.clone());
                self.closed_orders_by_seq.push(Reverse(SeqClosedOrders {
                    close_seq: seq,
                    orders: vec![id],
                }));
                Ok(order)
            }
            Err(e) => Err(e),
        }
    }

    /// Cancel an order by `(user_id, client_id)` pair.
    ///
    /// Returns an error if the client ID is unknown.
    pub fn cancel_by_client_id(
        &mut self,
        user_id: user::Id,
        client_id: ClientId,
        seq: seq::Seq,
    ) -> Result<Order, Error> {
        let id = self
            .client_id_to_order_id
            .get(&Order::format_user_client_id(&user_id, &client_id));
        match id {
            Some(id) => self.cancel(*id, seq),
            None => Err(Error::OrderClientIdNotFound(client_id)),
        }
    }

    /// Compute aggregated depth from the underlying book, limited to `limit` levels.
    pub fn depth(&self, limit: usize) -> Depth {
        self.hot_book.depth(limit)
    }

    /// Match crossable orders in the underlying book.
    ///
    /// Returns the vector of generated trades and the vector of orders that
    /// became closed because of the matching. Closed orders are cached in the
    /// warm store for later lookup.
    pub fn match_orders(&mut self) -> (Vec<crate::trade::Trade>, Vec<Order>) {
        let (trades, closed_orders) = self.hot_book.match_orders();
        for closed_order in &closed_orders {
            self.closed_orders
                .insert(closed_order.id, closed_order.clone());
        }

        let mut grouped: HashMap<seq::Seq, Vec<Id>> = HashMap::new();
        for order in closed_orders.iter() {
            grouped
                .entry(order.closed_by.unwrap())
                .or_default()
                .push(order.id);
        }
        for (seq, ids) in grouped.into_iter() {
            self.closed_orders_by_seq.push(Reverse(SeqClosedOrders {
                close_seq: seq,
                orders: ids,
            }));
        }

        (trades, closed_orders)
    }

    /// Lookup an order by internal `Id`.
    ///
    /// Prefers the warm store of closed orders; if not found there, falls back
    /// to the underlying hot book (active orders).
    pub fn lookup(&self, id: Id) -> Option<Order> {
        if let Some(order) = self.closed_orders.get(&id) {
            return Some(order.clone());
        }

        self.hot_book.lookup(id).cloned()
    }

    /// Lookup an order by `(user_id, client_id)` pair.
    ///
    /// Will search both closed and active orders through `lookup`.
    pub fn lookup_by_client_id(&self, user_id: user::Id, client_id: ClientId) -> Option<Order> {
        let id = self
            .client_id_to_order_id
            .get(&Order::format_user_client_id(&user_id, &client_id));
        match id {
            Some(id) => self.lookup(*id),
            None => None,
        }
    }
    /// Evict closed orders that are safe to remove from the warm cache.
    ///
    /// This method is intended to be called after the caller has persisted
    /// ("archived") all order closures up to and including `archived_seq` to a
    /// durable storage. It walks the internal min-heap of closed-order batches,
    /// ordered by their closing seq, and removes any batch whose seq
    /// is less than or equal to `archived_seq`.
    ///
    /// For each evicted order id, the corresponding entry is removed from:
    /// - `closed_orders` (the warm cache of closed orders), and
    /// - `client_id_to_order_id` so that lookups by `(user_id, client_id)` no
    ///   longer resolve to evicted, closed orders.
    ///
    /// If `archived_seq` is lower than the smallest pending seq, the call
    /// is a no-op.
    pub fn evict_closed_orders(&mut self, archived_seq: seq::Seq) {
        while let Some(Reverse(node)) = self.closed_orders_by_seq.peek() {
            if node.close_seq > archived_seq {
                break;
            }
            let seq = self.closed_orders_by_seq.pop();

            for order_id in seq.unwrap().0.orders.iter() {
                let order = self.closed_orders.remove(order_id);
                if let Some(order) = order {
                    self.client_id_to_order_id
                        .remove(&Order::format_user_client_id(
                            &order.user_id,
                            &order.client_id,
                        ));
                }
            }
        }
    }
}

impl<T: HotBook> SnapshotableBook for WarmBook<T> {
    /// Begins a snapshot over the current set of closed orders.
    ///
    /// Only one snapshot can be active at a time; attempting to take another
    /// while one is in progress returns `Error::AnotherSnapshotAlreadyTaken`.
    /// Orders closed after this call will not be part of the snapshot stream.
    fn take_snapshot(&mut self) -> Result<(), Error> {
        if self.snapshot.is_some() {
            return Err(Error::AnotherSnapshotAlreadyTaken);
        }

        self.snapshot = Some(Snapshot {
            keys: self.closed_orders.begin_snapshot(),
            cursor: 0,
        });
        Ok(())
    }

    /// Returns up to `limit` closed orders from the active snapshot.
    ///
    /// When no more orders remain in the snapshot, returns `Ok(None)`.
    /// If called without an active snapshot, returns `Error::NoSnapshotTaken`.
    /// If orders are evicted or modified during an active snapshot, the
    /// previously snapshotted values are returned (snapshot isolation).
    fn snapshot_batch(&mut self, limit: usize) -> Result<Option<Vec<Order>>, Error> {
        if self.snapshot.is_none() {
            return Err(Error::NoSnapshotTaken);
        }

        let snapshot = self.snapshot.as_mut().unwrap();
        let left_orders = snapshot.keys.len() - snapshot.cursor;
        let batch_size = cmp::min(left_orders, limit);
        if batch_size == 0 {
            return Ok(None);
        }

        let mut orders = Vec::with_capacity(batch_size);
        for _ in 0..batch_size {
            let order = self
                .closed_orders
                .get_for_snapshot(&snapshot.keys[snapshot.cursor])
                .unwrap()
                .clone();
            orders.push(order);
            snapshot.cursor += 1;
        }
        Ok(Some(orders))
    }

    /// Ends the current snapshot session and clears internal snapshot state.
    ///
    /// Returns `Error::NoSnapshotTaken` if no snapshot is active.
    fn end_snapshot(&mut self) -> Result<(), Error> {
        if self.snapshot.is_none() {
            return Err(Error::NoSnapshotTaken);
        }

        self.snapshot = None;
        self.closed_orders.end_snapshot();
        Ok(())
    }

    /// Restores a batch of closed orders back into the warm cache.
    ///
    /// Orders without `closed_by` are ignored. For the rest, this method:
    /// - Rebuilds the `(user_id, client_id) -> order_id` index,
    /// - Re-inserts the orders into the closed-order store, and
    /// - Reconstructs the min-heap that tracks groups of orders by their closing seq.
    fn restore_snapshot_batch(&mut self, orders: Vec<Order>) -> Result<(), Error> {
        let mut closed_orders_by_seq: HashMap<seq::Seq, Vec<Id>> = HashMap::new();
        for order in orders {
            if order.closed_by.is_none() {
                continue;
            }

            self.client_id_to_order_id.insert(
                Order::format_user_client_id(&order.user_id, &order.client_id),
                order.id,
            );
            closed_orders_by_seq
                .entry(order.closed_by.unwrap())
                .or_default()
                .push(order.id);
            self.closed_orders.insert(order.id, order);
        }

        for (seq, ids) in closed_orders_by_seq.into_iter() {
            self.closed_orders_by_seq.push(Reverse(SeqClosedOrders {
                close_seq: seq,
                orders: ids,
            }));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::WarmBook;
    use crate::order::book::{Error, SnapshotableBook, tree_map::TreeMap};
    use crate::order::{Order, Side};

    fn wb() -> WarmBook<TreeMap> {
        WarmBook::new(TreeMap::new())
    }

    fn o(id: u64, user: &str, client: &str, side: Side, price: u64, vol: u64) -> Order {
        Order::new(id, user.to_string(), client.to_string(), side, price, vol)
    }

    #[test]
    fn test_add_and_lookup_active() {
        let mut book = wb();
        let a = o(1, "u1", "c1", Side::Bid, 100, 10);

        let res = book.add(a.clone());
        assert!(
            res.is_ok(),
            "add should succeed for new order, got: {:?}",
            res
        );

        let found = book.lookup(1);
        assert!(
            found.is_some(),
            "lookup should find active order by id, got: {:?}",
            found
        );
        let found_client = book.lookup_by_client_id("u1".to_string(), "c1".to_string());
        assert!(
            found_client.is_some(),
            "lookup_by_client_id should find active order, got: {:?}",
            found_client
        );
    }

    #[test]
    fn test_add_duplicate_client_id_errors() {
        let mut book = wb();
        let a1 = o(1, "u1", "dup", Side::Bid, 100, 10);
        let a2 = o(2, "u1", "dup", Side::Ask, 101, 5);
        assert!(book.add(a1).is_ok(), "first add should succeed");
        let res = book.add(a2);
        match res {
            Err(Error::OrderClientIdExists(cid)) => {
                assert_eq!(
                    cid,
                    "dup".to_string(),
                    "error should carry the duplicated client id, got: {}",
                    cid
                )
            }
            other => panic!("expected OrderClientIdExists, got: {:?}", other),
        }
    }

    #[test]
    fn test_cancel_moves_to_closed_and_lookup() {
        let mut book = wb();
        let a = o(1, "u1", "c1", Side::Bid, 100, 10);
        assert!(
            book.add(a.clone()).is_ok(),
            "add should succeed before cancel"
        );

        let canceled = book.cancel(1, 0).expect("cancel by id should succeed");
        assert_eq!(
            canceled.id, 1,
            "canceled order id should match, got: {}",
            canceled.id
        );

        let found_active = book.lookup(1);
        assert!(
            found_active.is_some(),
            "lookup should find canceled order in warm store, got: {:?}",
            found_active
        );

        let found_client = book.lookup_by_client_id("u1".to_string(), "c1".to_string());
        assert!(
            found_client.is_some(),
            "lookup_by_client_id should find canceled order in warm store, got: {:?}",
            found_client
        );
    }

    #[test]
    fn test_cancel_by_client_id_paths() {
        let mut book = wb();
        assert!(
            matches!(book.cancel_by_client_id("uX".to_string(), "nope".to_string(), 0), Err(Error::OrderClientIdNotFound(cid)) if cid=="nope"),
            "cancel_by_client_id should error when unknown; got different result"
        );

        assert!(
            book.add(o(1, "u1", "c1", Side::Bid, 100, 10)).is_ok(),
            "add should succeed"
        );
        let canceled = book
            .cancel_by_client_id("u1".to_string(), "c1".to_string(), 0)
            .expect("cancel_by_client_id should succeed for existing mapping");
        assert_eq!(
            canceled.id, 1,
            "id of canceled order should be 1, got: {}",
            canceled.id
        );

        let found = book.lookup(1);
        assert!(
            found.is_some(),
            "lookup should find order from warm store after cancel, got: {:?}",
            found
        );
    }

    #[test]
    fn test_match_orders_caches_closed_and_lookup_prefers_closed() {
        let mut book = wb();
        // Maker ask at 101 for 5, taker bid at 105 for 5 -> ask closes
        assert!(
            book.add(o(1, "u1", "a1", Side::Ask, 101, 5)).is_ok(),
            "add ask should succeed"
        );
        assert!(
            book.add(o(2, "u2", "b1", Side::Bid, 105, 5)).is_ok(),
            "add bid should succeed"
        );

        let (_trades, closed) = book.match_orders();
        assert!(
            !closed.is_empty(),
            "closed vector should not be empty after crossing; got len {}",
            closed.len()
        );
        let closed_ids: Vec<_> = closed.iter().map(|o| o.id).collect();
        assert!(
            closed_ids.contains(&1) || closed_ids.contains(&2),
            "at least one order should close (id 1 or 2), got: {:?}",
            closed_ids
        );

        // The closed order must now be retrievable from warm store
        for id in closed_ids.iter() {
            let found = book.lookup(*id);
            assert!(
                found.is_some(),
                "lookup should find closed order {} in warm store, got: {:?}",
                id,
                found
            );
        }
    }

    #[test]
    fn test_lookup_by_client_id_active_and_closed() {
        let mut book = wb();
        assert!(
            book.add(o(1, "u1", "c1", Side::Bid, 100, 10)).is_ok(),
            "add should succeed"
        );
        let active = book.lookup_by_client_id("u1".to_string(), "c1".to_string());
        assert!(
            active.is_some(),
            "should find active order by client id, got: {:?}",
            active
        );

        // Cancel to move into warm store and ensure lookup still works
        let _ = book.cancel(1, 0).expect("cancel should succeed");
        let closed = book.lookup_by_client_id("u1".to_string(), "c1".to_string());
        assert!(
            closed.is_some(),
            "should find closed order by client id in warm store, got: {:?}",
            closed
        );
    }

    #[test]
    fn test_evict_closed_orders_noop_when_below_min_seq() {
        let mut book = wb();
        // Cancel two orders at seq 5 and 7
        assert!(book.add(o(1, "u1", "c1", Side::Bid, 100, 10)).is_ok());
        assert!(book.add(o(2, "u2", "c2", Side::Ask, 101, 5)).is_ok());
        let _ = book.cancel(1, 5).expect("cancel 1");
        let _ = book.cancel(2, 7).expect("cancel 2");
        // Both should be present
        assert!(book.lookup(1).is_some());
        assert!(book.lookup(2).is_some());
        // Evict with archived_seq lower than earliest (4)
        book.evict_closed_orders(4);
        // Nothing should be evicted
        assert!(book.lookup(1).is_some());
        assert!(book.lookup(2).is_some());
        assert!(
            book.lookup_by_client_id("u1".to_string(), "c1".to_string())
                .is_some()
        );
        assert!(
            book.lookup_by_client_id("u2".to_string(), "c2".to_string())
                .is_some()
        );
    }

    #[test]
    fn test_evict_closed_orders_partial_then_full() {
        let mut book = wb();
        assert!(book.add(o(1, "u1", "c1", Side::Bid, 100, 10)).is_ok());
        assert!(book.add(o(2, "u2", "c2", Side::Ask, 101, 5)).is_ok());
        assert!(book.add(o(3, "u3", "c3", Side::Bid, 99, 3)).is_ok());
        // Close at seq 5, 7, 9 respectively
        let _ = book.cancel(1, 5).expect("cancel 1");
        let _ = book.cancel(2, 7).expect("cancel 2");
        let _ = book.cancel(3, 9).expect("cancel 3");
        // Sanity
        for id in [1u64, 2, 3] {
            assert!(book.lookup(id).is_some(), "id {} should be cached", id);
        }
        // Evict up to 6 -> removes only id 1
        book.evict_closed_orders(6);
        assert!(book.lookup(1).is_none(), "id 1 should be evicted");
        assert!(
            book.lookup_by_client_id("u1".to_string(), "c1".to_string())
                .is_none(),
            "client map for id1 should be gone"
        );
        assert!(book.lookup(2).is_some(), "id 2 should remain");
        assert!(book.lookup(3).is_some(), "id 3 should remain");
        // Evict up to 9 -> removes remaining
        book.evict_closed_orders(9);
        assert!(book.lookup(2).is_none(), "id 2 should be evicted");
        assert!(book.lookup(3).is_none(), "id 3 should be evicted");
        assert!(
            book.lookup_by_client_id("u2".to_string(), "c2".to_string())
                .is_none()
        );
        assert!(
            book.lookup_by_client_id("u3".to_string(), "c3".to_string())
                .is_none()
        );
    }

    #[test]
    fn test_match_closed_then_evict_by_seq_from_matching() {
        let mut book = wb();
        // Add two orders that will cross fully: maker ask and taker bid
        assert!(book.add(o(1, "u1", "a1", Side::Ask, 100, 5)).is_ok());
        assert!(book.add(o(2, "u2", "b1", Side::Bid, 105, 5)).is_ok());
        // Match them; both should close and be cached in warm store
        let (_trades, closed) = book.match_orders();
        assert_eq!(closed.len(), 2, "both orders should close in this scenario");
        // Verify available in warm cache
        assert!(book.lookup(1).is_some());
        assert!(book.lookup(2).is_some());
        assert!(
            book.lookup_by_client_id("u1".to_string(), "a1".to_string())
                .is_some()
        );
        assert!(
            book.lookup_by_client_id("u2".to_string(), "b1".to_string())
                .is_some()
        );
        // Evict up to that seq
        book.evict_closed_orders(2);
        // Both should be gone now (they share the same seq in this simple full-cross)
        assert!(book.lookup(1).is_none());
        assert!(book.lookup(2).is_none());
        assert!(
            book.lookup_by_client_id("u1".to_string(), "a1".to_string())
                .is_none()
        );
        assert!(
            book.lookup_by_client_id("u2".to_string(), "b1".to_string())
                .is_none()
        );
    }

    #[test]
    fn test_match_multiple_groups_then_partial_evict() {
        let mut book = wb();
        // First crossing pair
        assert!(book.add(o(1, "u1", "a1", Side::Ask, 100, 4)).is_ok());
        assert!(book.add(o(2, "u2", "b1", Side::Bid, 100, 4)).is_ok());
        // Second crossing pair; use different ids to ensure different closed_by values
        assert!(book.add(o(10, "u10", "a10", Side::Ask, 99, 3)).is_ok());
        assert!(book.add(o(20, "u20", "b20", Side::Bid, 101, 3)).is_ok());
        let (_trades1, closed1) = book.match_orders();
        // closed1 should contain 4 orders (two pairs) but their closed_by may differ per pair
        assert_eq!(closed1.len(), 4, "two pairs should fully close");
        // Collect seq per pair
        let mut seqs: Vec<u64> = closed1.iter().map(|o| o.closed_by.unwrap()).collect();
        seqs.sort_unstable();
        seqs.dedup();
        assert!(
            seqs.len() >= 2,
            "expect at least two distinct seqs from two groups"
        );
        let first_seq = seqs[0];
        let second_seq = seqs[1];
        // After matching, all should be present
        for (id, u, c) in [
            (1u64, "u1", "a1"),
            (2, "u2", "b1"),
            (10, "u10", "a10"),
            (20, "u20", "b20"),
        ] {
            assert!(book.lookup(id).is_some(), "id {} should be cached", id);
            assert!(
                book.lookup_by_client_id(u.to_string(), c.to_string())
                    .is_some()
            );
        }
        // Evict only the first seq -> only one group should be removed
        book.evict_closed_orders(first_seq);
        let remaining = [1u64, 2, 10, 20]
            .iter()
            .filter(|&&id| book.lookup(id).is_some())
            .count();
        assert_eq!(
            remaining, 2,
            "after partial eviction, exactly two should remain"
        );
        // Evict the second seq -> clear the rest
        book.evict_closed_orders(second_seq);
        for (id, u, c) in [
            (1u64, "u1", "a1"),
            (2, "u2", "b1"),
            (10, "u10", "a10"),
            (20, "u20", "b20"),
        ] {
            assert!(book.lookup(id).is_none(), "id {} should be evicted", id);
            assert!(
                book.lookup_by_client_id(u.to_string(), c.to_string())
                    .is_none()
            );
        }
    }

    #[test]
    fn test_depth_delegates_to_underlying() {
        let mut book = wb();
        assert!(
            book.add(o(1, "u1", "c1", Side::Bid, 100, 3)).is_ok(),
            "add bid should succeed"
        );
        assert!(
            book.add(o(2, "u1", "c2", Side::Bid, 101, 4)).is_ok(),
            "add higher bid should succeed"
        );
        assert!(
            book.add(o(3, "u2", "c3", Side::Ask, 110, 2)).is_ok(),
            "add ask should succeed"
        );

        let depth = book.depth(2);
        assert_eq!(
            depth.bids.len(),
            2,
            "bids length should be 2 with limit=2, got: {}",
            depth.bids.len()
        );
        assert_eq!(
            depth.asks.len(),
            1,
            "asks length should be 1, got: {}",
            depth.asks.len()
        );
        assert_eq!(
            depth.bids[0].price, 101,
            "best bid price should be 101, got: {}",
            depth.bids[0].price
        );
        assert_eq!(
            depth.bids[0].volume, 4,
            "best bid volume should be 4, got: {}",
            depth.bids[0].volume
        );
        assert_eq!(
            depth.asks[0].price, 110,
            "best ask price should be 110, got: {}",
            depth.asks[0].price
        );
        assert_eq!(
            depth.asks[0].volume, 2,
            "best ask volume should be 2, got: {}",
            depth.asks[0].volume
        );
    }

    // --- SnapshotableBook tests ---

    #[test]
    fn test_snapshot_errors_and_batches() {
        let mut book = wb();
        // Prepare three closed orders
        assert!(book.add(o(1, "u1", "c1", Side::Bid, 100, 1)).is_ok());
        assert!(book.add(o(2, "u2", "c2", Side::Ask, 101, 2)).is_ok());
        assert!(book.add(o(3, "u3", "c3", Side::Bid, 99, 3)).is_ok());
        let _ = book.cancel(1, 10).unwrap();
        let _ = book.cancel(2, 11).unwrap();
        let _ = book.cancel(3, 12).unwrap();

        // Calling batch before take should error
        assert!(matches!(
            book.snapshot_batch(10),
            Err(Error::NoSnapshotTaken)
        ));
        assert!(matches!(book.end_snapshot(), Err(Error::NoSnapshotTaken)));

        // Take snapshot and ensure double take errors
        assert!(book.take_snapshot().is_ok());
        assert!(matches!(
            book.take_snapshot(),
            Err(Error::AnotherSnapshotAlreadyTaken)
        ));

        // Stream in two batches (limit=2 then limit=2)
        let b1 = book.snapshot_batch(2).unwrap().unwrap();
        assert_eq!(b1.len(), 2, "first batch should contain 2 orders");
        let b2 = book.snapshot_batch(2).unwrap().unwrap();
        assert_eq!(b2.len(), 1, "second batch should contain the last order");
        // No more orders
        assert!(book.snapshot_batch(1).unwrap().is_none());

        // End snapshot
        assert!(book.end_snapshot().is_ok());
        // No active snapshot anymore
        assert!(matches!(
            book.snapshot_batch(1),
            Err(Error::NoSnapshotTaken)
        ));
    }

    #[test]
    fn test_snapshot_isolation_with_eviction_and_new_closures() {
        let mut book = wb();
        // Close two orders at seq 5 and 6
        assert!(book.add(o(1, "u1", "c1", Side::Bid, 100, 1)).is_ok());
        assert!(book.add(o(2, "u2", "c2", Side::Ask, 101, 2)).is_ok());
        let _ = book.cancel(1, 5).unwrap();
        let _ = book.cancel(2, 6).unwrap();

        // Start snapshot
        assert!(book.take_snapshot().is_ok());

        // Evict everything up to 6 (would remove both from the live map)
        book.evict_closed_orders(6);
        // Also close a new order after snapshot start
        assert!(book.add(o(3, "u3", "c3", Side::Bid, 99, 3)).is_ok());
        let _ = book.cancel(3, 7).unwrap();

        // Even though we evicted and added new closures, the snapshot should still
        // stream the original two orders and not include id=3
        let mut total = 0usize;
        loop {
            let batch = book.snapshot_batch(10).unwrap();
            match batch {
                None => break,
                Some(v) => total += v.len(),
            }
        }
        assert_eq!(
            total, 2,
            "snapshot should contain exactly the two original orders"
        );
        // End snapshot to clear state
        assert!(book.end_snapshot().is_ok());
    }

    #[test]
    fn test_restore_snapshot_batch_rebuilds_structures() {
        let mut book = wb();
        // Build a batch with mixed seqs and one open order that should be ignored
        let mut o1 = o(1, "u1", "c1", Side::Bid, 100, 1);
        let mut o2 = o(2, "u2", "c2", Side::Ask, 101, 2);
        let o3 = o(3, "u3", "c3", Side::Bid, 99, 3);
        // Mark closed_by for first two; leave third as open (None) to be ignored
        o1.closed_by = Some(5);
        o2.closed_by = Some(7);
        // Restore
        assert!(
            book.restore_snapshot_batch(vec![o1.clone(), o2.clone(), o3.clone()])
                .is_ok()
        );
        // Lookups by client id should work for closed ones
        assert!(
            book.lookup_by_client_id("u1".to_string(), "c1".to_string())
                .is_some()
        );
        assert!(
            book.lookup_by_client_id("u2".to_string(), "c2".to_string())
                .is_some()
        );
        // The open one should not be present
        assert!(
            book.lookup_by_client_id("u3".to_string(), "c3".to_string())
                .is_none()
        );

        // Evict only seq 5 -> removes order 1; order 2 remains
        book.evict_closed_orders(5);
        assert!(book.lookup(1).is_none());
        assert!(book.lookup(2).is_some());
        // Evict up to 7 -> removes order 2 as well
        book.evict_closed_orders(7);
        assert!(book.lookup(2).is_none());
    }
}
