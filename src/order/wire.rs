//! Binary wire format for orders.
//!
//! This module defines a compact, versioned wire schema used to serialize and
//! deserialize orders for persistence or network transport. The encoding uses
//! bincode with fixed-int encoding, wrapped in zstd compression for smaller
//! payloads. The public Order type is converted into a stable, explicit wire
//! representation (OrderV1) so we can evolve the internal Order struct without
//! breaking backwards compatibility.
//!
//! Encoding pipeline:
//! 1) Order -> OrderV1 (stable schema)
//! 2) bincode (fixint) -> bytes
//! 3) zstd level 3 compression -> final payload
//!
//! Decoding does the reverse. The schema_version field allows forward migration
//! in the future when new fields are added.

use super::{ClientId, Id, Order, Price, Volume};
use crate::user;
use bincode::Options;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::io;
use std::io::Cursor;
use thiserror::Error;

/// Wire-level order side.
/// Maps 1:1 to the public Side but is frozen to a specific repr for stability.
#[derive(Debug, Copy, Clone, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Side {
    Bid = 0,
    Ask = 1,
}

impl From<super::Side> for Side {
    fn from(s: super::Side) -> Self {
        match s {
            super::Side::Bid => Side::Bid,
            super::Side::Ask => Side::Ask,
        }
    }
}

impl From<Side> for super::Side {
    fn from(value: Side) -> Self {
        match value {
            Side::Bid => super::Side::Bid,
            Side::Ask => super::Side::Ask,
        }
    }
}

/// Wire-level order status with a stable numeric representation.
#[derive(Debug, Copy, Clone, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Status {
    Open = 1,
    Executed = 2,
    Canceled = 3,
}

impl From<super::Status> for Status {
    fn from(s: super::Status) -> Self {
        match s {
            super::Status::Open => Status::Open,
            super::Status::Executed => Status::Executed,
            super::Status::Canceled => Status::Canceled,
        }
    }
}

impl From<Status> for super::Status {
    fn from(value: Status) -> Self {
        match value {
            Status::Open => super::Status::Open,
            Status::Executed => super::Status::Executed,
            Status::Canceled => super::Status::Canceled,
        }
    }
}

/// Version 1 of the order wire schema.
/// This is intentionally explicit and uses only stable scalar types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderV1 {
    pub id: u64,
    pub user_id: String,
    pub client_id: String,
    pub side: Side,
    pub price: u64,
    pub executed_price: Option<u64>,
    pub volume: u64,
    pub executed_volume: u64,
    pub status: Status,
    pub closed_by: Option<u64>,
}

impl From<&Order> for OrderV1 {
    fn from(o: &Order) -> Self {
        OrderV1 {
            id: o.id,
            user_id: o.user_id.clone(),
            client_id: o.client_id.clone(),
            side: o.side.into(),
            price: o.price,
            executed_price: o.executed_price,
            volume: o.volume,
            executed_volume: o.executed_volume,
            status: o.status.into(),
            closed_by: o.closed_by,
        }
    }
}

