//! Download management tools for the MCP server (FR-113, FR-114).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::types::QueueItem;
use crate::inbox::write_inbox_request;
use crate::persist::{Loaded, Store};

/// Read-only snapshot of one download item from the ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadView {
    pub id: String,
    pub name: String,
    pub status: String,
    pub progress: f64,
    pub size: u64,
    pub output_dir: String,
    pub dir: String,
}

impl From<&QueueItem> for DownloadView {
    fn from(item: &QueueItem) -> Self {
        let dir_str = item.dir.display().to_string();
        Self {
            id: item.id.clone(),
            name: item.name.clone(),
            status: item.status.label().to_string(),
            // When reading from the ledger without an active engine session,
            // progress is derived purely from whether the item ever finished.
            progress: if item.finished { 1.0 } else { 0.0 },
            size: item.total_bytes,
            output_dir: dir_str.clone(),
            dir: dir_str,
        }
    }
}

/// Result returned when a download is accepted into the inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddDownloadOutcome {
    pub queued_via: String,
    pub file: String,
    pub picked_up_by: String,
}

/// Abstract downloads handler allowing mock result injection in tests.
pub trait DownloadsHandler: Send + Sync {
    /// Returns the read-only ledger view of downloads.
    fn list_downloads(&self) -> Result<Vec<DownloadView>, String>;

    /// Queues a new download request to the inbox.
    fn add_download(
        &self,
        magnet: Option<&str>,
        info_hash: Option<&str>,
        dir: Option<&str>,
    ) -> Result<AddDownloadOutcome, String>;
}

/// Production downloads handler backed by `Store`.
pub struct LiveDownloads {
    store: Store,
}

impl LiveDownloads {
    /// Creates a downloads handler using the default state directory.
    pub fn new() -> Self {
        Self {
            store: Store::from_env(),
        }
    }

    /// Creates a downloads handler using an explicit store root (for tests).
    #[cfg(test)]
    pub fn with_store(store: Store) -> Self {
        Self { store }
    }
}

impl Default for LiveDownloads {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadsHandler for LiveDownloads {
    fn list_downloads(&self) -> Result<Vec<DownloadView>, String> {
        match self.store.load_ledger() {
            Loaded::Ok(items) => Ok(items.iter().map(DownloadView::from).collect()),
            Loaded::Recovered { warning, .. } => Err(warning),
        }
    }

