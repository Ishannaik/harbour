//! Watch mode (FR-57..FR-61): serves an item's primary media file over a
//! loopback HTTP endpoint with Range support and launches an external player
//! (mpv → VLC → Windows Media Player — the player is the renderer; harbour
//! ships no render engine).
//!
//! Architecture follows the spec and the proven rqbit/stremio pattern:
//! an HTTP stream URL + Range requests give the player seek while it reads.
//! This module serves *files on disk*; when the engine track lands
//! (phase 4/6), the stream URL will come from librqbit's live session
//! instead — the launch/lifecycle/UI stay identical. Loopback only, FR-61.
//!
//! All I/O here is std-only: no server framework for a two-endpoint
//! single-file server (lean-dependency rule, AGENTS.md).

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// Media extensions a finished item may contain, in no particular order —
/// the largest match wins.
const MEDIA_EXTS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "webm", "m4v", "ts", "flv", "wmv", "mpg", "mpeg",
];

/// Packed releases (FitGirl and friends). Never a watchable video — mpv on a
/// zip is issue #76.
const ARCHIVE_EXTS: &[&str] = &["zip", "rar", "7z"];

/// Sidecar subtitle extensions. Embedded MKV tracks stay in the player —
/// harbour does not parse containers (issue #80).
const SUB_EXTS: &[&str] = &["srt", "ass"];

/// Read chunk for streaming the file to the player (64 KiB).
const CHUNK: usize = 64 * 1024;

/// Picks the largest media file under `dir` — the stand-in for the engine's
/// file list (FR-37 metadata). None when the dir is missing or has no
/// media — the caller surfaces a loud error, never a silent no-op.
pub fn primary_media(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| MEDIA_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
}

/// Unique lowercase extensions of files sitting in `dir` (not nested). Used
/// to name what we found when there is no video to play.
fn top_level_extensions(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ext = BTreeSet::new();
    for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
        if !path.is_file() {
            continue;
        }
        if let Some(e) = path.extension().and_then(|e| e.to_str()) {
            ext.insert(e.to_ascii_lowercase());
        }
    }
    ext.into_iter().collect()
}

/// Why this torrent cannot be watched. Callers must surface this as a banner
/// and never launch a player — zip/rar/7z are not video (issue #76).
pub fn unplayable_watch_banner(dir: &Path) -> String {
    let exts = top_level_extensions(dir);
    if !exts.is_empty() && exts.iter().all(|e| ARCHIVE_EXTS.contains(&e.as_str())) {
        return "this is an archive — extract it from the download folder (press o to open)".into();
    }
    if !exts.is_empty() {
        let listed = exts
            .iter()
            .map(|e| format!(".{e}"))
            .collect::<Vec<_>>()
            .join(" / ");
        return format!("no playable video in this torrent (found {listed})");
    }
    "no playable video in this torrent".into()
}

