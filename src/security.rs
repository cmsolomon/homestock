//! Request hardening: failed-password throttling, client addresses behind a trusted
//! proxy, and a cross-site request check.
use crate::{error::AppError, AppState};
use axum::{
    extract::{ConnectInfo, FromRef, FromRequestParts, Request},
    http::{header, request::Parts, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_FAILURES: usize = 5;
const WINDOW: Duration = Duration::from_secs(60);

/// Failed password checks per client IP: at most `MAX_FAILURES` in any rolling minute.
/// In memory only; a restart forgets it, which is fine for a household LAN app.
#[derive(Default)]
pub struct PasswordLimiter(Mutex<HashMap<IpAddr, Vec<Instant>>>);

impl PasswordLimiter {
    pub fn allowed(&self, ip: IpAddr) -> bool {
        self.allowed_at(ip, Instant::now())
    }
    pub fn failed(&self, ip: IpAddr) {
        self.failed_at(ip, Instant::now())
    }
    pub fn succeeded(&self, ip: IpAddr) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(&ip);
        }
    }

    fn allowed_at(&self, ip: IpAddr, now: Instant) -> bool {
        let Ok(mut map) = self.0.lock() else {
            return false;
        };
        let Some(failures) = map.get_mut(&ip) else {
            return true;
        };
        failures.retain(|t| now.duration_since(*t) < WINDOW);
        failures.len() < MAX_FAILURES
    }

    fn failed_at(&self, ip: IpAddr, now: Instant) {
        let Ok(mut map) = self.0.lock() else { return };
        // Bound memory if many addresses fail: drop everyone outside the window.
        if map.len() > 1024 {
            map.retain(|_, failures| failures.iter().any(|t| now.duration_since(*t) < WINDOW));
        }
        map.entry(ip).or_default().push(now);
    }
}

/// The address a request came from, for per-client limits. Behind a reverse proxy listed in
/// `trusted_proxies` (e.g. HAProxy on OPNsense with X-Forwarded-For enabled) every connection
/// comes from the proxy, so use the address it appended instead.
pub struct ClientIp(pub IpAddr);

impl<S> FromRequestParts<S> for ClientIp
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let st = AppState::from_ref(state);
        let ConnectInfo(peer) = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .ok_or_else(|| AppError::internal("connection info is not available"))?;
        Ok(ClientIp(client_ip(
            peer.ip(),
            &parts.headers,
            &st.cfg.trusted_proxies,
        )))
    }
}

/// Only the last X-Forwarded-For entry is used: the proxy appends the address it saw, and
/// anything before it was sent by the client and can be forged. A request that did not come
/// from a trusted proxy keeps its own peer address, whatever headers it carries.
fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted_proxies: &[IpAddr]) -> IpAddr {
    let peer = peer.to_canonical();
    if !trusted_proxies
        .iter()
        .any(|proxy| proxy.to_canonical() == peer)
    {
        return peer;
    }
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .last()
        .and_then(|entry| entry.trim().parse::<IpAddr>().ok())
        .map_or(peer, |ip| ip.to_canonical())
}

/// Reject state-changing requests started by another site. SameSite=Lax cookies already stop
/// other *sites*, but sibling hosts on the same domain (another `*.home.lan` service) count
/// as same-site, so check `Sec-Fetch-Site`, falling back to `Origin` for older browsers.
pub async fn same_origin(req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        && !is_same_origin(req.headers(), req.uri().authority().map(|a| a.as_str()))
    {
        return (StatusCode::FORBIDDEN, "Cross-site request blocked").into_response();
    }
    next.run(req).await
}

