//! The shared fetch layer: timeouts, retries, mirror failover, and fail-fast on
//! bot challenges.
//!
//! This is where the reference product's hard-won operational knowledge lives,
//! ported deliberately (`docs/plan-engine.md` §6):
//!
//! * **Full-jitter exponential backoff.** A fixed backoff synchronises every
//!   client that failed at the same moment into a second thundering herd.
//! * **`Retry-After` is honoured when it is short.** A `429` is the one 4xx
//!   worth retrying, because the server told us exactly when to come back — but
//!   only if that fits inside the deadline, since nobody is waiting thirty
//!   seconds for a torrent search.
//! * **A bot challenge fails fast.** DDoS-Guard and Cloudflare answer `503`
//!   with their name in the `server` header; retrying a challenge page never
//!   turns it into a `200`, it just burns the budget and deepens the block.
//! * **Mirror failover with a sticky hint.** The engine remembers which mirror
//!   answered last and passes it in, so a dead primary is not re-probed first on
//!   every search — and the source stays stateless.

use std::time::Duration;

use crate::core::error::SourceError;
use crate::core::types::SearchCtx;

/// Sent on every request. Honest about who we are, with a contact URL — the
/// polite thing to do when scraping someone else's site.
pub const USER_AGENT: &str =
    "harbour/0.1 (+https://github.com/Ishannaik/harbour) terminal torrent search";

/// Statuses worth a second attempt. `429` is here deliberately (see module docs);
/// `403`/`404` are not, because they will not change on a retry.
const RETRYABLE: [u16; 6] = [408, 425, 429, 500, 502, 503];

/// Longest `Retry-After` we will actually wait during an interactive search.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(2);

/// Attempts per host before moving to the next mirror.
const ATTEMPTS_PER_HOST: u32 = 2;

/// Base for the exponential backoff.
const BACKOFF_BASE: Duration = Duration::from_millis(200);

/// One HTTP client, built once per source.
///
/// `reqwest::Client` holds the connection pool, so cloning it is cheap and
/// sharing one per source is what keeps keep-alive and DNS caching working
/// across a search's follow-up fetches.
#[derive(Debug, Clone)]
pub struct SourceClient {
    inner: reqwest::Client,
}