/// True for an external `.srt` / `.ass`. Not MKV/MP4 embedded tracks.
pub fn is_subtitle(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SUB_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

fn same_folder(a: &Path, b: &Path) -> bool {
    a.parent() == b.parent()
}

/// 0 = exact stem (`show.mkv` + `show.srt`), 1 = tagged stem (`show.en.srt`).
fn stem_rank(video: &Path, sub: &Path) -> Option<u8> {
    let v = video.file_stem()?.to_str()?.to_ascii_lowercase();
    let s = sub.file_stem()?.to_str()?.to_ascii_lowercase();
    if s == v {
        return Some(0);
    }
    if s.starts_with(&format!("{v}.")) || s.starts_with(&format!("{v}_")) {
        return Some(1);
    }
    None
}

/// Pick a `.srt`/`.ass` sitting next to `video`. Prefers a matching stem so a
/// multi-episode folder does not glue episode 2's sub onto episode 1. A lone
/// unmatched sidecar in the same folder still counts (`movie.mkv` + English.srt).
pub fn sidecar_for<'a, I>(video: &Path, candidates: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = &'a Path>,
{
    let same: Vec<&Path> = candidates
        .into_iter()
        .filter(|p| is_subtitle(p) && same_folder(video, p))
        .collect();
    same.iter()
        .filter_map(|p| stem_rank(video, p).map(|r| (r, *p)))
        .min_by_key(|(r, p)| (*r, p.as_os_str()))
        .map(|(_, p)| p.to_path_buf())
        .or_else(|| (same.len() == 1).then(|| same[0].to_path_buf()))
}

/// Disk scan: sidecar next to a video file that is already on disk.
pub fn sidecar_beside(video: &Path) -> Option<PathBuf> {
    let dir = video.parent()?;
    let paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    sidecar_for(video, paths.iter().map(PathBuf::as_path))
}

/// Absolute path of the sidecar to pass as `--sub-file`. `video_rel` is the
/// torrent path of the file being watched; `torrent_subs` are relative names
/// from the engine. Falls back to scanning the download dir.
pub fn resolve_sidecar(
    dir: &Path,
    video_rel: Option<&str>,
    torrent_subs: &[String],
) -> Option<PathBuf> {
    if let Some(rel) = video_rel {
        let names: Vec<&Path> = torrent_subs.iter().map(Path::new).collect();
        if let Some(found) = sidecar_for(Path::new(rel), names) {
            let path = dir.join(found);
            // Metadata names are not a download. Passing a missing path to
            // mpv/VLC starts playback with no subs (and a file excluded by
            // selection never appears later in the session).
            if path.is_file() {
                return Some(path);
            }
        }
        if let Some(found) = sidecar_beside(&dir.join(rel)) {
            return Some(found);
        }
    }
    primary_media(dir).and_then(|video| sidecar_beside(&video))
}

/// `--sub-file` args for mpv (`--sub-file=path`) and VLC (`--sub-file` + path).
/// Other players (Windows Media Player) get none — harbour is not a player.
pub fn sub_file_args(player: &str, sub: &Path) -> Vec<String> {
    match player_stem(player).as_str() {
        "mpv" => vec![format!("--sub-file={}", sub.display())],
        "vlc" => vec!["--sub-file".into(), sub.display().to_string()],
        _ => Vec::new(),
    }
}

/// Lowercased executable stem, treating `\` as a separator so Windows paths
/// still identify mpv/VLC when this binary is built on Linux (CI).
fn player_stem(player: &str) -> String {
    let unified = player.replace('\\', "/");
    Path::new(&unified)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(player)
        .to_ascii_lowercase()
}

fn subtitle_for_player(player: &str, sub: Option<&Path>) -> Option<PathBuf> {
    let sub = sub?;
    if sub_file_args(player, sub).is_empty() {
        None
    } else {
        Some(sub.to_path_buf())
    }
}

fn spawn_player(player: &str, media: &str, sub: Option<&Path>) -> std::io::Result<Child> {
    let mut cmd = Command::new(player);
    if let Some(path) = sub {
        for arg in sub_file_args(player, path) {
            cmd.arg(arg);
        }
    }
    cmd.arg(media).stdin(Stdio::null()).spawn()
}

/// A running watch session: the loopback stream server + the player child.
/// Drop calls [`WatchSession::stop`] so a session can never leak a server
/// or a player process.
pub struct WatchSession {
    /// The URL handed to the player (loopback + random port).
    pub url: String,
    /// Sidecar passed as `--sub-file`, if the torrent had one next to the video.
    pub subtitle: Option<PathBuf>,
    child: Child,
    stop: Arc<AtomicBool>,
}

impl WatchSession {
    /// Serves `file` on `127.0.0.1:0` and launches `player` with the URL.
    pub fn start(
        file: &Path,
        player: &str,
        sub_file: Option<&Path>,
    ) -> std::io::Result<WatchSession> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let file = file.to_path_buf();
        // Accept loop owns the listener; per-connection threads serve one
        // request each (players open fresh connections for seeks).
        thread::spawn(move || {
            let _ = accept_loop(listener, file, stop_clone);
        });
        let url = format!("http://127.0.0.1:{port}/stream");
        let child = spawn_player(player, &url, sub_file)?;
        Ok(WatchSession {
            url,
            subtitle: subtitle_for_player(player, sub_file),
            child,
            stop,
        })
    }

    /// True once the player process has exited (the loop then returns to the
    /// TUI — the player closing ends the session).
    pub fn player_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Launches the player against an engine-provided stream URL (FR-57:
    /// Stremio-style watch while downloading). No local server — the player
    /// talks straight to librqbit's loopback HTTP API, which blocks on
    /// missing pieces and prioritizes the requested ones.
    pub fn launch_remote(
        player: &str,
        url: &str,
        sub_file: Option<&Path>,
    ) -> std::io::Result<WatchSession> {
        let stop = Arc::new(AtomicBool::new(false));
        let child = spawn_player(player, url, sub_file)?;
        Ok(WatchSession {
            url: url.to_string(),
            subtitle: subtitle_for_player(player, sub_file),
            child,
            stop,
        })
    }

    /// Stops the server and kills the player (FR-59: `q`/esc stops cleanly).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for WatchSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: TcpListener, file: PathBuf, stop: Arc<AtomicBool>) -> std::io::Result<()> {
    for conn in listener.incoming() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Ok(stream) = conn else {
            continue;
        };
        let file = file.clone();
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let _ = handle_connection(stream, &file, &stop);
        });
    }
    Ok(())
}

