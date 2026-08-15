//! Auto-start a sibling `harbour-indexer` so `harbour` alone is enough.
//!
//! The client still contains zero scrapers. This only launches a binary the
//! user already has, the same way `start-harbour.ps1` does.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const DEFAULT_URL: &str = "http://127.0.0.1:8765";
const ENV_SKIP: &str = "HARBOUR_INDEXER_SKIP";
const ENV_BIN: &str = "HARBOUR_INDEXER";

#[cfg(windows)]
const BIN_NAME: &str = "harbour-indexer.exe";
#[cfg(not(windows))]
const BIN_NAME: &str = "harbour-indexer";

/// If `/health` is already up, do nothing. Otherwise spawn the first indexer
/// binary we can find and wait briefly for `/health`.
pub async fn ensure_local_indexer() {
    if std::env::var_os(ENV_SKIP).is_some() {
        return;
    }

    let url = crate::sources::resolve_indexer_url(DEFAULT_URL);
    if health_ok(&url).await {
        return;
    }

    // Clean up any stale orphaned indexers from previous crashes
    cleanup_orphaned_indexers();

    let Some(bin) = find_indexer_bin() else {
        eprintln!(
            "harbour: search needs harbour-indexer next to this exe, \
             in %USERPROFILE%\\.harbour\\bin, or on PATH"
        );
        return;
    };

    if let Err(err) = spawn_detached(&bin) {
        eprintln!("harbour: could not start {}: {err}", bin.display());
        return;
    }
    eprintln!("harbour: started {}", bin.display());

    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let url = crate::sources::resolve_indexer_url(DEFAULT_URL);
        if health_ok(&url).await {
            return;
        }
    }
    eprintln!(
        "harbour: indexer started but /health is not answering yet — search may take a moment"
    );
}

fn indexer_name() -> &'static str {
    BIN_NAME
}

/// Places we look, in order. `HARBOUR_INDEXER` wins when set.
pub fn indexer_bin_candidates(exe_dir: &Path, state_root: &Path) -> Vec<PathBuf> {
    let name = indexer_name();
    vec![
        exe_dir.join(name),
        state_root.join("bin").join(name),
        exe_dir
            .join("..")
            .join("harbour-indexer")
            .join("target")
            .join("release")
            .join(name),
        exe_dir
            .join("..")
            .join("..")
            .join("..")
            .join("harbour-indexer")
            .join("target")
            .join("release")
            .join(name),
        exe_dir
            .join("..")
            .join("..")
            .join("..")
            .join("..")
            .join("harbour-indexer")
            .join("target")
            .join("release")
            .join(name),
    ]
}

fn find_indexer_bin() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(ENV_BIN) {
        let path = PathBuf::from(explicit.trim());
        if path.is_file() {
            return Some(path);
        }
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let state = crate::core::paths::state_dir();
    for candidate in indexer_bin_candidates(&exe_dir, &state) {
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    which_on_path(indexer_name())
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(windows)]
fn cleanup_orphaned_indexers() {
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", "harbour-indexer.exe"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn cleanup_orphaned_indexers() {}

fn spawn_detached(bin: &Path) -> std::io::Result<()> {
    let mut cmd = Command::new(bin);
    cmd.arg("--parent-pid").arg(std::process::id().to_string());
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _child = cmd.spawn()?;
    Ok(())
}

async fn health_ok(base_url: &str) -> bool {
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(400))
        .timeout(Duration::from_millis(600))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(url).send().await {
        Ok(res) => res.status().is_success(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_prefer_same_folder_then_state_bin() {
        let exe = PathBuf::from(r"C:\tools\harbour");
        let state = PathBuf::from(r"C:\Users\me\.harbour");
        let list = indexer_bin_candidates(&exe, &state);
        assert_eq!(list[0], exe.join(BIN_NAME));
        assert_eq!(list[1], state.join("bin").join(BIN_NAME));
    }
}
