#[derive(Debug, Copy, Clone)]
pub enum Side {
    Bid,
    Ask
}

#[derive(Debug, Copy, Clone)]
pub enum Status {
    Open,
    Executed,
    Canceled
}

pub type Id = u64;
pub type ClientId = String;
pub type Price = u64; // ticks
pub type Volume = u64;

#[derive(Debug)]
pub struct Order {
    pub id: Id,
    pub client_id: ClientId,
    pub side: Side,
    pub price: Price,
    pub executed_price: Option<Price>,
    pub volume: Volume,
    pub executed_volume: Volume,
    pub status: Status,
}

impl Order {
    pub fn new(id: Id, client_id: ClientId, side: Side, price: Price, volume: Volume) -> Self {
        Order {
            id,
            client_id,
            side,
            price,
            executed_price: None,
            volume,
            executed_volume: 0,
            status: Status::Open,
        }
    }

    pub fn remaining_volume(&self) -> Volume {
        self.volume - self.executed_volume
    }
}

pub mod book;