/// One HTTP request: parse the head, honor `Range` (seek), stream the slice.
/// HEAD is answered without a body so players can probe length.
fn handle_connection(mut stream: TcpStream, file: &Path, stop: &AtomicBool) -> std::io::Result<()> {
    let mut head = [0u8; 16 * 1024];
    let n = stream.read(&mut head)?;
    if n == 0 {
        return Ok(());
    }
    let head = String::from_utf8_lossy(&head[..n]);
    let mut lines = head.lines();
    let request = lines.next().unwrap_or("");
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("");
    let head_only = method == "HEAD";
    let range = lines
        .find_map(|l| l.strip_prefix("Range:").map(str::trim))
        .and_then(parse_range);

    let content_type = content_type(file.extension().and_then(|e| e.to_str()));
    let mut file = File::open(file)?;
    let len = file.metadata()?.len();

    // RFC 7233: a range entirely past the end is 416 with a Content-Range
    // hint; anything else we can't serve is a plain 200 of the whole file
    // (players treat that as "no seeking needed").
    if let Some((start, end)) = range {
        if start >= len {
            return write_head(
                &mut stream,
                "416 Range Not Satisfiable",
                &[
                    ("Content-Range", &format!("bytes */{len}")),
                    ("Content-Length", "0"),
                ],
                head_only,
            );
        }
        let end = end.min(len - 1);
        let slice_len = end - start + 1;
        file.seek(SeekFrom::Start(start))?;
        write_head(
            &mut stream,
            "206 Partial Content",
            &[
                ("Content-Range", &format!("bytes {start}-{end}/{len}")),
                ("Content-Length", &slice_len.to_string()),
                ("Content-Type", content_type),
                ("Accept-Ranges", "bytes"),
            ],
            head_only,
        )?;
        if !head_only {
            stream_file(&mut stream, file, stop, Some(slice_len))?;
        }
        return Ok(());
    }

    write_head(
        &mut stream,
        "200 OK",
        &[
            ("Content-Length", &len.to_string()),
            ("Content-Type", content_type),
            ("Accept-Ranges", "bytes"),
        ],
        head_only,
    )?;
    if !head_only {
        stream_file(&mut stream, file, stop, None)?;
    }
    Ok(())
}

/// `bytes=start-end` or `bytes=start-`. Malformed or suffix (`-n`) ranges
/// return None → the request is served as a full 200.
fn parse_range(value: &str) -> Option<(u64, u64)> {
    let spec = value.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.parse().ok()?;
    let end: u64 = if end.is_empty() {
        u64::MAX
    } else {
        end.parse().ok()?
    };
    Some((start, end))
}

fn write_head(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    head_only: bool,
) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\n")?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "Connection: close\r\n\r\n")?;
    stream.flush()?;
    if head_only {
        stream.shutdown(std::net::Shutdown::Write)?;
    }
    Ok(())
}

fn stream_file(
    stream: &mut TcpStream,
    file: File,
    stop: &AtomicBool,
    limit: Option<u64>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(file);
    let mut buf = vec![0u8; CHUNK];
    let mut remaining = limit.unwrap_or(u64::MAX);
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(()); // session stopped mid-stream; drop the socket
        }
        if remaining == 0 {
            break;
        }
        let want = buf.len().min(remaining as usize);
        let n = reader.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        remaining -= n as u64;
        stream.write_all(&buf[..n])?;
    }
    stream.flush()
}