    fn add_download(
        &self,
        magnet: Option<&str>,
        info_hash: Option<&str>,
        dir: Option<&str>,
    ) -> Result<AddDownloadOutcome, String> {
        // Enforce exactly one of magnet or info_hash
        let (magnet_str, hash) = match (magnet, info_hash) {
            (Some(m), None) => {
                let m = m.trim();
                if !m
                    .get(..8)
                    .is_some_and(|p| p.eq_ignore_ascii_case("magnet:?"))
                {
                    return Err(
                        "that magnet link has no usable 40-hex infohash (xt=urn:btih:...)".into(),
                    );
                }
                let h = crate::core::magnet::info_hash_from_magnet(m).ok_or_else(|| {
                    "that magnet link has no usable 40-hex infohash (xt=urn:btih:...)".to_string()
                })?;
                (m.to_string(), h)
            }
            (None, Some(h)) => {
                let h = h.trim();
                if !crate::core::magnet::is_info_hash(h) {
                    return Err("that doesn't look like a usable 40-hex infohash".into());
                }
                let norm = crate::core::magnet::normalize_info_hash(h)
                    .ok_or_else(|| "that doesn't look like a usable 40-hex infohash".to_string())?;
                let m = crate::core::magnet::build_magnet(&norm, &norm);
                (m, norm)
            }
            _ => return Err("must provide exactly one of 'magnet' or 'info_hash'".into()),
        };

        let dir_buf = match dir {
            Some(d) => {
                let d = d.trim();
                let path = PathBuf::from(d);
                if !path.is_absolute() {
                    return Err("dir must be an absolute path".into());
                }
                Some(path)
            }
            None => None,
        };

        write_inbox_request(&self.store, &magnet_str, &hash, dir_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::core::types::QueueStatus;
    use crate::inbox::InboxRequest;

    fn temp_store(label: &str) -> (Store, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("harbour-dl-test-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        (Store::new(&root), root)
    }

    #[test]
    fn list_downloads_empty_ledger() {
        let (store, _root) = temp_store("empty");
        let handler = LiveDownloads::with_store(store);
        let list = handler.list_downloads().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn list_downloads_sample_ledger() {
        let (store, _root) = temp_store("sample");
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let item = QueueItem {
            id: hash.into(),
            name: "Debian ISO".into(),
            source: None,
            magnet: Some(format!("magnet:?xt=urn:btih:{hash}")),
            dir: PathBuf::from("/downloads/iso"),
            status: QueueStatus::Downloading,
            finished: false,
            bytes: None,
            total_bytes: 42_000_000,
            error: None,
            only_files: None,
            added_at_epoch_ms: 1000,
        };
        store.save_ledger(&[item]).unwrap();

        let handler = LiveDownloads::with_store(store);
        let list = handler.list_downloads().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, hash);
        assert_eq!(list[0].name, "Debian ISO");
        assert_eq!(list[0].status, "downloading");
        assert_eq!(list[0].progress, 0.0);
        assert_eq!(list[0].size, 42_000_000);
        assert_eq!(list[0].output_dir, "/downloads/iso");
        assert_eq!(list[0].dir, "/downloads/iso");
    }

    #[test]
    fn list_downloads_corrupt_ledger() {
        let (store, _root) = temp_store("corrupt");
        fs::write(store.ledger_path(), b"{ not valid json").unwrap();

        let handler = LiveDownloads::with_store(store);
        let err = handler.list_downloads().unwrap_err();
        assert!(err.contains("downloads.json"), "error must name the file");
    }

    #[test]
    fn add_download_validation_both_or_neither() {
        let (store, _root) = temp_store("val-both-neither");
        let handler = LiveDownloads::with_store(store);

        let err_neither = handler.add_download(None, None, None).unwrap_err();
        assert!(err_neither.contains("must provide exactly one"));

        let err_both = handler
            .add_download(
                Some("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"),
                Some("0123456789abcdef0123456789abcdef01234567"),
                None,
            )
            .unwrap_err();
        assert!(err_both.contains("must provide exactly one"));
    }

    #[test]
    fn add_download_validation_bad_magnet() {
        let (store, _root) = temp_store("val-bad-mag");
        let handler = LiveDownloads::with_store(store);

        let err = handler
            .add_download(Some("not_a_magnet"), None, None)
            .unwrap_err();
        assert!(err.contains("no usable 40-hex infohash"));

        let err2 = handler
            .add_download(Some("magnet:?xt=urn:btih:short"), None, None)
            .unwrap_err();
        assert!(err2.contains("no usable 40-hex infohash"));
    }

    #[test]
    fn add_download_validation_bad_hash() {
        let (store, _root) = temp_store("val-bad-hash");
        let handler = LiveDownloads::with_store(store);

        let err = handler
            .add_download(None, Some("not-a-hash"), None)
            .unwrap_err();
        assert!(err.contains("doesn't look like a usable 40-hex infohash"));
    }

    #[test]
    fn add_download_validation_relative_dir() {
        let (store, _root) = temp_store("val-rel-dir");
        let handler = LiveDownloads::with_store(store);

        let err = handler
            .add_download(
                Some("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"),
                None,
                Some("relative/path"),
            )
            .unwrap_err();
        assert!(err.contains("dir must be an absolute path"));
    }

    #[test]
    fn add_download_writes_file_contents() {
        let (store, _root) = temp_store("write-contents");
        let handler = LiveDownloads::with_store(store.clone());
        let hash = "0123456789abcdef0123456789abcdef01234567";

        let outcome = handler
            .add_download(None, Some(hash), Some("/var/downloads"))
            .unwrap();

        assert_eq!(outcome.queued_via, "inbox");
        assert_eq!(outcome.picked_up_by, "running TUI, or next launch");
        assert!(outcome.file.ends_with(&format!("-{hash}.json")));

        let content = fs::read_to_string(&outcome.file).unwrap();
        let parsed: InboxRequest = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.dir, Some(PathBuf::from("/var/downloads")));
        assert!(parsed.magnet.contains(hash));
        assert!(parsed.requested_at_ms > 0);
    }
}