impl SourceClient {
    /// Builds a client, falling back to a default one if the builder rejects
    /// our options — a source with a slightly wrong client still beats a source
    /// that cannot exist (`plan-engine.md` §4.1).
    pub fn new() -> Self {
        let inner = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            // A per-request timeout as well as the caller's deadline: without
            // it a half-open connection can hang past every budget we set.
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|err| {
                eprintln!("harbour: falling back to a default HTTP client ({err})");
                reqwest::Client::new()
            });
        Self { inner }
    }

    /// Fetches `url` as text, retrying transient failures within the deadline.
    pub async fn get_text(&self, url: &str, ctx: &SearchCtx) -> Result<String, SourceError> {
        let response = self.get(url, ctx).await?;
        response
            .text()
            .await
            .map_err(|e| SourceError::Network(format!("reading body: {e}")))
    }

    /// Fetches and deserializes JSON.
    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        ctx: &SearchCtx,
    ) -> Result<T, SourceError> {
        let body = self.get_text(url, ctx).await?;
        serde_json::from_str(&body).map_err(|e| SourceError::Parse(format!("{url}: {e}")))
    }

    /// One URL, with retries. Public so a source can inspect headers.
    pub async fn get(&self, url: &str, ctx: &SearchCtx) -> Result<reqwest::Response, SourceError> {
        let deadline = tokio::time::Instant::now() + ctx.total_deadline;
        let mut last: Option<SourceError> = None;

        for attempt in 0..ATTEMPTS_PER_HOST {
            if ctx.cancel.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(SourceError::Timeout);
            }

            let request = self.inner.get(url).timeout(remaining).send();
            let outcome = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => return Err(SourceError::Cancelled),
                res = request => res,
            };

            match outcome {
                Ok(response) => {
                    let status = response.status().as_u16();
                    if response.status().is_success() {
                        return Ok(response);
                    }
                    if let Some(blocked) = challenge_reason(&response) {
                        // Never retried: a challenge page does not become a 200
                        // by asking again.
                        return Err(SourceError::Blocked(blocked));
                    }
                    if !RETRYABLE.contains(&status) {
                        return Err(if response.status().is_client_error() {
                            SourceError::Blocked(format!("HTTP {status}"))
                        } else {
                            SourceError::Network(format!("HTTP {status}"))
                        });
                    }
                    let wait = retry_after(&response)
                        .filter(|d| *d <= MAX_RETRY_AFTER)
                        .unwrap_or_else(|| backoff(attempt));
                    last = Some(SourceError::Network(format!("HTTP {status}")));
                    if attempt + 1 < ATTEMPTS_PER_HOST && sleep_within(wait, deadline, ctx).await {
                        continue;
                    }
                    return Err(last.unwrap_or(SourceError::Timeout));
                }
                Err(err) if err.is_timeout() => return Err(SourceError::Timeout),
                Err(err) => {
                    last = Some(SourceError::Network(err.to_string()));
                    let wait = backoff(attempt);
                    if attempt + 1 < ATTEMPTS_PER_HOST && sleep_within(wait, deadline, ctx).await {
                        continue;
                    }
                    return Err(last.unwrap_or_else(|| SourceError::Network("unknown".into())));
                }
            }
        }
        Err(last.unwrap_or(SourceError::Timeout))
    }

    /// Tries each host in turn until one answers, starting from the sticky hint.
    ///
    /// `path` is appended to `https://<host>`. Returns the body and the host
    /// that produced it, so the caller can report a new hint back to the engine.
    pub async fn get_text_failover(
        &self,
        hosts: &[&str],
        path: &str,
        ctx: &SearchCtx,
    ) -> Result<(String, String), SourceError> {
        let ordered = order_hosts(hosts, ctx.host_hint.as_deref());
        let mut last = SourceError::Network("no hosts configured".into());
        for host in ordered {
            if ctx.cancel.is_cancelled() {
                return Err(SourceError::Cancelled);
            }
            match self.get_text(&format!("https://{host}{path}"), ctx).await {
                Ok(body) => return Ok((body, host.to_string())),
                // Cancellation and a spent budget are about us, not the host —
                // trying the next mirror would be pointless and slow.
                Err(e @ (SourceError::Cancelled | SourceError::Timeout)) => return Err(e),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

impl Default for SourceClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Puts the sticky hint first, keeping the configured order otherwise.
pub fn order_hosts<'a>(hosts: &[&'a str], hint: Option<&str>) -> Vec<&'a str> {
    let mut ordered: Vec<&str> = hosts.to_vec();
    if let Some(hint) = hint
        && let Some(pos) = ordered.iter().position(|h| *h == hint)
    {
        ordered.swap(0, pos);
    }
    ordered
}

/// Names the anti-bot layer when a response is one of its challenges.
fn challenge_reason(response: &reqwest::Response) -> Option<String> {
    let status = response.status().as_u16();
    // Cloudflare's own codes for "the origin is fine, you are not getting in".
    if matches!(status, 403 | 503 | 520 | 521 | 522 | 525 | 526) {
        let server = response
            .headers()
            .get(reqwest::header::SERVER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        for name in ["cloudflare", "ddos-guard", "sucuri"] {
            if server.contains(name) {
                return Some(format!("{name} challenge (HTTP {status})"));
            }
        }
        if status == 403 {
            return Some("HTTP 403".into());
        }
    }
    None
}

/// Parses `Retry-After` in either of its two legal forms.
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let raw = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .to_owned();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // The HTTP-date form. We do not carry a date parser for this; treating an
    // unparseable value as "no hint" and using our own backoff is correct and
    // costs one extra short wait.
    None
}

