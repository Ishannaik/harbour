//! Local shareable bug report (`FR-04a`).
//!
//! `harbour --bugreport` and TUI `shift+L` write `bugreport.txt` under the
//! state dir. Home-directory prefixes become `~`; infohashes stay. No extra
//! telemetry — Sentry panics are a separate path (`FR-09a`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::core::paths;

/// Last N lines of `harbour.log` / `crash.log` included in the report.
pub const TAIL_LINES: usize = 200;

/// Banner shown after `shift+L` copies the path.
pub const COPIED_BANNER: &str = "copied bugreport path";

/// Env + version snapshot so tests do not read the process environment.
pub struct Snapshot {
    pub version: String,
    pub os: String,
    pub term: Option<String>,
    pub wt_session: Option<String>,
    pub home: Option<PathBuf>,
}

impl Snapshot {
    fn from_env() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            term: std::env::var("TERM").ok().filter(|s| !s.is_empty()),
            wt_session: std::env::var("WT_SESSION").ok().filter(|s| !s.is_empty()),
            home: dirs::home_dir(),
        }
    }
}

/// Clipboard program used to copy the report path. Windows only (`clip`).
pub fn clipboard_program() -> Option<&'static str> {
    #[cfg(windows)]
    {
        Some("clip")
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Replace the home-directory prefix with `~` in `text`. Infohashes are kept.
pub fn redact_home(text: &str, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return text.to_string();
    };
    let home_str = home.to_string_lossy();
    if home_str.is_empty() {
        return text.to_string();
    }
    let mut out = text.replace(home_str.as_ref(), "~");
    let slash = home_str.replace('\\', "/");
    if slash != home_str {
        out = out.replace(&slash, "~");
    }
    let back = home_str.replace('/', "\\");
    if back != home_str {
        out = out.replace(&back, "~");
    }
    out
}

fn last_n_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

fn read_tail(path: &Path, n: usize) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    Some(last_n_lines(&text, n))
}

fn section_file(label: &str, path: &Path, missing: &str) -> String {
    match read_tail(path, TAIL_LINES) {
        Some(body) if !body.is_empty() => format!("--- {label} ---\n{body}\n"),
        Some(_) => format!("--- {label} ---\n(empty)\n"),
        None => format!("--- {label} ---\n{missing}\n"),
    }
}

fn section_small(label: &str, path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(body) if !body.trim().is_empty() => {
            format!("--- {label} ---\npresent\n{}\n", body.trim_end())
        }
        Ok(_) => format!("--- {label} ---\npresent\n(empty)\n"),
        Err(_) => format!("--- {label} ---\nabsent\n"),
    }
}

/// Build the shareable report body from files under `root`.
pub fn render(root: &Path, snap: &Snapshot) -> String {
    let config = paths::config_file(root);
    let mut body = String::new();
    body.push_str("harbour bugreport\n");
    body.push_str("version: harbour ");
    body.push_str(&snap.version);
    body.push('\n');
    body.push_str("os: ");
    body.push_str(&snap.os);
    body.push('\n');
    body.push_str("term: ");
    body.push_str(snap.term.as_deref().unwrap_or("(unset)"));
    body.push('\n');
    body.push_str("wt_session: ");
    body.push_str(snap.wt_session.as_deref().unwrap_or("(unset)"));
    body.push('\n');
    body.push_str("config: ");
    body.push_str(&config.display().to_string());
    body.push('\n');
    body.push('\n');
    body.push_str(&section_small(
        "boot.marker",
        &paths::boot_marker_file(root),
    ));
    body.push('\n');
    body.push_str(&section_small(
        "indexer.port",
        &paths::indexer_port_file(root),
    ));
    body.push('\n');
    body.push_str(&section_file(
        "crash.log (last 200 lines)",
        &paths::crash_log_file(root),
        "(none)",
    ));
    body.push('\n');
    body.push_str(&section_file(
        "harbour.log (last 200 lines)",
        &paths::harbour_log_file(root),
        "(none)",
    ));
    redact_home(&body, snap.home.as_deref())
}

/// Write `bugreport.txt` under `root` and return its path.
pub fn write_bugreport(root: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    let path = paths::bugreport_file(root);
    let body = render(root, &Snapshot::from_env());
    fs::write(&path, body)?;
    Ok(path)
}

