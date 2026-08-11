//! State locations and the environment knobs that move them (`FR-06`, `FR-07`).
//!
//! One rule holds this module together: **no path is ever derived from a
//! torrent name** (`NFR-11`). Every file we write is named after an infohash or
//! a source id, both of which are validated character sets. A crafted result
//! name therefore cannot escape the state directory.
//!
//! `HARBOUR_STATE_DIR` relocates everything. It exists so the whole test suite —
//! all three tracks — can exercise real persistence against a temp directory
//! without touching a developer's actual downloads, and it doubles as a
//! portable-state escape hatch.

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::types::SourceId;

/// Env var: relocates all state (config, ledger, cache) under one directory.
pub const ENV_STATE_DIR: &str = "HARBOUR_STATE_DIR";
/// Env var: cap on concurrent downloads. `0`/unset means unlimited.
pub const ENV_MAX_DOWNLOADS: &str = "HARBOUR_MAX_DOWNLOADS";
/// Env var: per-source total deadline, in seconds.
pub const ENV_SOURCE_TIMEOUT: &str = "HARBOUR_SOURCE_TIMEOUT";

/// Default per-source ceiling, covering follow-up fetches (`docs/sources.md` §6).
pub const DEFAULT_SOURCE_TIMEOUT: Duration = Duration::from_secs(10);

/// Where all harbour state lives.
///
/// `HARBOUR_STATE_DIR` wins; otherwise `~/.harbour` (`%USERPROFILE%\.harbour`).
/// If the home directory cannot be determined — a degenerate environment with
/// neither `HOME` nor `USERPROFILE` — we fall back to a relative `.harbour`
/// rather than panicking, because losing state locality is recoverable and
/// refusing to start is not (`plan-engine.md` §4.1).
pub fn state_dir() -> PathBuf {
    if let Some(dir) = env::var_os(ENV_STATE_DIR).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    match dirs::home_dir() {
        Some(home) => home.join(".harbour"),
        None => {
            eprintln!(
                "harbour: no home directory found; keeping state in ./.harbour \
                 (set {ENV_STATE_DIR} to choose a location)"
            );
            PathBuf::from(".harbour")
        }
    }
}

/// `config.toml` (`FR-51`).
pub fn config_file() -> PathBuf {
    state_dir().join("config.toml")
}

/// The download ledger (`FR-48`).
pub fn ledger_file() -> PathBuf {
    state_dir().join("downloads.json")
}

/// Search-query history (`FR-49`). Not the recently-downloaded list, which is
/// derived from the ledger.
pub fn history_file() -> PathBuf {
    state_dir().join("history.json")
}

/// Boot marker for the crash breaker (`FR-08`).
pub fn boot_marker_file() -> PathBuf {
    state_dir().join("boot.marker")
}

pub fn cache_dir() -> PathBuf {
    state_dir().join("cache")
}

/// Cached `.torrent` metadata, keyed by infohash so a re-seed can verify local
/// files without re-fetching from the swarm (`FR-37`).
pub fn torrent_cache_dir() -> PathBuf {
    cache_dir().join("torrents")
}

/// Path for one cached `.torrent`.
///
/// Returns `None` unless `info_hash` is a real 40-hex hash. That check is the
/// `NFR-11` guarantee in code: the only caller-supplied component of this path
/// is constrained to `[0-9a-f]{40}`, so no name can traverse out of the dir.
pub fn torrent_cache_file(info_hash: &str) -> Option<PathBuf> {
    let hash = crate::core::magnet::normalize_info_hash(info_hash)?;
    Some(torrent_cache_dir().join(format!("{hash}.torrent")))
}

/// Per-source search cache directory.
pub fn search_cache_dir(source: SourceId) -> PathBuf {
    cache_dir().join("search").join(source.as_str())
}

/// Per-source host-health marker, backing the negative TTL
/// (`plan-engine.md` §10 D5).
pub fn health_marker_file(source: SourceId) -> PathBuf {
    cache_dir()
        .join("health")
        .join(format!("{}.json", source.as_str()))
}

/// Default download directory: the user's Downloads folder, `harbour` inside it.
pub fn default_download_dir() -> PathBuf {
    match dirs::download_dir() {
        Some(dir) => dir.join("harbour"),
        // No Downloads folder (headless Linux, an unusual profile): keep the
        // files somewhere predictable under our own state rather than refusing.
        None => state_dir().join("downloads"),
    }
}

/// Reads `HARBOUR_MAX_DOWNLOADS` (`FR-07`).
///
/// `0`, unset, and *anything unparseable* mean unlimited. Garbage warns loudly
/// and falls back rather than panicking or being silently reinterpreted as a
/// cap the user did not ask for.
pub fn max_downloads() -> usize {
    match env::var(ENV_MAX_DOWNLOADS) {
        Err(_) => 0,
        Ok(raw) if raw.trim().is_empty() => 0,
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                eprintln!(
                    "harbour: {ENV_MAX_DOWNLOADS}={raw:?} is not a number; \
                     continuing with no download limit"
                );
                0
            }
        },
    }
}

