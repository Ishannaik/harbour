//! Inbox queue for asynchronous download requests from MCP clients (FR-114..115).
//!
//! To prevent concurrency bugs and lock contention across multiple processes,
//! only the TUI process ever writes to `downloads.json` or instantiates the
//! torrent engine. MCP clients write atomic request files into `<state>/inbox/`
//! which are drained periodically and on boot by the running TUI.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::mcp::downloads::AddDownloadOutcome;
use crate::persist::{Store, atomic_write};

/// One request written to the inbox by an MCP client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxRequest {
    pub magnet: String,
    #[serde(default)]
    pub dir: Option<PathBuf>,
    pub requested_at_ms: u64,
}

/// An inbox file that was successfully read and parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInboxItem {
    pub path: PathBuf,
    pub request: InboxRequest,
}

/// Result of draining the inbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxDrainResult {
    pub items: Vec<PendingInboxItem>,
    pub rejected: Vec<PathBuf>,
}

/// Writes an inbox request file atomically to `<state>/inbox/<unix_ms>-<hash>.json`.
pub fn write_inbox_request(
    store: &Store,
    magnet: &str,
    hash: &str,
    dir: Option<PathBuf>,
) -> Result<AddDownloadOutcome, String> {
    let inbox_dir = store.inbox_path();
    if let Err(err) = fs::create_dir_all(&inbox_dir) {
        return Err(format!("could not create inbox directory: {err}"));
    }

    let unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let req = InboxRequest {
        magnet: magnet.to_owned(),
        dir,
        requested_at_ms: unix_ms,
    };

    let filename = format!("{unix_ms}-{hash}.json");
    let target_path = inbox_dir.join(&filename);
    let bytes = serde_json::to_vec_pretty(&req)
        .map_err(|e| format!("could not serialize inbox request: {e}"))?;

    atomic_write(&target_path, &bytes)
        .map_err(|e| format!("could not write inbox request: {e}"))?;

    Ok(AddDownloadOutcome {
        queued_via: "inbox".into(),
        file: target_path.display().to_string(),
        picked_up_by: "running TUI, or next launch".into(),
    })
}

/// Drains the inbox, returning successfully parsed requests in oldest-first
/// order by filename, and moving unparseable files to `inbox/rejected/`.
pub fn drain_inbox(store: &Store) -> InboxDrainResult {
    let inbox_dir = store.inbox_path();
    if !inbox_dir.is_dir() {
        return InboxDrainResult::default();
    }

    let entries = match fs::read_dir(&inbox_dir) {
        Ok(e) => e,
        Err(_) => return InboxDrainResult::default(),
    };

    let mut paths = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_visible_file = path.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                // Only finished requests: atomic_write stages `<name>.<n>.tmp` in this same
                // directory, and draining that mid-write would reject a good request.
                .is_some_and(|name| !name.starts_with('.') && name.ends_with(".json"));
        if is_visible_file {
            paths.push(path);
        }
    }

    // Sort oldest-first by filename (timestamps prefix the filename).
    paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    let rejected_dir = store.inbox_rejected_path();
    let mut items = Vec::new();
    let mut rejected = Vec::new();

    for path in paths {
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => {
                reject_file(&rejected_dir, &path, &mut rejected);
                continue;
            }
        };

        match serde_json::from_slice::<InboxRequest>(&bytes) {
            Ok(request) => {
                if crate::core::magnet::info_hash_from_magnet(&request.magnet).is_some() {
                    items.push(PendingInboxItem { path, request });
                } else {
                    reject_file(&rejected_dir, &path, &mut rejected);
                }
            }
            Err(_) => {
                reject_file(&rejected_dir, &path, &mut rejected);
            }
        }
    }

    InboxDrainResult { items, rejected }
}

