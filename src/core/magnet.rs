//! Infohash normalization and the single magnet builder.
//!
//! Everything downstream — queue, ledger, cache, engine — consumes
//! [`build_magnet`] rather than formatting its own string, so the URL is
//! identical everywhere and the infohash is lowercased exactly once, at this
//! boundary. That is what makes `info_hash` a safe join key across sources.

use crate::core::types::InfoHash;

/// A BitTorrent v1 infohash is 40 hex characters.
const INFO_HASH_LEN: usize = 40;

/// Normalizes a 40-hex infohash to lowercase, or `None` if it is not one.
///
/// Accepts uppercase because plenty of sources emit it (`FR-05`); rejects
/// anything else rather than guessing, since a malformed id would become a
/// malformed cache path.
pub fn normalize_info_hash(raw: &str) -> Option<InfoHash> {
    let s = raw.trim();
    if s.len() != INFO_HASH_LEN || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(s.to_ascii_lowercase())
}

/// True when `raw` is a bare infohash.
pub fn is_info_hash(raw: &str) -> bool {
    normalize_info_hash(raw).is_some()
}

/// Extracts and normalizes the infohash from any text containing
/// `xt=urn:btih:<hash>` — a magnet URI, or an HTML blob a scraper is mining.
pub fn info_hash_from_magnet(text: &str) -> Option<InfoHash> {
    // Case-insensitive search without pulling in a regex crate: lowercase a copy
    // for locating the marker, then slice the original at the same offset.
    let lowered = text.to_ascii_lowercase();
    let start = lowered.find("xt=urn:btih:")? + "xt=urn:btih:".len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    normalize_info_hash(&rest[..end])
}

/// Percent-encodes a display name for a magnet's `dn` parameter.
///
/// Everything outside unreserved ASCII is encoded, so `&`, `=`, spaces and
/// non-ASCII titles cannot terminate the parameter early or corrupt the URL —
/// a real bug class in the reference product.
fn encode_dn(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The single source of truth for magnet construction.
///
/// Trackers are deliberately omitted: the engine appends its own at add time,
/// and a stable magnet string is what lets the cache and the ledger compare
/// them. `info_hash` is lowercased here so callers never have to remember to.
pub fn build_magnet(info_hash: &str, name: &str) -> String {
    format!(
        "magnet:?xt=urn:btih:{}&dn={}",
        info_hash.to_ascii_lowercase(),
        encode_dn(name)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn normalizes_case_and_trims() {
        assert_eq!(normalize_info_hash(HASH).as_deref(), Some(HASH));
        assert_eq!(
            normalize_info_hash(&HASH.to_ascii_uppercase()).as_deref(),
            Some(HASH),
            "uppercase is accepted and canonicalized"
        );
        assert_eq!(
            normalize_info_hash(&format!("  {HASH}  ")).as_deref(),
            Some(HASH)
        );
    }

    #[test]
    fn rejects_anything_that_is_not_a_v1_infohash() {
        assert!(normalize_info_hash("").is_none());
        assert!(normalize_info_hash(&HASH[..39]).is_none(), "too short");
        assert!(
            normalize_info_hash(&format!("{HASH}0")).is_none(),
            "too long"
        );
        assert!(
            normalize_info_hash(&HASH.replace('0', "z")).is_none(),
            "non-hex must not become a path component"
        );
        // A base32 v1 hash (32 chars) is not something we can use as an id.
        assert!(normalize_info_hash("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567").is_none());
    }

    #[test]
    fn extracts_the_hash_from_a_magnet_and_from_surrounding_html() {
        let magnet = format!("magnet:?xt=urn:btih:{HASH}&dn=Some+Name&tr=udp://x");
        assert_eq!(info_hash_from_magnet(&magnet).as_deref(), Some(HASH));

        let html = format!(
            r#"<a href="magnet:?xt=urn:btih:{}&amp;dn=x">get</a>"#,
            HASH.to_uppercase()
        );
        assert_eq!(
            info_hash_from_magnet(&html).as_deref(),
            Some(HASH),
            "uppercase inside markup still normalizes"
        );

        assert!(info_hash_from_magnet("no magnet here").is_none());
        assert!(
            info_hash_from_magnet("xt=urn:btih:tooshort").is_none(),
            "a truncated hash is not silently accepted"
        );
    }

    #[test]
    fn builds_a_stable_lowercase_magnet() {
        let m = build_magnet(&HASH.to_uppercase(), "Some Movie");
        assert!(m.starts_with(&format!("magnet:?xt=urn:btih:{HASH}")));
        assert!(m.ends_with("&dn=Some%20Movie"));
        assert!(!m.contains("&tr="), "trackers are the engine's job");
    }

    #[test]
    fn dn_encoding_cannot_corrupt_the_url() {
        // The reference product's bug: an unescaped `&` in a title truncating
        // the magnet. Also covers `=`, and non-ASCII titles from the anime feeds.
        let m = build_magnet(HASH, "Tom & Jerry = Fun");
        let dn = m.split("&dn=").nth(1).expect("dn parameter");
        assert!(!dn.contains('&'), "ampersand must not survive unencoded");
        assert!(!dn.contains('='), "equals must not survive unencoded");

        let jp = build_magnet(HASH, "進撃の巨人");
        let dn = jp.split("&dn=").nth(1).expect("dn parameter");
        assert!(dn.is_ascii(), "non-ASCII must be percent-encoded");
        assert!(dn.starts_with('%'));
    }

    #[test]
    fn a_built_magnet_parses_back_to_the_same_hash() {
        let m = build_magnet(HASH, "Round Trip");
        assert_eq!(info_hash_from_magnet(&m).as_deref(), Some(HASH));
    }

    #[test]
    fn is_info_hash_agrees_with_normalize() {
        assert!(is_info_hash(HASH));
        assert!(!is_info_hash("magnet:?xt=urn:btih:0123"));
    }
}
