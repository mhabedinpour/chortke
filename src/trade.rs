use time::OffsetDateTime;
use crate::order;

pub type Id = u64;

#[derive(Debug)]
pub struct Trade {
    pub id: Id,
    pub bid_order_id: order::Id,
    pub ask_order_id: order::Id,
    pub is_bid_maker: bool,
    pub price: order::Price,
    pub volume: order::Volume,
    pub timestamp: OffsetDateTime,
}