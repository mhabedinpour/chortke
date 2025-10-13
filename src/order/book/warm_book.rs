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

use crate::order::book::{Depth, Error, HotBook};
use crate::order::{ClientId, Id, Order};
use crate::user;
use std::collections::HashMap;

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
    closed_orders: HashMap<Id, Order>,
}

impl<T: HotBook> WarmBook<T> {
    /// Create a new warm book wrapper over the provided hot book.
    pub fn new(hot_book: T) -> Self {
        WarmBook {
            hot_book,
            client_id_to_order_id: HashMap::new(),
            closed_orders: HashMap::new(),
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
    pub fn cancel(&mut self, id: Id) -> Result<Order, Error> {
        match self.hot_book.cancel(id) {
            Ok(order) => {
                self.closed_orders.insert(id, order.clone());
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
    ) -> Result<Order, Error> {
        let id = self
            .client_id_to_order_id
            .get(&Order::format_user_client_id(&user_id, &client_id));
        match id {
            Some(id) => self.cancel(*id),
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
        self.closed_orders
            .extend(closed_orders.iter().map(|o| (o.id, o.clone())));

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
}

#[cfg(test)]
mod tests {
    use super::WarmBook;
    use crate::order::book::{Error, tree_map::TreeMap};
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

        let canceled = book.cancel(1).expect("cancel by id should succeed");
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
            matches!(book.cancel_by_client_id("uX".to_string(), "nope".to_string()), Err(Error::OrderClientIdNotFound(cid)) if cid=="nope"),
            "cancel_by_client_id should error when unknown; got different result"
        );

        assert!(
            book.add(o(1, "u1", "c1", Side::Bid, 100, 10)).is_ok(),
            "add should succeed"
        );
        let canceled = book
            .cancel_by_client_id("u1".to_string(), "c1".to_string())
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
        let _ = book.cancel(1).expect("cancel should succeed");
        let closed = book.lookup_by_client_id("u1".to_string(), "c1".to_string());
        assert!(
            closed.is_some(),
            "should find closed order by client id in warm store, got: {:?}",
            closed
        );
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
}
