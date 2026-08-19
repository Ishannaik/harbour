//! Friend crash reports (FR-09a) to Sentry project `harbour`.
//!
//! The DSN is public by design. Friends do not set env vars.
//! `HARBOUR_SENTRY=0` opts out. Tests never call [`init`].
//!
//! Events are panics and process-level failures only. Search queries, magnets,
//! and download paths are stripped in [`scrub_event`].

use std::borrow::Cow;
use std::env;
use std::sync::Arc;

use sentry::protocol::Event;
use sentry::{ClientInitGuard, ClientOptions, Level};

use crate::core::paths::{ENV_SENTRY, ENV_SENTRY_DSN, ENV_SENTRY_ENV};

/// Public DSN for org `ishan-rt`, project `harbour`. Not a secret.
const DEFAULT_DSN: &str = "https://7ee8959645b1088d397de02881a9f945@o4508130310946816.ingest.de.sentry.io/4511935607210064";

const APP_TAG: &str = "harbour";

/// True when the user asked us not to phone home.
pub fn opted_out(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::trim)
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("0") | Some("false") | Some("off") | Some("no")
    )
}

/// DSN string: override env, else the baked friend DSN.
pub fn resolve_dsn(override_dsn: Option<&str>) -> &str {
    match override_dsn {
        Some(s) if !s.trim().is_empty() => s.trim(),
        _ => DEFAULT_DSN,
    }
}

fn environment(raw: Option<&str>) -> Cow<'static, str> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => Cow::Owned(s.to_string()),
        None => Cow::Borrowed("friends"),
    }
}

fn looks_private(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("magnet:")
        || lower.contains("xt=urn:btih")
        || lower.contains("info_hash")
        || lower.contains("/users/")
        || lower.contains("\\users\\")
        || lower.contains("query=")
}

/// Drop anything that could be a magnet, query, or home-dir path.
pub fn scrub_event(mut event: Event<'static>) -> Option<Event<'static>> {
    event.user = None;
    event.request = None;
    event.server_name = None;
    event.extra.retain(|k, v| {
        if looks_private(k) {
            return false;
        }
        !v.to_string().contains("magnet:") && !looks_private(&v.to_string())
    });
    if let Some(msg) = event.message.as_ref() {
        if looks_private(msg) {
            event.message = Some("redacted".into());
        }
    }
    Some(event)
}

/// Start the client. `None` when opted out or the DSN cannot be parsed.
///
/// Keep the guard alive until process exit so the last panic flushes.
pub fn init() -> Option<ClientInitGuard> {
    if opted_out(env::var(ENV_SENTRY).ok().as_deref()) {
        return None;
    }
    let override_dsn = env::var(ENV_SENTRY_DSN).ok();
    let dsn = resolve_dsn(override_dsn.as_deref());
    let parsed = dsn.parse().ok()?;
    let mut opts = ClientOptions::new();
    opts.dsn = Some(parsed);
    opts.release = sentry::release_name!();
    opts.environment = Some(environment(env::var(ENV_SENTRY_ENV).ok().as_deref()));
    opts.send_default_pii = false;
    opts.attach_stacktrace = true;
    opts.before_send = Some(Arc::new(|event| scrub_event(event)));
    let guard = sentry::init(opts);
    sentry::configure_scope(|scope| {
        scope.set_tag("app", APP_TAG);
        scope.set_tag("component", "tui");
    });
    Some(guard)
}

/// Process-level failure that is not a panic (bind/engine boot).
pub fn capture_fatal(message: &str) {
    let safe = if looks_private(message) {
        "process failed (redacted)"
    } else {
        message
    };
    sentry::capture_message(safe, Level::Fatal);
    let _ = sentry::Hub::current().client().map(|c| c.flush(None));
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentry::protocol::Map;
    use serde_json::json;

    #[test]
    fn opt_out_spellings() {
        assert!(opted_out(Some("0")));
        assert!(opted_out(Some("OFF")));
        assert!(opted_out(Some(" false ")));
        assert!(!opted_out(None));
        assert!(!opted_out(Some("1")));
    }

    #[test]
    fn empty_override_falls_back_to_baked_dsn() {
        assert_eq!(resolve_dsn(None), DEFAULT_DSN);
        assert_eq!(resolve_dsn(Some("")), DEFAULT_DSN);
        assert!(resolve_dsn(Some("https://x@o1.ingest.sentry.io/2")).contains("o1"));
    }

    #[test]
    fn environment_defaults_to_friends() {
        assert_eq!(environment(None), "friends");
        assert_eq!(environment(Some("  ")), "friends");
        assert_eq!(environment(Some("dev")), "dev");
    }

    #[test]
    fn scrub_drops_magnet_and_user_home() {
        let mut event = Event::new();
        event.user = Some(Default::default());
        event.message = Some("magnet:?xt=urn:btih:abc".into());
        event.extra = Map::from_iter([("path".into(), json!("C:\\Users\\friend\\Downloads\\x"))]);
        let out = scrub_event(event).expect("kept");
        assert!(out.user.is_none());
        assert_eq!(out.message.as_deref(), Some("redacted"));
        assert!(out.extra.is_empty());
    }
}