impl From<OrderV1> for Order {
    fn from(w: OrderV1) -> Self {
        Order {
            id: Id::from(w.id),
            user_id: user::Id::from(w.user_id),
            client_id: ClientId::from(w.client_id),
            side: w.side.into(),
            price: Price::from(w.price),
            executed_price: w.executed_price,
            volume: Volume::from(w.volume),
            executed_volume: Volume::from(w.executed_volume),
            status: w.status.into(),
            closed_by: w.closed_by,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderBatch {
    version: u16,
    checksum: u64,
    orders: Vec<u8>,
}

impl OrderBatch {
    fn calc_checksum(encoded_orders: &[u8]) -> u64 {
        use blake3::Hasher;
        let mut hasher = Hasher::new();
        hasher.update(encoded_orders);
        let hash = hasher.finalize();
        u64::from_le_bytes(hash.as_bytes()[0..8].try_into().unwrap())
    }
}

/// Errors returned by encoding/decoding operations.
#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to Encode/Decode: {0}")]
    BinCode(#[from] bincode::Error),
    #[error("Failed to Compress/Decompress: {0}")]
    Io(#[from] io::Error),
    #[error("Unsupported schema version: {0}")]
    UnsupportedVersion(u16),
    #[error("Checksum mismatch")]
    ChecksumMismatch,
}

/// Encode a batch of orders into a compressed binary payload.
///
/// The output is zstd-compressed bincode bytes of Vec<OrderV1>.
pub fn encode_orders_batch(orders: &[Order]) -> Result<Vec<u8>, Error> {
    let opts = bincode::DefaultOptions::new().with_fixint_encoding();
    let orders = orders.iter().map(OrderV1::from).collect::<Vec<OrderV1>>();
    let encoded_orders = opts.serialize(&orders)?;
    let payload = opts.serialize(&OrderBatch {
        version: 1,
        checksum: OrderBatch::calc_checksum(&encoded_orders),
        orders: encoded_orders,
    })?;
    Ok(zstd::stream::encode_all(&payload[..], 3)?)
}

/// Decode a batch of orders from a compressed binary payload produced by
/// encode_orders_batch.
pub fn decode_orders_batch(bytes: &[u8]) -> Result<Vec<Order>, Error> {
    let dec = zstd::stream::decode_all(bytes)?;
    let opts = bincode::DefaultOptions::new().with_fixint_encoding();
    let mut rdr = Cursor::new(&dec);
    let version: u16 = opts.deserialize_from(&mut rdr)?;
    rdr.set_position(rdr.position() - 2);

    match version {
        1 => {
            let batch: OrderBatch = opts.deserialize_from(&mut rdr)?;
            let orders: Vec<OrderV1> = opts.deserialize_from(batch.orders.as_slice())?;
            if batch.checksum != OrderBatch::calc_checksum(&batch.orders) {
                return Err(Error::ChecksumMismatch);
            }
            Ok(orders.into_iter().map(Order::from).collect())
        }
        other => Err(Error::UnsupportedVersion(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_order(id: u64) -> Order {
        Order::new(
            id,
            user::Id::from("u-1".to_string()),
            "c-1".to_string(),
            super::super::Side::Bid,
            10,
            5,
        )
    }

    #[test]
    fn side_status_conversion_roundtrip() {
        // Side
        let s_bid_back: super::super::Side = Side::from(super::super::Side::Bid).into();
        match s_bid_back {
            super::super::Side::Bid => {}
            _ => panic!("expected Bid"),
        }
        let s_ask_back: super::super::Side = Side::from(super::super::Side::Ask).into();
        match s_ask_back {
            super::super::Side::Ask => {}
            _ => panic!("expected Ask"),
        }

        // Status
        let st_open_back: super::super::Status = Status::from(super::super::Status::Open).into();
        match st_open_back {
            super::super::Status::Open => {}
            _ => panic!("expected Open"),
        }
        let st_exec_back: super::super::Status =
            Status::from(super::super::Status::Executed).into();
        match st_exec_back {
            super::super::Status::Executed => {}
            _ => panic!("expected Executed"),
        }
        let st_cancel_back: super::super::Status =
            Status::from(super::super::Status::Canceled).into();
        match st_cancel_back {
            super::super::Status::Canceled => {}
            _ => panic!("expected Canceled"),
        }
    }

    #[test]
    fn encode_decode_roundtrip_single() {
        let order = sample_order(7);
        let bytes = encode_orders_batch(&[order.clone()]).expect("encode");
        let decoded = decode_orders_batch(&bytes).expect("decode");
        assert_eq!(
            decoded.len(),
            1,
            "decoded vector length mismatch: got {}, expected 1",
            decoded.len()
        );
        let back = &decoded[0];
        assert_eq!(
            back.id, order.id,
            "id mismatch after encode/decode: got {}, expected {}",
            back.id, order.id
        );
        assert_eq!(
            back.user_id, order.user_id,
            "user_id mismatch: got {:?}, expected {:?}",
            back.user_id, order.user_id
        );
        assert_eq!(
            back.client_id, order.client_id,
            "client_id mismatch: got {:?}, expected {:?}",
            back.client_id, order.client_id
        );
        assert_eq!(
            back.side as u8, order.side as u8,
            "side mismatch: got {}, expected {}",
            back.side as u8, order.side as u8
        );
        assert_eq!(
            back.price, order.price,
            "price mismatch: got {}, expected {}",
            back.price, order.price
        );
        assert_eq!(
            back.executed_price, order.executed_price,
            "executed_price mismatch: got {:?}, expected {:?}",
            back.executed_price, order.executed_price
        );
        assert_eq!(
            back.volume, order.volume,
            "volume mismatch: got {}, expected {}",
            back.volume, order.volume
        );
        assert_eq!(
            back.executed_volume, order.executed_volume,
            "executed_volume mismatch: got {}, expected {}",
            back.executed_volume, order.executed_volume
        );
        match (back.status, order.status) {
            (super::super::Status::Open, super::super::Status::Open) => {}
            (a, b) => panic!("status mismatch: {:?} != {:?}", a, b),
        }
        assert_eq!(
            back.closed_by, order.closed_by,
            "closed_by mismatch: got {:?}, expected {:?}",
            back.closed_by, order.closed_by
        );
    }

    #[test]
    fn encode_decode_roundtrip_multiple() {
        let orders: Vec<Order> = (1..=5).map(sample_order).collect();
        let bytes = encode_orders_batch(&orders).expect("encode");
        let decoded = decode_orders_batch(&bytes).expect("decode");
        assert_eq!(
            decoded.len(),
            orders.len(),
            "decoded length mismatch: got {}, expected {}",
            decoded.len(),
            orders.len()
        );
        for (a, b) in decoded.iter().zip(orders.iter()) {
            assert_eq!(
                a.id, b.id,
                "order id mismatch at position; got {}, expected {}",
                a.id, b.id
            );
        }
    }

    #[test]
    fn decode_fails_on_corrupted_payload() {
        // Not a valid zstd frame
        let bad = b"not-a-zstd-frame";
        let err = decode_orders_batch(bad).unwrap_err();
        // zstd errors are mapped to Error::Io
        match err {
            Error::Io(_) => {}
            e => panic!("unexpected error variant: {e:?}"),
        }

        // Produce valid zstd but corrupt bincode inside
        let invalid_inside = vec![0xFF, 0xFF, 0xFF, 0xFF];
        let compressed = zstd::stream::encode_all(&invalid_inside[..], 1).expect("zstd");
        let err2 = decode_orders_batch(&compressed).unwrap_err();
        match err2 {
            Error::UnsupportedVersion(_) => {}
            e => panic!("unexpected error variant: {e:?}"),
        }
    }
}
