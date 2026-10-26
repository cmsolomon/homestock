//! Request hardening: failed-password throttling and a cross-site request check.
use axum::{extract::Request, http::{header, HeaderMap, Method, StatusCode}, middleware::Next, response::{IntoResponse, Response}};
use std::{collections::HashMap, net::IpAddr, sync::Mutex, time::{Duration, Instant}};

const MAX_FAILURES: usize = 5;
const WINDOW: Duration = Duration::from_secs(60);

/// Failed password checks per client IP: at most `MAX_FAILURES` in any rolling minute.
/// In memory only; a restart forgets it, which is fine for a household LAN app.
#[derive(Default)]
pub struct PasswordLimiter(Mutex<HashMap<IpAddr, Vec<Instant>>>);

impl PasswordLimiter {
    pub fn allowed(&self, ip: IpAddr) -> bool { self.allowed_at(ip, Instant::now()) }
    pub fn failed(&self, ip: IpAddr) { self.failed_at(ip, Instant::now()) }
    pub fn succeeded(&self, ip: IpAddr) {
        if let Ok(mut map) = self.0.lock() { map.remove(&ip); }
    }

    fn allowed_at(&self, ip: IpAddr, now: Instant) -> bool {
        let Ok(mut map) = self.0.lock() else { return false };
        let Some(failures) = map.get_mut(&ip) else { return true };
        failures.retain(|t| now.duration_since(*t) < WINDOW);
        failures.len() < MAX_FAILURES
    }

    fn failed_at(&self, ip: IpAddr, now: Instant) {
        let Ok(mut map) = self.0.lock() else { return };
        // Bound memory if many addresses fail: drop everyone outside the window.
        if map.len() > 1024 { map.retain(|_, failures| failures.iter().any(|t| now.duration_since(*t) < WINDOW)); }
        map.entry(ip).or_default().push(now);
    }
}

/// Reject state-changing requests started by another site. SameSite=Lax cookies already stop
/// other *sites*, but sibling hosts on the same domain (another `*.home.lan` service) count
/// as same-site, so check `Sec-Fetch-Site`, falling back to `Origin` for older browsers.
pub async fn same_origin(req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) && !is_same_origin(req.headers(), req.uri().authority().map(|a| a.as_str())) {
        return (StatusCode::FORBIDDEN, "Cross-site request blocked").into_response();
    }
    next.run(req).await
}

fn is_same_origin(h: &HeaderMap, uri_authority: Option<&str>) -> bool {
    if let Some(site) = h.get("Sec-Fetch-Site").and_then(|v| v.to_str().ok()) {
        return matches!(site, "same-origin" | "none");
    }
    let Some(origin) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else { return true };
    // HTTP/2 carries the host in the URI authority rather than a Host header.
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).or(uri_authority);
    origin.split_once("://").map(|(_, authority)| authority) == host
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs { h.insert(*k, HeaderValue::from_static(v)); }
        h
    }

    #[test]
    fn fetch_metadata_decides_when_present() {
        assert!(is_same_origin(&headers(&[("sec-fetch-site", "same-origin")]), None));
        assert!(is_same_origin(&headers(&[("sec-fetch-site", "none")]), None));
        assert!(!is_same_origin(&headers(&[("sec-fetch-site", "same-site"), ("origin", "https://stock.lan"), ("host", "stock.lan")]), None));
        assert!(!is_same_origin(&headers(&[("sec-fetch-site", "cross-site")]), None));
    }

    #[test]
    fn origin_must_match_host_without_fetch_metadata() {
        assert!(is_same_origin(&headers(&[("origin", "https://10.0.0.5:8443"), ("host", "10.0.0.5:8443")]), None));
        assert!(is_same_origin(&headers(&[("origin", "https://10.0.0.5:8443")]), Some("10.0.0.5:8443")));
        assert!(!is_same_origin(&headers(&[("origin", "https://nas.home.lan"), ("host", "stock.home.lan")]), None));
        assert!(!is_same_origin(&headers(&[("origin", "null"), ("host", "stock.home.lan")]), None));
        assert!(is_same_origin(&HeaderMap::new(), None), "non-browser clients send neither header");
    }

    #[test]
    fn five_failures_per_minute_then_blocked_until_the_window_passes() {
        let limiter = PasswordLimiter::default();
        let ip: IpAddr = "10.0.0.9".parse().expect("ip");
        let other: IpAddr = "10.0.0.10".parse().expect("ip");
        let start = Instant::now();
        for _ in 0..5 { assert!(limiter.allowed_at(ip, start)); limiter.failed_at(ip, start); }
        assert!(!limiter.allowed_at(ip, start + Duration::from_secs(30)));
        assert!(limiter.allowed_at(other, start));
        assert!(limiter.allowed_at(ip, start + WINDOW));
    }

    #[test]
    fn success_clears_failures() {
        let limiter = PasswordLimiter::default();
        let ip: IpAddr = "10.0.0.9".parse().expect("ip");
        for _ in 0..5 { limiter.failed(ip); }
        limiter.succeeded(ip);
        assert!(limiter.allowed(ip));
    }
}
