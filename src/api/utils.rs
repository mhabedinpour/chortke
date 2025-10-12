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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ConnectInfo;
    use http::Request;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[test]
    fn parses_x_forwarded_for_first_ip() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            http::HeaderValue::from_static("1.2.3.4, 5.6.7.8"),
        );
        let ip = x_forwarded_for(&headers);
        assert_eq!(ip.as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn x_forwarded_for_empty_value_is_none() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", http::HeaderValue::from_static("   "));
        assert!(x_forwarded_for(&headers).is_none());

        let headers = HeaderMap::new();
        assert!(x_forwarded_for(&headers).is_none());
    }

    #[test]
    fn remote_ip_prefers_header_over_connect_info() {
        let mut req = Request::builder()
            .header("x-forwarded-for", "9.9.9.9, 8.8.8.8")
            .body(())
            .unwrap();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 12345);
        req.extensions_mut().insert(ConnectInfo(addr));

        let ip = remote_ip(&req);
        assert_eq!(ip, "9.9.9.9");
    }

    #[test]
    fn remote_ip_from_connect_info_when_no_header() {
        let mut req = Request::builder().body(()).unwrap();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 23456);
        req.extensions_mut().insert(ConnectInfo(addr));

        let ip = remote_ip(&req);
        assert_eq!(ip, "10.0.0.2");
    }

    #[test]
    fn remote_ip_unknown_when_no_info() {
        let req = Request::builder().body(()).unwrap();
        let ip = remote_ip(&req);
        assert_eq!(ip, "unknown");
    }
}
