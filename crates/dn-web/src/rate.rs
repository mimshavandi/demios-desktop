//! A per-host rate limit: at most `concurrent` requests to one host at a time, and at least
//! `interval` between the starts of two requests to it. Kept in memory; a restart forgets it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

struct Host {
    slots: Arc<Semaphore>,
    next_start: Instant,
}

pub(crate) struct RateLimit {
    interval: Duration,
    concurrent: usize,
    hosts: Mutex<HashMap<String, Host>>,
}

/// Held while a request to the host runs.
pub(crate) struct Turn {
    _permit: OwnedSemaphorePermit,
}

impl RateLimit {
    pub(crate) fn new(interval: Duration, concurrent: usize) -> Self {
        Self {
            interval,
            concurrent: concurrent.max(1),
            hosts: Mutex::new(HashMap::new()),
        }
    }

    /// Waits for this host's turn.
    pub(crate) async fn turn(&self, host: &str) -> Turn {
        let slots = {
            let mut hosts = self.hosts.lock().await;
            let h = hosts.entry(host.to_owned()).or_insert_with(|| Host {
                slots: Arc::new(Semaphore::new(self.concurrent)),
                next_start: Instant::now(),
            });
            Arc::clone(&h.slots)
        };
        let permit = slots
            .acquire_owned()
            .await
            .unwrap_or_else(|_| unreachable!("the semaphore is never closed"));
        let start = {
            let mut hosts = self.hosts.lock().await;
            let now = Instant::now();
            let h = hosts
                .get_mut(host)
                .unwrap_or_else(|| unreachable!("added above and never removed"));
            let start = h.next_start.max(now);
            h.next_start = start + self.interval;
            start
        };
        tokio::time::sleep_until(start).await;
        Turn { _permit: permit }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn requests_to_one_host_are_spaced_and_other_hosts_dont_wait() {
        let r = RateLimit::new(Duration::from_secs(1), 2);
        let t0 = Instant::now();
        let _a = r.turn("a.example").await;
        let _b = r.turn("a.example").await;
        assert_eq!(Instant::now() - t0, Duration::from_secs(1));
        let _c = r.turn("b.example").await;
        assert_eq!(
            Instant::now() - t0,
            Duration::from_secs(1),
            "another host doesn't wait"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn at_most_n_requests_run_at_once() {
        let r = Arc::new(RateLimit::new(Duration::ZERO, 2));
        let a = r.turn("a.example").await;
        let _b = r.turn("a.example").await;
        let r2 = Arc::clone(&r);
        let third = tokio::spawn(async move { r2.turn("a.example").await });
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!third.is_finished(), "a third request waits for a slot");
        drop(a);
        third.await.unwrap();
    }
}
