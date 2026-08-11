//! On-disk state: the ledger, search history, config, and the crash marker.
//!
//! Three rules, all of them about not losing a user's data:
//!
//! 1. **Every write is atomic** — temp file in the same directory, then rename.
//!    A crash mid-write leaves either the old file or the new one, never half of
//!    one (`FR-55`). Same directory matters: a rename across filesystems is a
//!    copy, and stops being atomic.
//! 2. **A corrupt file is quarantined, never deleted and never overwritten.**
//!    We rename it to `.corrupt`, start from defaults, and say so (`FR-54`).
//!    The user's file may be the only copy of something they care about.
//! 3. **One bad row does not lose the file.** A ledger with nineteen good
//!    entries and one malformed one restores nineteen items.
//!
//! Everything is addressed through [`Store`] rather than reading the environment
//! at each call, so tests run against a temp directory in parallel without
//! fighting over `HARBOUR_STATE_DIR`.
//!
//! `dead_code` is allowed module-wide until the app loop calls [`Store`] on the
//! boot and quit paths (E2/E3). Every item here is covered by tests; the lint
//! only means nothing in `main` reaches it yet. Remove the allow as the wiring
//! lands.

#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::paths;
use crate::core::types::QueueItem;

/// Hard cap on remembered search queries (`FR-49`).
pub const HISTORY_CAP: usize = 500;

/// User configuration (`FR-51`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where downloads land unless overridden per item.
    pub download_dir: PathBuf,
    /// Active theme name.
    pub theme: String,
    /// Keep seeding after a download completes.
    pub seed_by_default: bool,
    /// Extra announce URLs added to every torrent.
    pub trackers: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            download_dir: paths::default_download_dir(),
            theme: "titanium".into(),
            seed_by_default: true,
            trackers: Vec::new(),
        }
    }
}

/// Outcome of a load that can degrade. The caller turns `Recovered` into the
/// banner the user sees — a silent fallback is how people lose data without
/// noticing.
#[derive(Debug, Clone, PartialEq)]
pub enum Loaded<T> {
    /// Loaded cleanly, or the file simply did not exist yet.
    Ok(T),
    /// Something was wrong. `value` is the best we could salvage.
    Recovered { value: T, warning: String },
}

impl<T> Loaded<T> {
    pub fn value(self) -> T {
        match self {
            Loaded::Ok(v) => v,
            Loaded::Recovered { value, .. } => value,
        }
    }

    pub fn warning(&self) -> Option<&str> {
        match self {
            Loaded::Ok(_) => None,
            Loaded::Recovered { warning, .. } => Some(warning),
        }
    }
}

