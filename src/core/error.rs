//! Typed errors for the source and engine boundaries.
//!
//! These are enums rather than strings on purpose. `SourceError`'s variant is
//! what decides whether the fetch layer retries (`Network`), fails fast without
//! burning the retry budget (`Blocked` — a bot challenge never becomes a 200 by
//! asking again), and whether the engine writes a negative-cache marker
//! (`docs/plan-engine.md` §10 D5: hard failures only). A `Result<_, String>`
//! erases exactly the distinction all three behaviours key off.
//!
//! Hand-rolled `Display`/`Error` rather than a `thiserror` dependency: it is
//! twenty lines for two enums, and `AGENTS.md` rule 8 asks us to justify every
//! crate. If a third error enum shows up, take the dependency.

use std::fmt;

/// Why a source failed to answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// Transport-level failure — DNS, connection refused, TLS, a 5xx after
    /// retries. Retryable, and a hard failure for negative-caching purposes.
    Network(String),
    /// The response arrived but did not parse. Usually markup drift; never
    /// retried, because the same bytes will not parse the second time.
    Parse(String),
    /// The source refused us — rate limit, bot check, geoblock. Failing fast is
    /// the point: retrying a challenge page only deepens the block.
    Blocked(String),
    /// The source ran out of its deadline budget (`SearchCtx`). Distinct from
    /// `Network` because the source may be perfectly healthy and merely slow —
    /// the user simply is not waiting any longer.
    Timeout,
    /// The search was cancelled (a new query, or quit). Not a failure of the
    /// source, and never surfaced to the user or negative-cached.
    Cancelled,
}

impl SourceError {
    /// Whether this failure should be recorded in the per-host health marker so
    /// a sick host is not re-probed on every keystroke (`plan-engine.md` §10 D5).
    ///
    /// `Cancelled` never counts — we stopped it, the source did not fail. A
    /// `Parse` failure is a *source* defect rather than a host one and would be
    /// identical on every mirror, so parking the host would not help.
    pub fn is_hard_host_failure(&self) -> bool {
        matches!(
            self,
            SourceError::Network(_) | SourceError::Blocked(_) | SourceError::Timeout
        )
    }

    /// Stable machine-readable tag, used in the health marker and in logs.
    pub fn class(&self) -> &'static str {
        match self {
            SourceError::Network(_) => "network",
            SourceError::Parse(_) => "parse",
            SourceError::Blocked(_) => "blocked",
            SourceError::Timeout => "timeout",
            SourceError::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceError::Network(m) => write!(f, "network: {m}"),
            SourceError::Parse(m) => write!(f, "parse: {m}"),
            SourceError::Blocked(m) => write!(f, "source rejected the request: {m}"),
            SourceError::Timeout => f.write_str("timed out"),
            SourceError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for SourceError {}

/// Why an engine operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The engine could not be constructed at all (a hostile environment, a
    /// port we cannot bind). Search still works; downloads report this reason
    /// rather than the app refusing to start (`plan-engine.md` §4.2).
    Unavailable(String),
    /// The input was not a magnet, infohash, or readable `.torrent`.
    InvalidInput(String),
    /// The engine does not know this id — already removed, or never added.
    NotFound,
    /// Metadata never arrived within the deadline. A magnet with no peers fires
    /// no success and no error of its own, so this is our own timeout.
    NoMetadata,
    /// Anything the engine itself reported.
    Backend(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Unavailable(m) => write!(f, "torrent engine unavailable: {m}"),
            EngineError::InvalidInput(m) => write!(f, "not a magnet, infohash, or .torrent: {m}"),
            EngineError::NotFound => f.write_str("no such torrent"),
            EngineError::NoMetadata => {
                f.write_str("no peers found — could not fetch torrent metadata")
            }
            EngineError::Backend(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for EngineError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_host_level_failures_are_negative_cached() {
        assert!(SourceError::Network("refused".into()).is_hard_host_failure());
        assert!(SourceError::Blocked("cf".into()).is_hard_host_failure());
        assert!(SourceError::Timeout.is_hard_host_failure());
        // A parse failure is the source's defect, identical on every mirror —
        // parking the host would not help and would hide a working host.
        assert!(!SourceError::Parse("bad row".into()).is_hard_host_failure());
        // We cancelled it; the source did nothing wrong.
        assert!(!SourceError::Cancelled.is_hard_host_failure());
    }

    #[test]
    fn classes_are_stable_tags() {
        assert_eq!(SourceError::Timeout.class(), "timeout");
        assert_eq!(SourceError::Parse(String::new()).class(), "parse");
    }

    #[test]
    fn messages_read_as_sentences() {
        assert_eq!(
            EngineError::NoMetadata.to_string(),
            "no peers found — could not fetch torrent metadata"
        );
    }
}
