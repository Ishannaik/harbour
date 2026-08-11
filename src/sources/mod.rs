//! The ten curated sources, the shared fetch layer, and the search cache.
//!
//! Every adapter implements [`crate::core::types::Source`] and is registered in
//! [`registry`]. Adapters are stateless by contract: anything that would be
//! remembered between searches — mirror health, the sticky host hint, cached
//! results — lives in the engine and arrives through `SearchCtx`.
//!
//! Each adapter separates *parsing* from *fetching* so the parser can be tested
//! against a committed fixture (`FR-22`). A scraper tested only against the live
//! site is a scraper that breaks silently.

// Staged API: the host-health helpers and a couple of fetch conveniences are
// consumed as each scraper lands. Remove this once the registry is complete and
// let the compiler point at anything genuinely unused.
#![allow(dead_code)]

pub mod cache;
pub mod eztv;
pub mod net;
pub mod nyaa;
pub mod subsplease;
pub mod yts;

use std::sync::Arc;

use crate::core::types::ArcSource;

/// Every source, in sidebar order.
pub fn registry() -> Vec<ArcSource> {
    vec![Arc::new(yts::Yts::new())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::SourceId;

    #[test]
    fn the_registry_is_a_working_trait_object_collection() {
        // This is the assertion that the `Source` trait must stay
        // dyn-compatible: `async fn` or `-> impl Future` in the trait would fail
        // to compile here, and only here — which is why it went unnoticed in the
        // design docs for so long.
        let sources = registry();
        assert!(!sources.is_empty());
        for source in &sources {
            assert!(!source.def().label.is_empty());
        }
    }

    #[test]
    fn every_registered_source_has_a_distinct_id() {
        let ids: Vec<SourceId> = registry().iter().map(|s| s.def().id).collect();
        let mut sorted: Vec<&str> = ids.iter().map(|i| i.as_str()).collect();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(sorted.len(), before, "duplicate source id in the registry");
    }
}
