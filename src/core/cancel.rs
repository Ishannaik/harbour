//! A minimal cancellation token for in-flight searches (`FR-20`).
//!
//! `tokio_util::sync::CancellationToken` does this and more. We hand-roll the
//! two operations we actually need — "is it cancelled" and "wake me when it is"
//! — because `AGENTS.md` rule 8 asks us to justify every crate and this is forty
//! lines over primitives `tokio` already gives us. If we ever need child tokens
//! or `run_until_cancelled`, take the dependency instead of growing this.
//!
//! Cancellation is deliberately one-way and idempotent: a token that has fired
//! stays fired, and cancelling twice is not an error. A search is abandoned, it
//! is never un-abandoned.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[derive(Debug, Default)]
struct Inner {
    cancelled: AtomicBool,
    notify: Notify,
}

/// A cheap clonable handle; every clone observes the same cancellation.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<Inner>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels every holder of this token. Idempotent.
    pub fn cancel(&self) {
        // Release pairs with the Acquire in `is_cancelled`, so a task that sees
        // the flag also sees everything written before the cancel.
        self.0.cancelled.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    /// Resolves once cancelled; returns immediately if it already has been.
    ///
    /// The flag is re-checked after registering with `Notify` because a cancel
    /// racing between the first check and the registration would otherwise be
    /// missed and the caller would wait forever.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let waiter = self.0.notify.notified();
        if self.is_cancelled() {
            return;
        }
        waiter.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_live_and_cancels_every_clone() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!a.is_cancelled());
        assert!(!b.is_cancelled());
        b.cancel();
        assert!(a.is_cancelled(), "cancellation is shared, not per-handle");
    }

    #[test]
    fn cancelling_twice_is_not_an_error() {
        let t = CancelToken::new();
        t.cancel();
        t.cancel();
        assert!(t.is_cancelled());
    }

    #[tokio::test]
    async fn await_returns_immediately_when_already_cancelled() {
        let t = CancelToken::new();
        t.cancel();
        // Would hang if the pre-check were missing.
        t.cancelled().await;
    }

    #[tokio::test]
    async fn await_wakes_on_a_later_cancel() {
        let t = CancelToken::new();
        let waiter = t.clone();
        let handle = tokio::spawn(async move {
            waiter.cancelled().await;
            true
        });
        // Yield so the task reaches its await point before we fire.
        tokio::task::yield_now().await;
        t.cancel();
        assert!(handle.await.expect("waiter task panicked"));
    }
}
