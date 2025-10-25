use crate::order::book::warm_book::WarmBook;
use crate::order::book::{Depth, Error, HotBook, SnapshotableBook};
use crate::order::{ClientId, Id, Order};
use crate::seq::Seq;
use crate::user;

/// A thin orchestrator over an order book that:
/// - assigns sequential IDs to new orders,
/// - forwards operations to the underlying WarmBook,
/// - guards the sequence counter by rolling it back on failures,
/// - and provides snapshot metadata for external persistence.
pub struct Matcher<T: HotBook + SnapshotableBook> {
    book: WarmBook<T>,
    last_seq: Seq,
    last_archived_seq: Seq,
    ready: bool,
}

/// Minimal metadata returned together with a snapshot stream.
///
/// This allows a consumer to persist snapshot progress and later restore the
/// matcher state using [`Matcher::init`].
pub struct SnapshotMetadata {
    /// The last known sequence value at the time the snapshot started.
    pub last_seq: Seq,
    /// The last archived (durably persisted) sequence value known to the caller.
    pub last_archived_seq: Seq,
}

impl<T: HotBook + SnapshotableBook> Matcher<T> {
    /// Create a new matcher around the given hot book and initial sequence.
    pub fn new(book: T) -> Self {
        Self {
            book: WarmBook::new(book),
            last_seq: 0,
            last_archived_seq: 0,
            ready: false,
        }
    }

