//! Minimal queue ledger — MVP persistence for the UI track (FR-48/FR-55).
//!
//! Every queue mutation (enqueue, pause/resume) is written atomically to
//! `downloads.json` (temp file + rename, FR-55), and loaded at boot, so
//! status changes survive restart. `HARBOUR_STATE_DIR` relocates the file
//! for tests (AGENTS.md env vocabulary).
//!
//! This is the *UI track's* stopgap until the engine track lands its full
//! persistence (phase 4/5): librqbit resume state, bootguard safe mode,
//! history.json, corrupt-file quarantine are all engine-track. The ledger
//! format is the frozen `QueueItem` serde round-trip, so the engine can
//! adopt the file unchanged. Every failure is loud — never a silent drop.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::types::QueueItem;

/// The state dir: `$HARBOUR_STATE_DIR` when set, else `~/.harbour`
/// (`%USERPROFILE%\.harbour` on Windows). Created on demand.
pub fn state_dir() -> PathBuf {
    let dir = match std::env::var_os("HARBOUR_STATE_DIR") {
        Some(state) => PathBuf::from(state),
        None => dirs::home_dir()
            .unwrap_or_else(|| {
                eprintln!("queue ledger: no home dir; using ./.harbour");
                PathBuf::from(".harbour")
            })
            .join(".harbour"),
    };
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("queue ledger: cannot create {}: {e}", dir.display());
    }
    dir
}

/// Outcome of loading the ledger — `Corrupt` is distinct from `Missing` so
/// the caller can quarantine the bad file instead of silently overwriting it
/// (FR-54's loud-failure rule, engine track owns the full behavior).
#[derive(Debug, PartialEq)]
pub enum Load {
    Ok(Vec<QueueItem>),
    Missing,
    Corrupt,
}

/// Loads `path`; corrupt files are renamed to `<name>.corrupt` (quarantined,
/// never overwritten silently) before returning `Corrupt`.
pub fn load(path: &Path) -> Load {
    let raw = match fs::read(path) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Load::Missing,
        Err(e) => {
            eprintln!("queue ledger: cannot read {}: {e}", path.display());
            return Load::Missing;
        }
    };
    match serde_json::from_slice(&raw) {
        Ok(items) => Load::Ok(items),
        Err(e) => {
            eprintln!(
                "queue ledger: corrupt {} — quarantining: {e}",
                path.display()
            );
            let quarantine = path.with_extension("json.corrupt");
            let _ = fs::rename(path, &quarantine);
            Load::Corrupt
        }
    }
}

/// Writes the queue atomically (temp + rename on the same volume, FR-55): a
/// crash mid-write leaves either the old or the new file, never a partial
/// one. Loud on failure — never a silent drop.
pub fn save(path: &Path, items: &[QueueItem]) {
    let parent = path.parent().unwrap_or(Path::new("."));
    if let Err(e) = fs::create_dir_all(parent) {
        eprintln!("queue ledger: cannot create {}: {e}", parent.display());
        return;
    }
    let json = match serde_json::to_vec(items) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("queue ledger: cannot serialize queue: {e}");
            return;
        }
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = fs::write(&tmp, json) {
        eprintln!("queue ledger: cannot write {}: {e}", tmp.display());
        return;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        eprintln!(
            "queue ledger: cannot move {} -> {}: {e}",
            tmp.display(),
            path.display()
        );
        let _ = fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::QueueStatus;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A unique temp dir per test — no shared state, no env mutation.
    fn temp_ledger() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("harbour-ledger-{nanos}"));
        dir.join("downloads.json")
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_ledger();
        let items = crate::fake::fake_queue();
        save(&path, &items);
        match load(&path) {
            Load::Ok(loaded) => assert_eq!(loaded, items),
            other => panic!("expected Ok, got {other:?}"),
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn pause_status_survives_round_trip() {
        let path = temp_ledger();
        let mut items = crate::fake::fake_queue();
        items[2].status = QueueStatus::Paused; // the seeding item, paused
        items[2].upload_speed_mib = 0.0;
        save(&path, &items);
        match load(&path) {
            Load::Ok(loaded) => {
                assert_eq!(loaded[2].status, QueueStatus::Paused);
                assert!(loaded[2].finished, "paused seed stays finished");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_loads_as_missing() {
        let path = temp_ledger();
        assert!(matches!(load(&path), Load::Missing));
    }

    #[test]
    fn corrupt_file_is_quarantined_not_overwritten() {
        let path = temp_ledger();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{not json").unwrap();
        assert!(matches!(load(&path), Load::Corrupt));
        assert!(
            path.with_extension("json.corrupt").exists(),
            "corrupt ledger must be quarantined, not clobbered"
        );
        let _ = fs::remove_file(path.with_extension("json.corrupt"));
    }
}
