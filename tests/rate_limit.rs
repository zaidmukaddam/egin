//! The router's rate limit. Its own test binary: the breaker pauses every model call in the process,
//! which would slow the timing-sensitive tests elsewhere.
mod common;

use common::{fx, limited, say};
use egin::Approve;
use std::time::{Duration, Instant};

#[tokio::test]
async fn a_rate_limited_call_waits_for_the_breaker_and_retries() {
    let f = fx(vec![limited(), say("after the pause")], Approve::Auto).await;
    let t0 = Instant::now();
    let (_, out) = f.go(&f.agent(""), "hi").await;
    assert_eq!(out.unwrap(), "after the pause");
    assert_eq!(f.mock.reqs().len(), 2);
    assert!(t0.elapsed() >= Duration::from_secs(2), "the first 429 pauses calls for 2 s");
}
