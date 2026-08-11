//! Torrent engine implementations behind [`crate::core::types::Engine`].
//!
//! The trait lives in `core` so the queue can depend on it without depending on
//! a backend. [`fake::FakeEngine`] is the in-memory one every queue test runs
//! against; the librqbit adapter lands after the E1 spike gate, deliberately —
//! `docs/plan-engine.md` §5 does not code against unobserved engine behaviour.
//!
//! `dead_code` is allowed for this subtree until the app loop constructs an
//! engine (E2); the fake's drivers are used by the queue tests today.

#![allow(dead_code)]

pub mod fake;
pub mod rqbit;