/// Best-effort content type from the extension — players mostly sniff, but
/// a correct one avoids surprises.
fn content_type(ext: Option<&str>) -> &'static str {
    match ext.map(str::to_ascii_lowercase).as_deref() {
        Some("mp4" | "m4v" | "mov") => "video/mp4",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("avi") => "video/x-msvideo",
        Some("ts") => "video/mp2t",
        _ => "application/octet-stream",
    }
}

/// Every installed player, in preference order: the OS default video handler
/// (the program Windows itself opens .mkv/.mp4 with — the user's own choice,
/// e.g. VLC), then mpv, then VLC, then Windows Media Player (ships on every
/// Windows box, so a bare install always has a working default). Each entry
/// is a (display label, command path) pair — the command is what
/// `Command::new` needs, the label is what the TUI picker shows. Listing all
/// instead of stopping at the first lets the user choose.
pub fn find_players() -> Vec<(String, String)> {
    let mut players = Vec::new();
    if let Some((label, command)) = default_video_handler() {
        players.push((label, command));
    }
    if command_exists("mpv") {
        players.push(("mpv".to_string(), "mpv".to_string()));
    }
    if command_exists("vlc") {
        players.push(("VLC".to_string(), "vlc".to_string()));
    }
    // wmplayer is rarely on PATH; check the standard install roots.
    // 64-bit wmplayer lives under Program Files; the x86 root is a
    // fallback for 32-bit Windows.
    for path in [
        r"C:\Program Files\Windows Media Player\wmplayer.exe",
        r"C:\Program Files (x86)\Windows Media Player\wmplayer.exe",
    ] {
        if Path::new(path).exists() {
            players.push(("Windows Media Player".to_string(), path.to_string()));
            break;
        }
    }
    players
}

/// The program Windows has registered as the default handler for video
/// files — the user's own system-wide choice. Reads the classic association
/// chain: `HKCR\.mkv`'s default value names a ProgID, and that ProgID's
/// `shell\open\command` is the launch command. AppX handlers (Movies & TV)
/// have no open command and fall through; browsers never own a video
/// extension, so this cannot resolve to a browser. Windows only.
fn default_video_handler() -> Option<(String, String)> {
    if !cfg!(windows) {
        return None;
    }
    // Probe the common container extensions in order; the first with a
    // command wins. The swarm stream URL carries no extension, so the
    // container is unknown until a file lands — the mkv/mp4 probe is the
    // honest stand-in for "whatever this turns out to be".
    for ext in [".mkv", ".mp4", ".avi", ".webm"] {
        let Some(progid) = reg_default(&format!("HKCR\\{ext}")) else {
            continue; // no association for this container — try the next
        };
        let Some(command) = reg_default(&format!("HKCR\\{progid}\\shell\\open\\command")) else {
            continue; // AppX handlers have no open command — try the next
        };
        if let Some(exe) = command_exe(&command) {
            let label = Path::new(&exe)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("system default")
                .to_string();
            return Some((label, exe));
        }
    }
    None
}