fn reject_file(rejected_dir: &Path, file_path: &Path, rejected: &mut Vec<PathBuf>) {
    let _ = fs::create_dir_all(rejected_dir);
    if let Some(file_name) = file_path.file_name() {
        let dest = rejected_dir.join(file_name);
        if fs::rename(file_path, &dest).is_ok() {
            rejected.push(dest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(label: &str) -> (Store, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("harbour-inbox-test-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        (Store::new(&root), root)
    }

    #[test]
    fn drain_empty_inbox_returns_empty_result() {
        let (store, _root) = temp_store("empty");
        let result = drain_inbox(&store);
        assert!(result.items.is_empty());
        assert!(result.rejected.is_empty());
    }

    #[test]
    fn drain_orders_oldest_first_by_filename() {
        let (store, _root) = temp_store("ordering");
        let inbox = store.inbox_path();
        fs::create_dir_all(&inbox).unwrap();

        let magnet1 = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234561";
        let magnet2 = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234562";
        let magnet3 = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234563";

        // Write files with explicit timestamp prefixes in reverse order
        let file3 = inbox.join("1003-hash.json");
        let file1 = inbox.join("1001-hash.json");
        let file2 = inbox.join("1002-hash.json");

        let write_req = |path: &Path, mag: &str| {
            let req = InboxRequest {
                magnet: mag.to_string(),
                dir: None,
                requested_at_ms: 1000,
            };
            fs::write(path, serde_json::to_string(&req).unwrap()).unwrap();
        };

        write_req(&file3, magnet3);
        write_req(&file1, magnet1);
        write_req(&file2, magnet2);

        let result = drain_inbox(&store);
        assert_eq!(result.items.len(), 3);
        assert!(result.rejected.is_empty());
        assert_eq!(result.items[0].request.magnet, magnet1);
        assert_eq!(result.items[1].request.magnet, magnet2);
        assert_eq!(result.items[2].request.magnet, magnet3);
    }

    #[test]
    fn in_flight_tmp_files_are_left_alone() {
        let (store, _root) = temp_store("inbox-tmp");
        std::fs::create_dir_all(store.inbox_path()).unwrap();
        let tmp = store.inbox_path().join("1-abc.json.123.tmp");
        std::fs::write(&tmp, b"{\"magnet\": \"half-writ").unwrap();
        let result = drain_inbox(&store);
        assert!(result.items.is_empty() && result.rejected.is_empty());
        assert!(
            tmp.exists(),
            "a staged write must not be moved to rejected/"
        );
    }

    #[test]
    fn unparseable_files_are_moved_to_rejected() {
        let (store, _root) = temp_store("rejection");
        let inbox = store.inbox_path();
        fs::create_dir_all(&inbox).unwrap();

        let bad_json = inbox.join("1001-bad.json");
        fs::write(&bad_json, b"{ invalid json").unwrap();

        let invalid_magnet = inbox.join("1002-invalid-mag.json");
        fs::write(
            &invalid_magnet,
            br#"{"magnet":"not-a-magnet","requested_at_ms":1002}"#,
        )
        .unwrap();

        let good = inbox.join("1003-good.json");
        let valid_magnet = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
        fs::write(
            &good,
            format!(r#"{{"magnet":"{valid_magnet}","requested_at_ms":1003}}"#),
        )
        .unwrap();

        let result = drain_inbox(&store);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].request.magnet, valid_magnet);
        assert_eq!(result.rejected.len(), 2);

        let rejected_dir = store.inbox_rejected_path();
        assert!(rejected_dir.join("1001-bad.json").exists());
        assert!(rejected_dir.join("1002-invalid-mag.json").exists());
        assert!(!bad_json.exists());
        assert!(!invalid_magnet.exists());
    }

    #[test]
    fn write_inbox_request_creates_valid_file() {
        let (store, _root) = temp_store("write");
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let magnet = format!("magnet:?xt=urn:btih:{hash}");

        let outcome =
            write_inbox_request(&store, &magnet, hash, Some(PathBuf::from("/tmp/dl"))).unwrap();
        assert_eq!(outcome.queued_via, "inbox");
        assert_eq!(outcome.picked_up_by, "running TUI, or next launch");
        assert!(Path::new(&outcome.file).exists());

        let drained = drain_inbox(&store);
        assert_eq!(drained.items.len(), 1);
        assert_eq!(drained.items[0].request.magnet, magnet);
        assert_eq!(drained.items[0].request.dir, Some(PathBuf::from("/tmp/dl")));
    }
}
