use axum::extract::ConnectInfo;
use http::{HeaderMap, Request};
use std::net::SocketAddr;

pub(super) fn remote_ip<B>(req: &Request<B>) -> String {
    if let Some(ip) = x_forwarded_for(req.headers()) {
        return ip;
    }
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        return addr.ip().to_string();
    }

    String::from("unknown")
}

fn x_forwarded_for(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("x-forwarded-for")?.to_str().ok()?;
    let first = v.split(',').next()?.trim();
    if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}