/// The default (unnamed) value of a registry key, via `reg query`.
fn reg_default(key: &str) -> Option<String> {
    let out = Command::new("reg")
        .args(["query", key, "/ve"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Output line: `    (Default)    REG_SZ    mkvfile`
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.contains("REG_SZ"))
        .and_then(|l| l.split("REG_SZ").nth(1))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The executable path out of a shell open command. Handles the quoted form
/// (`"C:\Program Files\VideoLAN\VLC\vlc.exe" "%1"`) and the bare form
/// (`C:\vlc.exe %1`). A command that is *only* the file placeholder (`%1`)
/// names no program and yields None.
fn command_exe(command: &str) -> Option<String> {
    let trimmed = command.trim();
    let exe = if trimmed.starts_with('"') {
        trimmed
            .split_once('"')
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(exe, _)| exe.to_string())
    } else {
        trimmed.split_whitespace().next().map(str::to_string)
    };
    exe.filter(|p| !p.is_empty() && p != "%1")
}

/// The first installed player — the picker highlights this when
/// `config.player` is unset so first-run Enter confirms the detected default
/// (#73). Same as the first entry of [`find_players`].
pub fn find_player() -> Option<String> {
    find_players()
        .into_iter()
        .next()
        .map(|(_, command)| command)
}

fn command_exists(cmd: &str) -> bool {
    let probe = if cfg!(windows) { "where" } else { "which" };
    Command::new(probe)
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range("bytes=0-99"), Some((0, 99)));
        assert_eq!(parse_range("bytes=1024-"), Some((1024, u64::MAX)));
        assert_eq!(parse_range("bytes=-500"), None, "suffix ranges unsupported");
        assert_eq!(parse_range("bytes=abc-def"), None);
        assert_eq!(parse_range("garbage"), None);
    }

    #[test]
    fn shell_open_commands_yield_the_executable() {
        // The quoted form VLC/MPC register: `"C:\Program Files\VideoLAN\VLC\vlc.exe" "%1"`.
        assert_eq!(
            command_exe(r#""C:\Program Files\VideoLAN\VLC\vlc.exe" "%1""#),
            Some(r"C:\Program Files\VideoLAN\VLC\vlc.exe".to_string())
        );
        // The bare form some players register: `C:\vlc.exe %1`.
        assert_eq!(
            command_exe(r"C:\vlc.exe %1"),
            Some(r"C:\vlc.exe".to_string())
        );
        // Unparseable garbage yields no handler rather than a broken one.
        assert_eq!(command_exe(""), None);
        assert_eq!(command_exe("%1"), None);
    }

    #[test]
    fn no_video_association_falls_through_the_whole_probe() {
        // Non-Windows never probes the registry at all.
        if !cfg!(windows) {
            assert_eq!(default_video_handler(), None);
        }
        // On Windows the result is whatever the machine says — the contract
        // is only that AppX-style handlers (no open command) cannot return a
        // browser; that property is enforced by reg_default + command_exe
        // returning None for commandless ProgIDs.
    }

    #[test]
    fn primary_media_finds_largest_media_file() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("harbour-watch-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("movie.avi"), vec![0u8; 50]).unwrap();
        std::fs::write(dir.join("movie.mp4"), vec![0u8; 200]).unwrap();
        std::fs::write(dir.join("readme.txt"), b"not media").unwrap();

        let found = primary_media(&dir).expect("media file found");
        assert_eq!(found.file_name().unwrap(), "movie.mp4");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn primary_media_empty_dir_is_none() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("harbour-watch-empty-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(primary_media(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn primary_media_skips_archive_files() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("harbour-watch-zip-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("game.zip"), vec![0u8; 500]).unwrap();
        std::fs::write(dir.join("pack.rar"), vec![0u8; 400]).unwrap();
        std::fs::write(dir.join("data.7z"), vec![0u8; 300]).unwrap();
        std::fs::write(dir.join("readme.txt"), b"not media").unwrap();
        assert!(
            primary_media(&dir).is_none(),
            "archives must never be picked as watchable media"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn scratch_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("harbour-watch-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn archive_only_dir_tells_the_user_to_extract() {
        let dir = scratch_dir("archive-banner");
        std::fs::write(dir.join("game.zip"), b"PK").unwrap();
        let msg = unplayable_watch_banner(&dir);
        assert!(
            msg.contains("archive"),
            "zip-only torrents must say they are archives, got: {msg}"
        );
        assert!(
            msg.contains("extract"),
            "the banner must tell the user to extract, got: {msg}"
        );
        assert!(
            msg.contains('o'),
            "the banner must offer o to open the folder, got: {msg}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unplayable_dir_names_the_extensions_it_found() {
        let dir = scratch_dir("found-exts");
        std::fs::write(dir.join("readme.nfo"), b"nfo").unwrap();
        std::fs::write(dir.join("disc.iso"), b"iso").unwrap();
        let msg = unplayable_watch_banner(&dir);
        assert!(
            msg.contains("no playable video"),
            "non-video files must say there is nothing to watch, got: {msg}"
        );
        assert!(msg.contains(".iso"), "must name .iso, got: {msg}");
        assert!(msg.contains(".nfo"), "must name .nfo, got: {msg}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sidecar_picks_matching_stem_next_to_the_video() {
        let video = Path::new("Show/Episode 01.mkv");
        let files = [
            Path::new("Show/Episode 01.srt"),
            Path::new("Show/Episode 02.srt"),
            Path::new("Show/subs/Episode 01.ass"),
        ];
        let found = sidecar_for(video, files).expect("sidecar");
        assert_eq!(found, Path::new("Show/Episode 01.srt"));
    }

    #[test]
    fn sidecar_prefers_exact_stem_over_tagged_language() {
        let video = Path::new("movie.mkv");
        let files = [Path::new("movie.en.srt"), Path::new("movie.srt")];
        let found = sidecar_for(video, files).expect("sidecar");
        assert_eq!(found, Path::new("movie.srt"));
    }

    #[test]
    fn sidecar_accepts_a_lone_unmatched_name_in_the_same_folder() {
        let video = Path::new("movie.mkv");
        let files = [Path::new("English.ass")];
        let found = sidecar_for(video, files).expect("sidecar");
        assert_eq!(found, Path::new("English.ass"));
    }

    #[test]
    fn sidecar_ignores_subs_in_a_nested_folder() {
        let video = Path::new("Show/ep.mkv");
        let files = [Path::new("Show/subs/ep.srt")];
        assert!(sidecar_for(video, files).is_none());
    }

    #[test]
    fn sidecar_beside_finds_srt_on_disk() {
        let dir = scratch_dir("sidecar-disk");
        let video = dir.join("anime.mkv");
        std::fs::write(&video, vec![0u8; 20]).unwrap();
        std::fs::write(dir.join("anime.srt"), b"1\n").unwrap();
        let found = sidecar_beside(&video).expect("disk sidecar");
        assert_eq!(found.file_name().unwrap(), "anime.srt");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mpv_gets_equals_form_vlc_gets_two_args_others_get_none() {
        let sub = Path::new(r"C:\dl\file.srt");
        assert_eq!(
            sub_file_args("mpv", sub),
            vec![format!("--sub-file={}", sub.display())]
        );
        assert_eq!(
            sub_file_args(r"C:\Program Files\mpv\mpv.exe", sub),
            vec![format!("--sub-file={}", sub.display())]
        );
        assert_eq!(
            sub_file_args("vlc", sub),
            vec!["--sub-file".into(), sub.display().to_string()]
        );
        let wmplayer = r"C:\Program Files\Windows Media Player\wmplayer.exe";
        assert!(sub_file_args(wmplayer, sub).is_empty());
    }

    #[test]
    fn resolve_sidecar_joins_the_download_dir() {
        let dir = scratch_dir("sidecar-join");
        let video_dir = dir.join("Show");
        std::fs::create_dir_all(&video_dir).unwrap();
        std::fs::write(video_dir.join("ep.srt"), b"1\n").unwrap();
        let found = resolve_sidecar(&dir, Some("Show/ep.mkv"), &["Show/ep.srt".into()]);
        assert_eq!(found, Some(dir.join("Show/ep.srt")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_sidecar_ignores_a_metadata_name_with_no_file() {
        let dir = scratch_dir("sidecar-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let found = resolve_sidecar(&dir, Some("Show/ep.mkv"), &["Show/ep.srt".into()]);
        assert!(found.is_none(), "a name is not a downloaded sidecar");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_unmatched_sidecars_are_not_guessed() {
        let video = Path::new("movie.mkv");
        let files = [Path::new("English.srt"), Path::new("Signs.ass")];
        assert!(sidecar_for(video, files).is_none());
    }

    /// End-to-end server test: serve a temp file, issue a Range request over
    /// a real socket, assert the 206 slice bytes.
    #[test]
    fn server_serves_ranges() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("harbour-stream-{nanos}.mp4"));
        let content: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &content).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let f = path.clone();
        let s = Arc::clone(&stop);
        thread::spawn(move || {
            let _ = accept_loop(listener, f, s);
        });

        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            client,
            "GET /stream HTTP/1.1\r\nHost: 127.0.0.1\r\nRange: bytes=10-19\r\n\r\n"
        )
        .unwrap();
        client.flush().unwrap();
        let mut resp = Vec::new();
        client.read_to_end(&mut resp).unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(
            text.starts_with("HTTP/1.1 206"),
            "got: {}",
            text.lines().next().unwrap()
        );
        assert!(text.contains("Content-Range: bytes 10-19/1024"));
        // Body is the 10 requested bytes (headers end at the first \r\n\r\n).
        let body = &resp[resp.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4..];
        assert_eq!(body, &content[10..20], "range slice must match");

        stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&path);
    }
}
