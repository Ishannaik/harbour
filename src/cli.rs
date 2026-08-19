//! Command-line parsing (`FR-02`…`FR-05`).
//!
//! Hand-rolled rather than `clap`: the whole surface is three flags and one
//! positional argument, and `docs/architecture.md` §2 says to reach for clap
//! only if it grows. Parsing is a pure function of `argv` so every case is a
//! unit test rather than a manual run.
//!
//! `dead_code` is allowed until the initial-download action is wired to the
//! queue (E2); parsing itself is reached from `main` and fully tested here.

#![allow(dead_code)]

use std::path::PathBuf;

use crate::core::magnet::{is_info_hash, normalize_info_hash};

/// What the user asked us to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Open the TUI.
    Run,
    /// Open the TUI and immediately enqueue this magnet.
    RunWithMagnet(String),
    /// Open the TUI and immediately enqueue this `.torrent` file.
    RunWithTorrent(PathBuf),
    Help,
    Version,
    /// Bad input: print `message` to stderr and exit non-zero **without**
    /// starting the TUI (`FR-02`).
    Invalid {
        message: String,
    },
}

pub const HELP: &str = "\
harbour — curated torrents straight from your terminal

usage
  harbour                       open the search TUI
  harbour \"magnet:?xt=...\"      start a download on launch
  harbour <40-hex infohash>     same, from a bare infohash
  harbour path/to/file.torrent  open a .torrent file on launch
  harbour --help                print this and exit
  harbour --version             print the version and exit

keys
  enter  search (empty query browses the curated top lists)
  d      download to the default folder
  D      download to a folder you pick
  o      change the default download folder
  ← →    switch between the downloads and seeding tabs
  p      pause / resume seeding
  ?      show all keybinds
  q      quit

environment
  HARBOUR_MAX_DOWNLOADS   max concurrent downloads (0 or unset = unlimited)
  HARBOUR_SOURCE_TIMEOUT  per-source deadline in seconds (default 10)
  HARBOUR_STATE_DIR       relocate config, ledger and cache (testing/portable)
  HARBOUR_SENTRY          0/off disables friend crash reports (Sentry)
  HARBOUR_SENTRY_DSN      override the baked Sentry DSN
  HARBOUR_SENTRY_ENV      Sentry environment tag (default friends)

tip: quote magnet links — they contain & characters your shell will eat.
";

/// Parses `argv` (without the program name).
pub fn parse(args: &[String]) -> Command {
    let args: Vec<&str> = args
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .collect();

    let Some(first) = args.first().copied() else {
        return Command::Run;
    };

    match first {
        "--help" | "-h" => return Command::Help,
        "--version" | "-V" | "-v" => return Command::Version,
        _ => {}
    }

    if args.len() > 1 {
        return Command::Invalid {
            message: format!(
                "expected one argument, got {}. If this is a magnet link, quote it — \
                 your shell split it on the & characters.",
                args.len()
            ),
        };
    }

    if first.starts_with('-') {
        return Command::Invalid {
            message: format!("unknown option '{first}'"),
        };
    }

    // `get(..8)` rather than `[..8]`: `len()` is bytes, and slicing at byte 8
    // panics on a non-ASCII first argument (e.g. a Japanese title) whose char
    // boundary lands elsewhere — FR-02 wants a usage error, not a crash.
    if first
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("magnet:?"))
    {
        // Validate rather than trust: an unusable magnet should fail here with
        // a clear message, not three layers down inside the engine.
        return match crate::core::magnet::info_hash_from_magnet(first) {
            Some(_) => Command::RunWithMagnet(first.to_owned()),
            None => Command::Invalid {
                message: "that magnet link has no usable 40-hex infohash (xt=urn:btih:...)".into(),
            },
        };
    }

    if is_info_hash(first) {
        // The guard above already shaped the string; still handle failure
        // honestly — a malformed infohash is user input, not a crash.
        return match normalize_info_hash(first) {
            Some(hash) => Command::RunWithMagnet(crate::core::magnet::build_magnet(&hash, &hash)),
            None => Command::Invalid {
                message: "that doesn't look like a usable 40-hex infohash".into(),
            },
        };
    }

    if first.to_ascii_lowercase().ends_with(".torrent") {
        return Command::RunWithTorrent(PathBuf::from(first));
    }

    Command::Invalid {
        message: format!(
            "'{first}' is not a magnet link, a 40-character infohash, or a .torrent file"
        ),
    }
}

