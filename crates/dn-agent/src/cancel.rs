//! Cooperative cancellation shared by the agent loop and tool executors.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

/// Cheap, cloneable cancellation flag. All clones observe the same state.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel every holder of this token. Idempotent.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once [`cancel`](Self::cancel) has been called (immediately if it already was).
    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_wakes_waiters() {
        let t = CancelToken::new();
        let waiter = {
            let t = t.clone();
            tokio::spawn(async move { t.cancelled().await })
        };
        tokio::task::yield_now().await;
        assert!(!t.is_cancelled());
        t.cancel();
        waiter.await.unwrap();
        assert!(t.is_cancelled());
        // Already cancelled: returns immediately.
        t.cancelled().await;
    }
}