/// Reads `HARBOUR_SOURCE_TIMEOUT` in seconds, defaulting to
/// [`DEFAULT_SOURCE_TIMEOUT`]. Zero and garbage both fall back with a warning —
/// a zero deadline would make every search fail instantly, which is never what
/// someone setting this meant.
pub fn source_timeout() -> Duration {
    match env::var(ENV_SOURCE_TIMEOUT) {
        Err(_) => DEFAULT_SOURCE_TIMEOUT,
        Ok(raw) if raw.trim().is_empty() => DEFAULT_SOURCE_TIMEOUT,
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) | Err(_) => {
                eprintln!(
                    "harbour: {ENV_SOURCE_TIMEOUT}={raw:?} is not a positive number of \
                     seconds; using {}s",
                    DEFAULT_SOURCE_TIMEOUT.as_secs()
                );
                DEFAULT_SOURCE_TIMEOUT
            }
            Ok(secs) => Duration::from_secs(secs),
        },
    }
}

/// Creates a directory, reporting failure to the caller rather than panicking.
pub fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env vars are process-global, so these tests must not run concurrently
    /// with each other. One test drives all env-dependent assertions.
    #[test]
    fn env_knobs_degrade_instead_of_panicking() {
        // SAFETY: single-threaded within this test, and every path is restored
        // before returning. Rust 2024 requires the unsafe block for set_var.
        unsafe {
            // --- HARBOUR_MAX_DOWNLOADS -------------------------------------
            env::remove_var(ENV_MAX_DOWNLOADS);
            assert_eq!(max_downloads(), 0, "unset means unlimited");

            env::set_var(ENV_MAX_DOWNLOADS, "3");
            assert_eq!(max_downloads(), 3);

            env::set_var(ENV_MAX_DOWNLOADS, "0");
            assert_eq!(max_downloads(), 0, "explicit 0 means unlimited");

            env::set_var(ENV_MAX_DOWNLOADS, "  4 ");
            assert_eq!(max_downloads(), 4, "whitespace is tolerated");

            env::set_var(ENV_MAX_DOWNLOADS, "banana");
            assert_eq!(max_downloads(), 0, "garbage falls back, never panics");

            env::set_var(ENV_MAX_DOWNLOADS, "-2");
            assert_eq!(max_downloads(), 0, "negatives are not a cap");
            env::remove_var(ENV_MAX_DOWNLOADS);

            // --- HARBOUR_SOURCE_TIMEOUT ------------------------------------
            env::remove_var(ENV_SOURCE_TIMEOUT);
            assert_eq!(source_timeout(), DEFAULT_SOURCE_TIMEOUT);

            env::set_var(ENV_SOURCE_TIMEOUT, "5");
            assert_eq!(source_timeout(), Duration::from_secs(5));

            env::set_var(ENV_SOURCE_TIMEOUT, "0");
            assert_eq!(
                source_timeout(),
                DEFAULT_SOURCE_TIMEOUT,
                "a zero deadline would fail every search instantly"
            );

            env::set_var(ENV_SOURCE_TIMEOUT, "soon");
            assert_eq!(source_timeout(), DEFAULT_SOURCE_TIMEOUT);
            env::remove_var(ENV_SOURCE_TIMEOUT);

            // --- HARBOUR_STATE_DIR -----------------------------------------
            env::set_var(ENV_STATE_DIR, "/tmp/harbour-test-state");
            let root = PathBuf::from("/tmp/harbour-test-state");
            assert_eq!(state_dir(), root);
            assert_eq!(config_file(), root.join("config.toml"));
            assert_eq!(ledger_file(), root.join("downloads.json"));
            assert_eq!(history_file(), root.join("history.json"));
            assert_eq!(boot_marker_file(), root.join("boot.marker"));
            assert_eq!(torrent_cache_dir(), root.join("cache").join("torrents"));
            assert_eq!(
                search_cache_dir(SourceId::VaultMovies),
                root.join("cache").join("search").join("vault-movies")
            );
            assert_eq!(
                health_marker_file(SourceId::CineVault),
                root.join("cache").join("health").join("cinevault.json")
            );

            // An empty value must not silently relocate state to "".
            env::set_var(ENV_STATE_DIR, "");
            assert_ne!(state_dir(), PathBuf::new());
            env::remove_var(ENV_STATE_DIR);
        }
    }

    #[test]
    fn torrent_cache_paths_are_infohash_only() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let path = torrent_cache_file(hash).expect("valid hash");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(format!("{hash}.torrent").as_str())
        );

        // NFR-11: nothing name-derived may reach the filesystem.
        assert!(torrent_cache_file("../../etc/passwd").is_none());
        assert!(torrent_cache_file("").is_none());
        assert!(torrent_cache_file("Some Movie (2026)").is_none());
        assert!(
            torrent_cache_file(&format!("{hash}/../..")).is_none(),
            "a traversal suffix must not pass the hash check"
        );
    }

    #[test]
    fn uppercase_hashes_are_canonicalized_into_one_cache_path() {
        let lower = "abcdef0123456789abcdef0123456789abcdef01";
        let a = torrent_cache_file(lower).expect("valid");
        let b = torrent_cache_file(&lower.to_uppercase()).expect("valid");
        assert_eq!(a, b, "the same torrent must not get two cache files");
    }
}