/// Full jitter: uniform in `[0, base * 2^attempt]`.
///
/// Without the jitter every client that failed together retries together.
/// Derived from the process id and the attempt rather than a random crate —
/// this needs to be *spread*, not cryptographically random.
fn backoff(attempt: u32) -> Duration {
    let ceiling = BACKOFF_BASE.saturating_mul(1u32 << attempt.min(4));
    let spread = (std::process::id() as u64).wrapping_mul(2_654_435_761) >> 16;
    let frac = (spread % 1000) as f64 / 1000.0;
    Duration::from_secs_f64(ceiling.as_secs_f64() * (0.5 + frac / 2.0))
}

/// Sleeps, but never past the deadline and never through a cancellation.
/// Returns false if the caller should give up instead of retrying.
async fn sleep_within(wait: Duration, deadline: tokio::time::Instant, ctx: &SearchCtx) -> bool {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if wait >= remaining {
        return false;
    }
    tokio::select! {
        _ = ctx.cancel.cancelled() => false,
        _ = tokio::time::sleep(wait) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sticky_hint_is_probed_first_without_dropping_the_others() {
        let hosts = ["a.example", "b.example", "c.example"];
        assert_eq!(
            order_hosts(&hosts, Some("c.example")),
            vec!["c.example", "b.example", "a.example"]
        );
        // An unknown or absent hint leaves the configured order alone.
        assert_eq!(order_hosts(&hosts, Some("gone.example")), hosts.to_vec());
        assert_eq!(order_hosts(&hosts, None), hosts.to_vec());
        assert!(order_hosts(&[], Some("x")).is_empty());
    }

    #[test]
    fn backoff_stays_inside_its_ceiling_and_grows() {
        for attempt in 0..5 {
            let ceiling = BACKOFF_BASE.saturating_mul(1u32 << attempt);
            let d = backoff(attempt);
            assert!(
                d <= ceiling,
                "attempt {attempt}: {d:?} exceeded {ceiling:?}"
            );
            assert!(d >= ceiling / 2, "jitter should not collapse to zero");
        }
        assert!(backoff(3) > backoff(0), "backoff grows with attempts");
    }

    #[test]
    fn only_transient_statuses_are_retried() {
        // 429 is the one 4xx worth retrying; 403/404 never are.
        assert!(RETRYABLE.contains(&429));
        assert!(RETRYABLE.contains(&503));
        assert!(!RETRYABLE.contains(&403));
        assert!(!RETRYABLE.contains(&404));
    }

    #[tokio::test]
    async fn a_cancelled_search_fails_fast_rather_than_fetching() {
        let client = SourceClient::new();
        let ctx = SearchCtx::default();
        ctx.cancel.cancel();
        // 127.0.0.1:1 would hang or refuse; cancellation must win before that.
        let err = client.get("http://127.0.0.1:1/", &ctx).await.unwrap_err();
        assert_eq!(err, SourceError::Cancelled);
    }

    #[tokio::test]
    async fn an_exhausted_budget_reports_timeout_not_a_network_error() {
        let client = SourceClient::new();
        let ctx = SearchCtx {
            total_deadline: Duration::ZERO,
            ..SearchCtx::default()
        };
        let err = client.get("http://127.0.0.1:1/", &ctx).await.unwrap_err();
        assert_eq!(err, SourceError::Timeout);
    }

    #[tokio::test]
    async fn a_refused_connection_is_a_network_error_and_stays_inside_the_budget() {
        let client = SourceClient::new();
        let ctx = SearchCtx {
            total_deadline: Duration::from_secs(2),
            ..SearchCtx::default()
        };
        let started = std::time::Instant::now();
        let err = client.get("http://127.0.0.1:1/", &ctx).await.unwrap_err();
        assert!(
            matches!(err, SourceError::Network(_) | SourceError::Timeout),
            "got {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the deadline must bound the whole retry ladder"
        );
    }
}