fn is_same_origin(h: &HeaderMap, uri_authority: Option<&str>) -> bool {
    if let Some(site) = h.get("Sec-Fetch-Site").and_then(|v| v.to_str().ok()) {
        return matches!(site, "same-origin" | "none");
    }
    let Some(origin) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    // HTTP/2 carries the host in the URI authority rather than a Host header.
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or(uri_authority);
    origin.split_once("://").map(|(_, authority)| authority) == host
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn fetch_metadata_decides_when_present() {
        assert!(is_same_origin(
            &headers(&[("sec-fetch-site", "same-origin")]),
            None
        ));
        assert!(is_same_origin(
            &headers(&[("sec-fetch-site", "none")]),
            None
        ));
        assert!(!is_same_origin(
            &headers(&[
                ("sec-fetch-site", "same-site"),
                ("origin", "https://stock.lan"),
                ("host", "stock.lan")
            ]),
            None
        ));
        assert!(!is_same_origin(
            &headers(&[("sec-fetch-site", "cross-site")]),
            None
        ));
    }

    #[test]
    fn origin_must_match_host_without_fetch_metadata() {
        assert!(is_same_origin(
            &headers(&[
                ("origin", "https://10.0.0.5:8443"),
                ("host", "10.0.0.5:8443")
            ]),
            None
        ));
        assert!(is_same_origin(
            &headers(&[("origin", "https://10.0.0.5:8443")]),
            Some("10.0.0.5:8443")
        ));
        assert!(!is_same_origin(
            &headers(&[
                ("origin", "https://nas.home.lan"),
                ("host", "stock.home.lan")
            ]),
            None
        ));
        assert!(!is_same_origin(
            &headers(&[("origin", "null"), ("host", "stock.home.lan")]),
            None
        ));
        assert!(
            is_same_origin(&HeaderMap::new(), None),
            "non-browser clients send neither header"
        );
    }

    #[test]
    fn client_ip_uses_the_proxy_appended_address_only_from_trusted_proxies() {
        let proxy: IpAddr = "10.0.10.1".parse().expect("ip");
        let phone: IpAddr = "10.0.20.7".parse().expect("ip");
        let trusted = [proxy];
        // The client forged the first entry; HAProxy appended the real one.
        let forwarded = headers(&[("x-forwarded-for", "1.2.3.4, 10.0.20.7")]);
        assert_eq!(client_ip(proxy, &forwarded, &trusted), phone);
        assert_eq!(
            client_ip(phone, &forwarded, &trusted),
            phone,
            "untrusted peers can't pick their address"
        );
        assert_eq!(
            client_ip(proxy, &forwarded, &[]),
            proxy,
            "no proxies configured"
        );

        let mut separate_lines = headers(&[("x-forwarded-for", "1.2.3.4")]);
        separate_lines.append("x-forwarded-for", HeaderValue::from_static("10.0.20.7"));
        assert_eq!(client_ip(proxy, &separate_lines, &trusted), phone);

        assert_eq!(
            client_ip(proxy, &HeaderMap::new(), &trusted),
            proxy,
            "header missing"
        );
        assert_eq!(
            client_ip(proxy, &headers(&[("x-forwarded-for", "unknown")]), &trusted),
            proxy
        );
        let mapped: IpAddr = "::ffff:10.0.10.1".parse().expect("ip");
        assert_eq!(
            client_ip(mapped, &forwarded, &trusted),
            phone,
            "IPv4-mapped proxy address"
        );
    }

    #[test]
    fn five_failures_per_minute_then_blocked_until_the_window_passes() {
        let limiter = PasswordLimiter::default();
        let ip: IpAddr = "10.0.0.9".parse().expect("ip");
        let other: IpAddr = "10.0.0.10".parse().expect("ip");
        let start = Instant::now();
        for _ in 0..5 {
            assert!(limiter.allowed_at(ip, start));
            limiter.failed_at(ip, start);
        }
        assert!(!limiter.allowed_at(ip, start + Duration::from_secs(30)));
        assert!(limiter.allowed_at(other, start));
        assert!(limiter.allowed_at(ip, start + WINDOW));
    }

    #[test]
    fn success_clears_failures() {
        let limiter = PasswordLimiter::default();
        let ip: IpAddr = "10.0.0.9".parse().expect("ip");
        for _ in 0..5 {
            limiter.failed(ip);
        }
        limiter.succeeded(ip);
        assert!(limiter.allowed(ip));
    }
}