/// Copy `text` to the clipboard. Windows uses `clip`; other OS: unsupported.
pub fn copy_to_clipboard(text: &str) -> io::Result<()> {
    let Some(program) = clipboard_program() else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "clipboard copy is Windows-only (clip)",
        ));
    };
    let mut child = std::process::Command::new(program)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{program} exited {status}")))
    }
}

/// Write the report and try to copy its path. `true` when the clipboard copy
/// succeeded.
pub fn share_bugreport(root: &Path) -> io::Result<(PathBuf, bool)> {
    let path = write_bugreport(root)?;
    let copied = copy_to_clipboard(&path.display().to_string()).is_ok();
    Ok((path, copied))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("harbour-bugreport-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temp root");
        root
    }

    fn snap(home: &Path) -> Snapshot {
        Snapshot {
            version: "0.1.0".into(),
            os: "windows x86_64".into(),
            term: Some("xterm-256color".into()),
            wt_session: Some("session-id".into()),
            home: Some(home.to_path_buf()),
        }
    }

    #[test]
    fn render_includes_version_os_term_and_redacts_home() {
        let home = temp_root("home");
        let root = home.join(".harbour");
        fs::create_dir_all(&root).expect("root");
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let crash_path = format!(
            "crash in {}{}cache{}{hash}",
            home.join(".harbour").display(),
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR,
        );
        fs::write(root.join("crash.log"), format!("{crash_path}\n")).unwrap();
        let body = render(&root, &snap(&home));
        assert!(body.contains("version: harbour 0.1.0"), "{body}");
        assert!(body.contains("os: windows x86_64"), "{body}");
        assert!(body.contains("term: xterm-256color"), "{body}");
        assert!(body.contains("wt_session: session-id"), "{body}");
        assert!(body.contains("config: ~"), "{body}");
        assert!(body.contains("config.toml"), "{body}");
        let home_str = home.display().to_string();
        assert!(!body.contains(&home_str), "home must be ~, got:\n{body}");
        assert!(body.contains(hash), "infohashes are kept:\n{body}");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn render_tails_crash_log_and_notes_missing_files() {
        let root = temp_root("tail");
        let mut crash = String::new();
        for i in 0..250 {
            crash.push_str(&format!("crash-line-{i}\n"));
        }
        fs::write(root.join("crash.log"), crash).unwrap();
        let home = PathBuf::from("/home/testhome");
        let body = render(&root, &snap(&home));
        assert!(body.contains("crash-line-249"), "{body}");
        assert!(body.contains("crash-line-50"), "{body}");
        assert!(
            !body.contains("crash-line-49"),
            "only last 200 lines:\n{body}"
        );
        assert!(body.contains("--- boot.marker ---\nabsent"), "{body}");
        assert!(body.contains("--- indexer.port ---\nabsent"), "{body}");
        assert!(
            body.contains("--- harbour.log (last 200 lines) ---\n(none)"),
            "{body}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn render_includes_boot_marker_and_indexer_port() {
        let root = temp_root("marker");
        fs::write(root.join("boot.marker"), "harbour boot in progress\n").unwrap();
        fs::write(root.join("indexer.port"), "8765\n").unwrap();
        fs::write(root.join("harbour.log"), "search ok\n").unwrap();
        let home = PathBuf::from("/home/testhome");
        let body = render(&root, &snap(&home));
        assert!(body.contains("harbour boot in progress"), "{body}");
        assert!(body.contains("8765"), "{body}");
        assert!(body.contains("search ok"), "{body}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn write_bugreport_creates_the_file_under_root() {
        let root = temp_root("write");
        let path = write_bugreport(&root).expect("write");
        assert_eq!(path, root.join("bugreport.txt"));
        let body = fs::read_to_string(&path).expect("read");
        assert!(body.contains("harbour bugreport"), "{body}");
        assert!(
            body.contains(&format!("version: harbour {}", env!("CARGO_PKG_VERSION"))),
            "{body}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn windows_clipboard_program_is_clip() {
        #[cfg(windows)]
        assert_eq!(clipboard_program(), Some("clip"));
        #[cfg(not(windows))]
        assert_eq!(clipboard_program(), None);
    }

    #[test]
    fn copied_banner_is_the_share_copy() {
        assert_eq!(COPIED_BANNER, "copied bugreport path");
    }
}