/// `harbour <semver>` (`FR-04`).
pub fn version_line() -> String {
    format!("harbour {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    fn parse_str(args: &[&str]) -> Command {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn no_arguments_opens_the_tui() {
        assert_eq!(parse_str(&[]), Command::Run);
        // Shells and test harnesses both hand us blanks; they are not an error.
        assert_eq!(parse_str(&["", "   "]), Command::Run);
    }

    #[test]
    fn help_and_version_are_recognised_in_their_usual_spellings() {
        for flag in ["--help", "-h"] {
            assert_eq!(parse_str(&[flag]), Command::Help, "{flag}");
        }
        for flag in ["--version", "-V", "-v"] {
            assert_eq!(parse_str(&[flag]), Command::Version, "{flag}");
        }
    }

    #[test]
    fn version_line_matches_the_crate_version() {
        assert_eq!(
            version_line(),
            format!("harbour {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn help_text_documents_every_environment_knob() {
        // If a knob is added without a line here, users have no way to find it.
        for var in [
            crate::core::paths::ENV_MAX_DOWNLOADS,
            crate::core::paths::ENV_SOURCE_TIMEOUT,
            crate::core::paths::ENV_STATE_DIR,
            crate::core::paths::ENV_SENTRY,
            crate::core::paths::ENV_SENTRY_DSN,
            crate::core::paths::ENV_SENTRY_ENV,
        ] {
            assert!(HELP.contains(var), "{var} is undocumented in --help");
        }
    }

    #[test]
    fn a_magnet_is_accepted_and_kept_verbatim() {
        let magnet = format!("magnet:?xt=urn:btih:{HASH}&dn=Example");
        assert_eq!(
            parse_str(&[&magnet]),
            Command::RunWithMagnet(magnet.clone()),
            "the original string is preserved, trackers and all"
        );
        // Some sources emit the scheme capitalised.
        let upper = format!("MAGNET:?xt=urn:btih:{HASH}");
        assert!(matches!(parse_str(&[&upper]), Command::RunWithMagnet(_)));
    }

    #[test]
    fn a_magnet_without_a_usable_infohash_is_rejected_with_a_reason() {
        let Command::Invalid { message } = parse_str(&["magnet:?dn=no+hash+here"]) else {
            panic!("expected rejection");
        };
        assert!(
            message.contains("infohash"),
            "the message must say what is wrong"
        );
    }

    #[test]
    fn a_bare_infohash_becomes_a_magnet() {
        let Command::RunWithMagnet(magnet) = parse_str(&[HASH]) else {
            panic!("expected a magnet");
        };
        assert_eq!(
            crate::core::magnet::info_hash_from_magnet(&magnet).as_deref(),
            Some(HASH)
        );

        // FR-05: uppercase is accepted and normalized.
        let Command::RunWithMagnet(magnet) = parse_str(&[&HASH.to_uppercase()]) else {
            panic!("expected a magnet");
        };
        assert!(magnet.contains(HASH), "normalized to lowercase");
    }

    #[test]
    fn a_torrent_path_is_accepted_case_insensitively() {
        assert_eq!(
            parse_str(&["movie.torrent"]),
            Command::RunWithTorrent(PathBuf::from("movie.torrent"))
        );
        assert_eq!(
            parse_str(&["C:\\downloads\\Movie.TORRENT"]),
            Command::RunWithTorrent(PathBuf::from("C:\\downloads\\Movie.TORRENT"))
        );
    }

    #[test]
    fn an_unsplit_magnet_gets_a_message_that_explains_the_real_problem() {
        // The common footgun: an unquoted magnet arrives as several arguments.
        let Command::Invalid { message } =
            parse_str(&["magnet:?xt=urn:btih:abc", "dn=Example", "tr=udp://x"])
        else {
            panic!("expected rejection");
        };
        assert!(
            message.contains("quote"),
            "tell the user to quote it rather than just refusing: {message}"
        );
    }

    #[test]
    fn unknown_options_and_junk_are_rejected_rather_than_ignored() {
        assert!(matches!(parse_str(&["--wat"]), Command::Invalid { .. }));
        assert!(matches!(
            parse_str(&["not-a-torrent"]),
            Command::Invalid { .. }
        ));
        // A near-miss infohash must not be silently treated as a search term.
        assert!(matches!(parse_str(&[&HASH[..39]]), Command::Invalid { .. }));
    }

    #[test]
    fn rejections_never_start_the_tui() {
        // The type system carries this guarantee: Invalid is not a Run variant.
        for bad in ["--wat", "nonsense", "magnet:?dn=x"] {
            assert!(
                matches!(parse_str(&[bad]), Command::Invalid { .. }),
                "{bad} must not reach the TUI"
            );
        }
    }
}
