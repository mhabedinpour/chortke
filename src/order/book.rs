pub mod tree_map;

use crate::order::{Id, Order, Price, Volume};

#[derive(Debug, Copy, Clone)]
pub struct DepthItem {
    pub price: Price,
    pub volume: Volume,
}

#[derive(Debug, Clone)]
pub struct Depth {
    pub bids: Vec<DepthItem>,
    pub asks: Vec<DepthItem>,
}

#[derive(Debug)]
pub enum Error {
    OrderNotFound,
    OrderExists,
}

// TODO: add prometheus metrics
// TODO: think more about error handling

pub trait Book {
    fn add(&mut self, order: Order) -> Result<(), Error>;
    fn cancel(&mut self, id: Id) -> Result<(), Error>;
    fn depth(&self, limit: usize) -> Depth;
    fn match_orders(&mut self) -> Vec<crate::trade::Trade>;
}