    /// Add a new order, assigning it the next sequential ID.
    ///
    /// On error the internal sequence is rolled back to avoid gaps.
    pub fn add_order(&mut self, mut order: Order) -> Result<(), Error> {
        if !self.ready {
            return Err(Error::NotReady);
        }

        self.last_seq += 1;
        order.id = self.last_seq;

        match self.book.add(order) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.last_seq -= 1;
                Err(e)
            }
        }
    }

    /// Run the matching engine until no more crossing prices remain.
    pub fn match_orders(&mut self) -> Result<(Vec<crate::trade::Trade>, Vec<Order>), Error> {
        if !self.ready {
            return Err(Error::NotReady);
        }

        Ok(self.book.match_orders())
    }

    /// Convenience function: add an order and immediately try to match.
    pub fn add_and_match(
        &mut self,
        order: Order,
    ) -> Result<(Vec<crate::trade::Trade>, Vec<Order>), Error> {
        self.add_order(order)?;
        self.match_orders()
    }

    /// Cancel an order by its internal ID.
    ///
    /// The cancel operation consumes a new sequence number; on failure the
    /// sequence is rolled back to avoid gaps.
    pub fn cancel_order(&mut self, id: Id) -> Result<Order, Error> {
        if !self.ready {
            return Err(Error::NotReady);
        }

        self.last_seq += 1;
        match self.book.cancel(id, self.last_seq) {
            Ok(order) => Ok(order),
            Err(e) => {
                self.last_seq -= 1;
                Err(e)
            }
        }
    }

    /// Cancel an order by `(user_id, client_id)` pair.
    ///
    /// Consumes a new sequence on success; rolls back on error.
    pub fn cancel_by_client_id(
        &mut self,
        user_id: user::Id,
        client_id: ClientId,
    ) -> Result<Order, Error> {
        if !self.ready {
            return Err(Error::NotReady);
        }

        self.last_seq += 1;
        match self
            .book
            .cancel_by_client_id(user_id, client_id, self.last_seq)
        {
            Ok(order) => Ok(order),
            Err(e) => {
                self.last_seq -= 1;
                Err(e)
            }
        }
    }

    /// Get aggregated depth up to `limit` levels per side.
    pub fn depth(&self, limit: usize) -> Result<Depth, Error> {
        if !self.ready {
            return Err(Error::NotReady);
        }

        Ok(self.book.depth(limit))
    }

    /// Lookup an order by internal `Id` from active or recently closed orders.
    pub fn lookup(&self, id: Id) -> Result<Option<Order>, Error> {
        if !self.ready {
            return Err(Error::NotReady);
        }

        Ok(self.book.lookup(id))
    }

    /// Lookup an order by `(user_id, client_id)`.
    pub fn lookup_by_client_id(
        &self,
        user_id: user::Id,
        client_id: ClientId,
    ) -> Result<Option<Order>, Error> {
        if !self.ready {
            return Err(Error::NotReady);
        }

        Ok(self.book.lookup_by_client_id(user_id, client_id))
    }

    /// Begin a snapshot on the underlying book and return associated metadata.
    pub fn take_snapshot(&mut self) -> Result<SnapshotMetadata, Error> {
        if !self.ready {
            return Err(Error::NotReady);
        }

        self.book.take_snapshot()?;
        Ok(SnapshotMetadata {
            last_seq: self.last_seq,
            last_archived_seq: self.last_archived_seq,
        })
    }

    /// End the currently active snapshot session.
    pub fn end_snapshot(&mut self) -> Result<(), Error> {
        self.book.end_snapshot()
    }

    /// Retrieve up to `limit` orders from the in-progress snapshot.
    pub fn snapshot_batch(&mut self, limit: usize) -> Result<Option<Vec<Order>>, Error> {
        self.book.snapshot_batch(limit)
    }

    /// Restore a batch of orders (previously taken via snapshot) into the book.
    pub fn restore_snapshot_batch(&mut self, orders: Vec<Order>) -> Result<(), Error> {
        self.book.restore_snapshot_batch(orders)
    }

    /// Initialize the matcher from snapshot metadata.
    pub fn init(&mut self, metadata: SnapshotMetadata) {
        self.last_seq = metadata.last_seq;
        self.last_archived_seq = metadata.last_archived_seq;

        self.ready = true;
    }

    // Updates last archived seq and evicts closed orders from the warm book up to that seq.
    pub fn set_archived_seq(&mut self, seq: Seq) {
        self.last_archived_seq = seq;
        self.book.evict_closed_orders(seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::order::book::tree_map::TreeMap;
    use crate::order::{Side, Status};

    fn new_matcher() -> Matcher<TreeMap> {
        let mut m = Matcher::new(TreeMap::new());
        m.init(SnapshotMetadata {
            last_seq: 0,
            last_archived_seq: 0,
        });

        m
    }

    fn o(user: &str, client: &str, side: Side, price: u64, vol: u64) -> Order {
        Order::new(0, user.to_string(), client.to_string(), side, price, vol)
    }

    #[test]
    fn add_order_assigns_sequential_ids_and_rolls_back_on_error() {
        let mut m = new_matcher();
        // First add assigns id=1
        let bid1 = o("u1", "c1", Side::Bid, 100, 10);
        m.add_order(bid1).unwrap();
        let added = m.lookup(1).unwrap().unwrap();
        assert_eq!(added.id, 1);

        // Duplicate client id should error and NOT consume a sequence
        let dup = o("u1", "c1", Side::Bid, 101, 5);
        let err = m.add_order(dup).unwrap_err();
        matches!(err, Error::OrderClientIdExists(_));

        // Next successful add should get id=2 (no gap)
        let bid2 = o("u1", "c2", Side::Bid, 100, 7);
        m.add_order(bid2).unwrap();
        assert_eq!(m.lookup(2).unwrap().unwrap().id, 2);
    }

    #[test]
    fn add_and_match_crossing_produces_trades_and_closes_orders() {
        let mut m = new_matcher();
        m.add_order(o("u1", "b1", Side::Bid, 100, 10)).unwrap(); // id=1
        let (trades, closed) = m.add_and_match(o("u2", "a1", Side::Ask, 90, 10)).unwrap();
        assert!(!trades.is_empty());
        assert_eq!(closed.len(), 2);
        // Both orders should be fully executed and available via lookup
        for id in [1, 2] {
            let ord = m.lookup(id).unwrap().unwrap();
            assert!(matches!(ord.status, Status::Executed));
            assert!(ord.closed_by.is_some());
        }
    }

    #[test]
    fn cancel_order_consumes_seq_and_sets_closed_by_seq() {
        let mut m = new_matcher();
        m.add_order(o("u1", "c1", Side::Bid, 100, 10)).unwrap(); // id=1
        let canceled = m.cancel_order(1).unwrap(); // consumes seq=2
        assert!(matches!(canceled.status, Status::Canceled));
        assert_eq!(canceled.closed_by, Some(2));

        // Next add should get id=3
        m.add_order(o("u1", "c2", Side::Bid, 100, 1)).unwrap();
        assert_eq!(m.lookup(3).unwrap().unwrap().id, 3);
    }

    #[test]
    fn cancel_by_client_id_paths_and_seq_rollback_on_error() {
        let mut m = new_matcher();
        m.add_order(o("u1", "c1", Side::Ask, 100, 5)).unwrap(); // id=1
        let canceled = m
            .cancel_by_client_id("u1".to_string(), "c1".to_string())
            .unwrap(); // consumes seq=2
        assert_eq!(canceled.id, 1);
        assert!(matches!(canceled.status, Status::Canceled));
        assert_eq!(canceled.closed_by, Some(2));

        // Failing cancel should NOT consume a sequence
        let err = m
            .cancel_by_client_id("u1".to_string(), "unknown".to_string())
            .unwrap_err();
        matches!(err, Error::OrderClientIdNotFound(_));

        // Next add should get id=3
        m.add_order(o("u1", "c2", Side::Ask, 101, 1)).unwrap();
        assert_eq!(m.lookup(3).unwrap().unwrap().id, 3);

        // lookup_by_client_id should work for active order by client id
        let ord = m
            .lookup_by_client_id("u1".to_string(), "c2".to_string())
            .unwrap()
            .unwrap();
        assert_eq!(ord.id, 3);
    }

    #[test]
    fn depth_and_lookup_passthrough() {
        let mut m = new_matcher();
        m.add_order(o("u1", "b1", Side::Bid, 100, 3)).unwrap(); // id=1
        m.add_order(o("u1", "b2", Side::Bid, 101, 2)).unwrap(); // id=2
        m.add_order(o("u2", "a1", Side::Ask, 105, 4)).unwrap(); // id=3

        let depth = m.depth(2).unwrap();
        assert_eq!(depth.bids.len(), 2);
        assert_eq!(depth.bids[0].price, 101);
        assert_eq!(depth.bids[1].price, 100);
        assert_eq!(depth.asks.len(), 1);
        assert_eq!(depth.asks[0].price, 105);

        assert_eq!(m.lookup(2).unwrap().unwrap().client_id, "b2");
    }

    #[test]
    fn snapshot_lifecycle_and_restore() {
        let mut m = new_matcher();
        // Non-crossing orders to keep them open
        m.add_order(o("u1", "b1", Side::Bid, 100, 3)).unwrap(); // id=1
        m.add_order(o("u2", "a1", Side::Ask, 110, 2)).unwrap(); // id=2
        m.add_order(o("u3", "a2", Side::Ask, 120, 1)).unwrap(); // id=3

        let meta = m.take_snapshot().unwrap();
        assert_eq!(meta.last_seq, 3);

        let mut all = Vec::new();
        while let Some(batch) = m.snapshot_batch(2).unwrap() {
            all.extend(batch);
        }
        assert_eq!(all.len(), 3);
        m.end_snapshot().unwrap();

        // Restore into a fresh matcher
        let mut m2 = new_matcher();
        m2.restore_snapshot_batch(all).unwrap();
        let depth = m2.depth(10).unwrap();
        assert_eq!(depth.bids.len(), 1);
        assert_eq!(depth.asks.len(), 2);
    }
}
