//! Core domain — the normative shared contract for all three tracks.
//!
//! This module is the freeze described in `docs/plan-engine.md` §3. It replaces
//! the earlier working `src/types.rs`, which was written by the UI track to
//! unblock itself and explicitly deferred to this one.
//!
//! Everything here is deliberately free of I/O, the terminal, and the torrent
//! engine, so the UI can render it, the sources can produce it, and the queue can
//! be unit-tested against it without a network or a runtime.
//!
//! Changes to `types` are breaking for every track (`AGENTS.md` rule 4).
//!
//! `dead_code` is allowed for this subtree: a contract is defined ahead of its
//! consumers by definition, and the pieces without one yet — `SourceError`, the
//! `Source` trait, `SearchCtx`, the cache paths — are consumed by the engine
//! adapter and the search layer in E2/E3. The lint scope covers the child
//! modules, so it lives here rather than being repeated in five files. Remove it
//! once the search layer lands, and let the compiler point at anything genuinely
//! unused.

#![allow(dead_code)]

pub mod cancel;
pub mod error;
pub mod magnet;
pub mod paths;
pub mod types;
