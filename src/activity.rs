use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Default)]
pub struct Activity(AtomicU64);

impl Activity {
    pub fn touch(&self) {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.0.store(t, Ordering::Relaxed);
    }
    pub fn last_request(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// `.layer(axum::middleware::from_fn_with_state(activity.clone(), track))`
pub async fn track(State(a): State<Arc<Activity>>, req: Request, next: Next) -> Response {
    // Static/photo fetches don't count as “usage”.
    let p = req.uri().path();
    if !p.starts_with("/static/") && !p.starts_with("/photos/") {
        a.touch();
    }
    next.run(req).await
}