/// Handle on one state directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The real state directory (`HARBOUR_STATE_DIR`, else `~/.harbour`).
    pub fn from_env() -> Self {
        Self::new(paths::state_dir())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ledger_path(&self) -> PathBuf {
        self.root.join("downloads.json")
    }

    pub fn history_path(&self) -> PathBuf {
        self.root.join("history.json")
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn boot_marker_path(&self) -> PathBuf {
        self.root.join("boot.marker")
    }

    // --- ledger -------------------------------------------------------------

    /// Writes the ledger atomically. Only durable fields are in [`QueueItem`],
    /// so this is called on status transitions rather than on every poll tick.
    pub fn save_ledger(&self, items: &[QueueItem]) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(items)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        atomic_write(&self.ledger_path(), &json)
    }

    /// Loads the ledger, salvaging as much as possible.
    ///
    /// A missing file is a clean empty start. A file that will not parse at all
    /// is quarantined. A file that parses as an array keeps every row that is a
    /// valid item and reports how many were dropped.
    pub fn load_ledger(&self) -> Loaded<Vec<QueueItem>> {
        let path = self.ledger_path();
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Loaded::Ok(Vec::new()),
            Err(e) => {
                return Loaded::Recovered {
                    value: Vec::new(),
                    warning: format!("could not read {}: {e}", path.display()),
                };
            }
        };

        // Parse to generic JSON first so one malformed entry costs one entry
        // rather than the whole file.
        let rows: Vec<serde_json::Value> = match serde_json::from_str(&raw) {
            Ok(serde_json::Value::Array(rows)) => rows,
            _ => {
                let warning = match quarantine(&path) {
                    Ok(moved) => format!(
                        "{} was unreadable; kept a copy at {} and started with an empty queue",
                        path.display(),
                        moved.display()
                    ),
                    Err(e) => format!(
                        "{} was unreadable and could not be set aside ({e}); \
                         started with an empty queue",
                        path.display()
                    ),
                };
                return Loaded::Recovered {
                    value: Vec::new(),
                    warning,
                };
            }
        };

        let total = rows.len();
        let items: Vec<QueueItem> = rows
            .into_iter()
            .filter_map(|row| serde_json::from_value(row).ok())
            .collect();

        let dropped = total - items.len();
        if dropped == 0 {
            Loaded::Ok(items)
        } else {
            Loaded::Recovered {
                warning: format!(
                    "{dropped} of {total} saved downloads could not be read and were skipped"
                ),
                value: items,
            }
        }
    }

    // --- search history -----------------------------------------------------

    /// Saves search queries, newest first, capped at [`HISTORY_CAP`].
    pub fn save_history(&self, queries: &[String]) -> io::Result<()> {
        let capped: Vec<&String> = queries.iter().take(HISTORY_CAP).collect();
        let json = serde_json::to_vec_pretty(&capped)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        atomic_write(&self.history_path(), &json)
    }

    /// Loads search history. Corrupt history is worth nothing, so it degrades
    /// to empty without quarantine noise.
    pub fn load_history(&self) -> Loaded<Vec<String>> {
        let path = self.history_path();
        match fs::read_to_string(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Loaded::Ok(Vec::new()),
            Err(e) => Loaded::Recovered {
                value: Vec::new(),
                warning: format!("could not read search history: {e}"),
            },
            Ok(raw) => match serde_json::from_str::<Vec<String>>(&raw) {
                Ok(mut v) => {
                    v.truncate(HISTORY_CAP);
                    Loaded::Ok(v)
                }
                Err(_) => Loaded::Recovered {
                    value: Vec::new(),
                    warning: "search history was unreadable and has been reset".into(),
                },
            },
        }
    }

    /// Records a query at the front, de-duplicated, capped.
    pub fn push_history(&self, existing: &mut Vec<String>, query: &str) -> io::Result<()> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(());
        }
        existing.retain(|q| q != query);
        existing.insert(0, query.to_owned());
        existing.truncate(HISTORY_CAP);
        self.save_history(existing)
    }

    // --- config -------------------------------------------------------------

    pub fn save_config(&self, config: &Config) -> io::Result<()> {
        let text = toml::to_string_pretty(config)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        atomic_write(&self.config_path(), text.as_bytes())
    }

    /// Loads config, falling back loudly.
    ///
    /// A missing config is a first run, not an error. An unparseable one keeps
    /// defaults **and leaves the file alone** — overwriting someone's config
    /// because we could not read it is not a recovery, it is a second failure.
    pub fn load_config(&self) -> Loaded<Config> {
        let path = self.config_path();
        match fs::read_to_string(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Loaded::Ok(Config::default()),
            Err(e) => Loaded::Recovered {
                value: Config::default(),
                warning: format!("could not read {}: {e}", path.display()),
            },
            Ok(raw) => match toml::from_str::<Config>(&raw) {
                Ok(cfg) => Loaded::Ok(cfg),
                Err(e) => Loaded::Recovered {
                    value: Config::default(),
                    warning: format!(
                        "{} is not valid TOML ({}); using defaults. Your file has been left \
                         untouched.",
                        path.display(),
                        e.message()
                    ),
                },
            },
        }
    }

    // --- bootguard ----------------------------------------------------------

    /// True if the previous run died before it finished starting up (`FR-08`).
    pub fn boot_was_interrupted(&self) -> bool {
        self.boot_marker_path().exists()
    }

    /// Writes the crash marker. Called immediately before state is handed to the
    /// engine.
    pub fn arm_boot_marker(&self) -> io::Result<()> {
        ensure_parent(&self.boot_marker_path())?;
        fs::write(self.boot_marker_path(), b"harbour boot in progress\n")
    }

    /// Clears the crash marker. **Flush the ledger before calling this**: a
    /// crash between the two would otherwise leave a clean marker over stale
    /// state, and bootguard would stand down exactly when it was needed.
    pub fn disarm_boot_marker(&self) -> io::Result<()> {
        match fs::remove_file(self.boot_marker_path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The exit path, in the one order that is safe: flush state, *then* stand
    /// the crash breaker down.
    pub fn flush_and_disarm(&self, items: &[QueueItem]) -> io::Result<()> {
        self.save_ledger(items)?;
        self.disarm_boot_marker()
    }
}

/// Writes `bytes` to `path` atomically.
///
/// The temp file is a sibling so the rename stays within one filesystem, and it
/// is removed on a failed write so a crashed run does not leave litter that
/// looks like state.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    ensure_parent(path)?;
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("new")
    ));
    if let Err(e) = fs::write(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    // rename replaces the destination on both Unix and Windows.
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn ensure_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => fs::create_dir_all(dir),
        _ => Ok(()),
    }
}

