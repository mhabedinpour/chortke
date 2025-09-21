use crate::order::book::{Book, Depth, DepthItem, Error};
use crate::order::{Id, Order, Price, Side, Volume};
use slab::Slab;
use std::cmp;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Default)]
struct PriceLevel {
    head: Option<usize>,
    tail: Option<usize>,
    total_volume: Volume,
    total_orders: usize,
}

impl PriceLevel {
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

#[derive(Debug)]
struct OrderNode {
    order: Order,
    seq: usize,
    next: Option<usize>,
    prev: Option<usize>,
}

#[derive(Debug, Default)]
pub struct TreeMap {
    bids: BTreeMap<Price, PriceLevel>,
    asks: BTreeMap<Price, PriceLevel>,
    orders: Slab<OrderNode>,
    order_indexes: HashMap<Id, usize>,
    last_seq: usize,
}

impl TreeMap {
    pub fn new() -> Self {
        TreeMap::default()
    }

    fn remove_order_from_level(&mut self, idx: usize) {
        let price = self.orders[idx].order.price;
        let side = self.orders[idx].order.side;
        self.order_indexes.remove(&self.orders[idx].order.id);
        let level = match side {
            Side::Buy => self.bids.get_mut(&price).unwrap(),
            Side::Sell => self.asks.get_mut(&price).unwrap(),
        };
        level.remove(&mut self.orders, idx);
        self.orders.remove(idx);
        if level.total_orders == 0 {
            match side {
                Side::Buy => self.bids.remove(&price),
                Side::Sell => self.asks.remove(&price),
            };
        }
    }

    fn remove_by_volume(&mut self, level: &mut PriceLevel, volume: Volume) {
        assert!(volume <= level.total_volume);

        let mut remaining_volume = volume;
        while remaining_volume > 0 && level.total_volume > 0 {
            let idx = level.head.unwrap();

            // if the remaining volume is higher than the remaining volume in the order, remove the order completely
            if self.orders[idx].order.remaining_volume() <= remaining_volume {
                remaining_volume -= self.orders[idx].order.remaining_volume();
                self.remove_order_from_level(idx);
                continue;
            }

            // if the remaining volume is lower than the remaining volume in the order, execute the order partially
            level.total_volume -= self.orders[idx].order.remaining_volume();
            self.orders[idx].order.executed_volume += remaining_volume;
            break;
        }
    }
}

impl Book for TreeMap {
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
            Side::Buy => self.bids.entry(self.orders[idx].order.price).or_default(),
            Side::Sell => self.asks.entry(self.orders[idx].order.price).or_default(),
        };
        level.push(&mut self.orders, idx);
        self.last_seq += 1;

        Ok(())
    }

    fn cancel(&mut self, id: Id) -> Result<(), Error> {
        let idx = self.order_indexes.get(&id);
        if idx.is_none() {
            return Err(Error::OrderNotFound);
        }

        self.remove_order_from_level(*idx.unwrap());

        Ok(())
    }

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

    fn match_orders(&mut self) {
        let asks_ptr: *mut BTreeMap<Price, PriceLevel> = &mut self.asks;
        let bids_ptr: *mut BTreeMap<Price, PriceLevel> = &mut self.bids;

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
            let volume = cmp::min(top_bid_level.total_volume, top_ask_level.total_volume);
            if volume == 0 {
                break;
            }

            self.remove_by_volume(top_bid_level, volume);
            self.remove_by_volume(top_ask_level, volume);

            // TODO: generate trades
        }
    }
}
