//! Internal utility helpers used by the API layer.

use axum::extract::ConnectInfo;
use http::{HeaderMap, Request};
use std::net::SocketAddr;

/// Try to infer the remote client's IP address as a string.
///
/// Priority:
/// 1) First IP from the X-Forwarded-For header if present.
/// 2) Socket address from Axum's ConnectInfo extension.
/// 3) Fallback to "unknown".
pub(super) fn remote_ip<B>(req: &Request<B>) -> String {
    if let Some(ip) = x_forwarded_for(req.headers()) {
        return ip;
    }
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        return addr.ip().to_string();
    }

    String::from("unknown")
}

/// Extract the first IP from the X-Forwarded-For header when present.
fn x_forwarded_for(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("x-forwarded-for")?.to_str().ok()?;
    let first = v.split(',').next()?.trim();
    if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}