/// Moves an unreadable file aside, never deleting it.
fn quarantine(path: &Path) -> io::Result<PathBuf> {
    let target = path.with_extension(format!(
        "{}.corrupt",
        path.extension().and_then(|e| e.to_str()).unwrap_or("bak")
    ));
    fs::rename(path, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{QueueStatus, SourceId};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Per-test temp directory. No `tempfile` dependency for what is six lines;
    /// the counter keeps parallel tests from colliding.
    fn temp_store(tag: &str) -> Store {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("harbour-test-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        Store::new(dir)
    }

    fn item(id: char, status: QueueStatus) -> QueueItem {
        let mut item = QueueItem::new(
            std::iter::repeat_n(id, 40).collect(),
            format!("item {id}"),
            Some(SourceId::CineVault),
            Some("magnet:?xt=urn:btih:abc".into()),
            PathBuf::from("/tmp/dl"),
            1_786_000_000_000,
        );
        item.status = status;
        item
    }

    #[test]
    fn ledger_round_trips() {
        let store = temp_store("ledger");
        let items = vec![
            item('a', QueueStatus::Seeding),
            item('b', QueueStatus::Paused),
        ];
        store.save_ledger(&items).expect("save");
        assert_eq!(store.load_ledger(), Loaded::Ok(items));
    }

    #[test]
    fn a_missing_ledger_is_a_clean_start_not_an_error() {
        let store = temp_store("missing");
        assert_eq!(store.load_ledger(), Loaded::Ok(Vec::new()));
        assert!(store.load_ledger().warning().is_none());
    }

    #[test]
    fn a_corrupt_ledger_is_quarantined_never_deleted() {
        let store = temp_store("corrupt");
        fs::write(store.ledger_path(), b"{ this is not json").expect("write");

        let loaded = store.load_ledger();
        assert!(loaded.warning().is_some(), "the user is told");
        assert!(loaded.value().is_empty(), "startup survives");

        let kept = store.ledger_path().with_extension("json.corrupt");
        assert!(
            kept.exists(),
            "FR-54: the original is set aside, not destroyed"
        );
        assert_eq!(fs::read_to_string(kept).unwrap(), "{ this is not json");
    }

    #[test]
    fn one_bad_row_does_not_lose_the_whole_ledger() {
        let store = temp_store("badrow");
        let good = serde_json::to_value(item('a', QueueStatus::Queued)).unwrap();
        let payload = serde_json::json!([good, { "id": "nonsense" }]);
        fs::write(store.ledger_path(), payload.to_string()).expect("write");

        let loaded = store.load_ledger();
        let warning = loaded.warning().map(str::to_owned);
        let items = loaded.value();
        assert_eq!(items.len(), 1, "the good row survives");
        assert!(warning.unwrap().contains("1 of 2"));
    }

    #[test]
    fn writes_are_atomic_and_leave_no_litter() {
        let store = temp_store("atomic");
        store
            .save_ledger(&[item('a', QueueStatus::Queued)])
            .expect("save");
        store
            .save_ledger(&[item('b', QueueStatus::Queued)])
            .expect("overwrite");

        let stray: Vec<String> = fs::read_dir(store.root())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(stray.is_empty(), "temp files left behind: {stray:?}");
        assert_eq!(store.load_ledger().value()[0].name, "item b");
    }

    #[test]
    fn history_dedupes_and_caps() {
        let store = temp_store("history");
        let mut history = Vec::new();
        store.push_history(&mut history, "dune").unwrap();
        store.push_history(&mut history, "shogun").unwrap();
        store.push_history(&mut history, "dune").unwrap();
        assert_eq!(
            history,
            vec!["dune", "shogun"],
            "newest first, no duplicate"
        );

        for i in 0..HISTORY_CAP + 50 {
            store.push_history(&mut history, &format!("q{i}")).unwrap();
        }
        assert_eq!(history.len(), HISTORY_CAP, "FR-49 cap enforced on write");
        assert_eq!(store.load_history().value().len(), HISTORY_CAP);
    }

    #[test]
    fn blank_queries_are_not_recorded() {
        let store = temp_store("blank");
        let mut history = Vec::new();
        store.push_history(&mut history, "   ").unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn corrupt_history_resets_quietly_but_visibly() {
        let store = temp_store("badhistory");
        fs::write(store.history_path(), b"not json").unwrap();
        let loaded = store.load_history();
        assert!(loaded.warning().is_some());
        assert!(loaded.value().is_empty());
    }

    #[test]
    fn config_round_trips() {
        let store = temp_store("config");
        let cfg = Config {
            download_dir: PathBuf::from("/tmp/somewhere"),
            theme: "midnight".into(),
            seed_by_default: false,
            trackers: vec!["udp://tracker.example:80".into()],
        };
        store.save_config(&cfg).expect("save");
        assert_eq!(store.load_config(), Loaded::Ok(cfg));
    }

    #[test]
    fn a_missing_config_is_a_first_run() {
        let store = temp_store("noconfig");
        let loaded = store.load_config();
        assert!(loaded.warning().is_none(), "a first run is not an error");
        let cfg = loaded.value();
        // Asserted field-wise rather than against `Config::default()` as a
        // whole: the default download directory is derived from the
        // environment, and comparing two separately-computed copies of it
        // couples this test to process-global state.
        assert_eq!(cfg.theme, "titanium");
        assert!(cfg.seed_by_default);
        assert!(cfg.trackers.is_empty());
    }

    #[test]
    fn an_invalid_config_falls_back_without_overwriting_the_users_file() {
        let store = temp_store("badconfig");
        let original = "theme = [this is not toml";
        fs::write(store.config_path(), original).unwrap();

        let loaded = store.load_config();
        assert!(loaded.warning().unwrap().contains("not valid TOML"));
        assert_eq!(loaded.value(), Config::default());
        assert_eq!(
            fs::read_to_string(store.config_path()).unwrap(),
            original,
            "we do not get to destroy a config we merely failed to parse"
        );
    }

    #[test]
    fn a_partial_config_keeps_defaults_for_the_rest() {
        let store = temp_store("partialconfig");
        fs::write(store.config_path(), "theme = \"midnight\"\n").unwrap();
        let cfg = store.load_config().value();
        assert_eq!(cfg.theme, "midnight");
        assert!(cfg.seed_by_default, "unspecified keys keep their default");
    }

    #[test]
    fn bootguard_detects_an_interrupted_run() {
        let store = temp_store("boot");
        assert!(!store.boot_was_interrupted());
        store.arm_boot_marker().unwrap();
        assert!(
            store.boot_was_interrupted(),
            "a marker left behind means a crash"
        );
        store.disarm_boot_marker().unwrap();
        assert!(!store.boot_was_interrupted());
    }

    #[test]
    fn disarming_twice_is_not_an_error() {
        let store = temp_store("boot2");
        store.disarm_boot_marker().expect("no marker is fine");
        store.arm_boot_marker().unwrap();
        store.disarm_boot_marker().unwrap();
        store.disarm_boot_marker().expect("idempotent");
    }

    #[test]
    fn clean_exit_flushes_before_standing_the_breaker_down() {
        let store = temp_store("exit");
        store.arm_boot_marker().unwrap();
        let items = vec![item('a', QueueStatus::Downloading)];
        store.flush_and_disarm(&items).unwrap();

        assert!(!store.boot_was_interrupted());
        assert_eq!(store.load_ledger().value(), items, "state landed first");
    }

    #[test]
    fn a_failed_flush_leaves_the_breaker_armed() {
        // The ordering guarantee that matters: if we cannot save state, the
        // next boot must still know this run did not finish cleanly.
        let store = Store::new(std::env::temp_dir().join("harbour-test-exit-ro/nested"));
        fs::create_dir_all(store.root()).unwrap();
        store.arm_boot_marker().unwrap();

        // Make the ledger path un-writable by turning it into a directory.
        fs::create_dir_all(store.ledger_path()).unwrap();
        assert!(
            store
                .flush_and_disarm(&[item('a', QueueStatus::Queued)])
                .is_err()
        );
        assert!(
            store.boot_was_interrupted(),
            "a failed flush must not clear the crash marker"
        );
        let _ = fs::remove_dir_all(store.root());
    }
}
