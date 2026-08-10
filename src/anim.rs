//! Animation primitives for the harbour TUI: frame pacing, eased
//! interpolation, spinner cadence, and DEC 2026 synchronized output.
//!
//! Spec: docs/design.md §3 (Animation spec). Everything here is small and
//! terminal-free so the fixed-tick tests from §9 can drive it
//! deterministically: instants are passed in (`Spinner::advance`) and
//! durations are explicit (`EasedValue::update`, `eased`), never read from a
//! hidden clock.
//!
//! `dead_code` is allowed module-wide: `Ticker::elapsed`, `EasedValue`, and
//! `eased` are staged API — consumed by the download-progress bars and status
//! line in slice 2+. Remove the allow as views land.

#![allow(dead_code)]

use std::io;
use std::time::{Duration, Instant};

use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};

/// Paces a render loop to a fixed frame rate (30fps base cadence, spec §3).
///
/// `next()` returns how long the caller should sleep so frames fire on a
/// stable grid anchored at construction; `elapsed()` reports the wall time
/// since the previous tick, which the app feeds to [`EasedValue::update`] as
/// `dt`.
pub struct Ticker {
    /// One frame period: `1 / fps`.
    interval: Duration,
    /// The frame boundary currently being slept toward (or, after a resync,
    /// the moment the last `next()` was called). Initialized at construction
    /// so the first `next()` behaves like every other call.
    last: Instant,
}

impl Ticker {
    /// Creates a ticker pacing at `fps` frames per second.
    ///
    /// `fps` is clamped to at least 1 — a zero or absurd rate would produce a
    /// zero interval and turn the loop into a busy-wait.
    pub fn new(fps: u32) -> Ticker {
        let fps = fps.max(1);
        let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
        Ticker {
            interval,
            last: Instant::now(),
        }
    }

    /// Duration to sleep until the next frame boundary.
    ///
    /// The boundary is the previous boundary plus one interval, not "now"
    /// plus one interval, so variable frame cost cannot accumulate drift or
    /// double the cadence. When the loop is already past the boundary (a slow
    /// frame), this returns zero and resyncs the phase to now — the slow
    /// frame delays the next tick by one full interval (spec §3: "a slow
    /// frame simply delays the next tick"; frames are never queued).
    pub fn next(&mut self) -> Duration {
        let now = Instant::now();
        let boundary = self.last + self.interval;
        let sleep = boundary.saturating_duration_since(now);
        self.last = if now >= boundary { now } else { boundary };
        sleep
    }

    /// Wall-clock time since the previous tick cycle began.
    ///
    /// `last` tracks the boundary being slept toward, so subtracting one
    /// interval yields the boundary the previous tick fired on; the result is
    /// the true inter-frame period (≈ 1/fps), which is the `dt` eased values
    /// should be advanced by. Call it at the top of the frame loop, before
    /// `next()`.
    pub fn elapsed(&self) -> Duration {
        // Saturating on both sides: `last` predates the first boundary (the
        // `checked_sub` fallback keeps `last` when we are still inside the
        // first interval), and the caller may ask before the boundary has
        // been reached.
        Instant::now()
            .saturating_duration_since(self.last.checked_sub(self.interval).unwrap_or(self.last))
    }
}

/// Exponentially-smoothed value that eases toward a target and never
/// overshoots it (spec §3: "progress values never jump").
///
/// `current` moves toward `target` by a blend factor of
/// `1 - exp(-dt / tau)` per [`update`](EasedValue::update), so the step size
/// shrinks as the value approaches the target — a display bar eases in and
/// settles instead of snapping. The app uses a `tau` of 200ms.
pub struct EasedValue {
    /// The smoothed (displayed) value.
    current: f64,
    /// The value `current` converges toward.
    target: f64,
    /// Smoothing time constant; larger values ease more slowly.
    tau: Duration,
}

impl EasedValue {
    /// Starts at `initial` with the given smoothing time constant.
    ///
    /// A zero `tau` is allowed and makes `update` snap straight to the
    /// target — convenient for "instant" displays that share the eased API.
    pub fn new(initial: f64, tau: Duration) -> EasedValue {
        EasedValue {
            current: initial,
            target: initial,
            tau,
        }
    }

    /// Retargets the value; `update` eases from wherever `current` happens
    /// to be, so mid-flight target changes never cause a jump.
    pub fn set_target(&mut self, v: f64) {
        self.target = v;
    }

    /// Advances the filter by `dt` (typically one frame period).
    ///
    /// `current += (target - current) * (1 - exp(-dt / tau))`. The blend
    /// factor is always in `[0, 1)` for a positive tau, so each step moves
    /// strictly toward `target` — never past it. The design doc has the
    /// caller clamp `dt` to `[0, 1]`; this primitive deliberately does not,
    /// keeping a single obvious formula.
    pub fn update(&mut self, dt: Duration) {
        self.current = eased(self.current, self.target, dt, self.tau);
    }

    /// The current smoothed value.
    pub fn value(&self) -> f64 {
        self.current
    }
}

/// One exponential-smoothing step: eases `current` toward `target` over `dt`.
///
/// `tau` is the filter time constant (200ms in the app). The blend factor
/// `1 - exp(-dt / tau)` lies in `[0, 1)`, so the result never overshoots the
/// target and converges as `dt` accumulates.
pub fn eased(current: f64, target: f64, dt: Duration, tau: Duration) -> f64 {
    // A zero tau would be a division by zero; treat it as "instant snap"
    // rather than producing NaN.
    if tau.is_zero() {
        return target;
    }
    let blend = 1.0 - (-dt.as_secs_f64() / tau.as_secs_f64()).exp();
    current + (target - current) * blend
}

/// A rotating glyph set that advances on a fixed cadence.
///
/// Status-line spinners advance every 80ms (≈12.5fps), activity spinners
/// every frame — the cadence is chosen by the caller via `interval`, and
/// `advance` simply gates on it (spec §3).
pub struct Spinner {
    /// Invariant: never empty — enforced in [`Spinner::new`]; theme
    /// validation already rejects empty `spinnerFrames` before one is built.
    frames: Vec<String>,
    /// Index of the frame currently shown.
    idx: usize,
    /// When the current frame was chosen; advancing is gated on this.
    last_advance: Instant,
}

impl Spinner {
    /// Creates a spinner from the given frames.
    ///
    /// # Panics
    ///
    /// Panics if `frames` is empty: a spinner with nothing to spin is a
    /// programming error and would make [`Spinner::current`] index out of
    /// bounds.
    pub fn new(frames: Vec<String>) -> Spinner {
        assert!(
            !frames.is_empty(),
            "Spinner::new requires at least one frame"
        );
        Spinner {
            frames,
            idx: 0,
            last_advance: Instant::now(),
        }
    }

    /// Advances to the next frame if at least `interval` has elapsed since
    /// the last advance, wrapping around at the end of the set.
    ///
    /// Call once per rendered frame; `interval` (80ms for status spinners,
    /// one frame period for activity spinners) picks the cadence. `now` is
    /// passed in so fixed-tick tests can control time.
    pub fn advance(&mut self, now: Instant, interval: Duration) {
        if now.saturating_duration_since(self.last_advance) >= interval {
            self.idx = (self.idx + 1) % self.frames.len();
            self.last_advance = now;
        }
    }

    /// The currently displayed frame.
    pub fn current(&self) -> &str {
        // Safe: `idx` is always < `frames.len()` (wrapped mod len, and the
        // non-empty invariant is enforced in `new`).
        &self.frames[self.idx]
    }
}

/// Runs `f` bracketed by DEC 2026 synchronized-update commands.
///
/// The terminal defers painting until the matching End command, so a frame
/// written inside `f` appears atomically — no partial paints, no flicker
/// (spec §3 "Sync output"). The End command is emitted even when `f` fails,
/// so an errored frame cannot leave the terminal wedged in synchronized
/// mode. `f` returns an I/O result (the same type `execute!` and ratatui's
/// `Terminal::draw` produce), which is propagated unchanged.
pub fn with_sync_output<F: FnOnce() -> io::Result<()>>(f: F) -> io::Result<()> {
    let mut stdout = io::stdout();
    with_sync_output_on(&mut stdout, f)
}

/// [`with_sync_output`] against an arbitrary writer, so tests can capture
/// the exact bytes instead of talking to the real stdout.
fn with_sync_output_on<W: io::Write, F: FnOnce() -> io::Result<()>>(
    writer: &mut W,
    f: F,
) -> io::Result<()> {
    crossterm::execute!(writer, BeginSynchronizedUpdate)?;
    let result = f();
    // Always close the bracket, even if f failed, or the terminal stays in
    // synchronized-update mode and every later frame flickers.
    crossterm::execute!(writer, EndSynchronizedUpdate)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAU: Duration = Duration::from_millis(200);
    const FRAME: Duration = Duration::from_millis(33); // one 30fps frame

    // --- Ticker ---

    #[test]
    fn ticker_returns_interval_aligned_waits() {
        let interval = Duration::from_secs_f64(1.0 / 30.0);
        let mut ticker = Ticker::new(30);
        let slack = Duration::from_millis(2);

        // No sleeps: this tests the grid MATH, which is deterministic on
        // every OS. Real sleeps are what flaked macOS CI (a loaded runner
        // overslept a 33ms frame to 62ms and pushed the phase past the next
        // boundary). The invariant to defend is "waits land on the anchored
        // grid", not "sleep() returns on time".
        let first = ticker.next();
        assert!(
            (first.as_secs_f64() - interval.as_secs_f64()).abs() < slack.as_secs_f64(),
            "first wait {first:?} vs interval {interval:?}"
        );

        // Second boundary is two intervals from construction.
        let second = ticker.next();
        let two = interval * 2;
        assert!(
            (second.as_secs_f64() - two.as_secs_f64()).abs() < slack.as_secs_f64(),
            "second wait {second:?} vs {two:?}"
        );

        // Third boundary: three intervals. Waits grow monotonically while no
        // real time passes — that IS the phase grid (a slow frame resyncs
        // via the `now >= boundary` path, covered by the elapsed floor below).
        let third = ticker.next();
        let three = interval * 3;
        assert!(
            (third.as_secs_f64() - three.as_secs_f64()).abs() < slack.as_secs_f64(),
            "third wait {third:?} vs {three:?}"
        );

        // elapsed() never reports more than the paced period when nothing
        // real has elapsed (no negative/zero dt surprises for eased values).
        let dt = ticker.elapsed();
        assert!(
            dt < interval + slack,
            "elapsed {dt:?} should stay under one interval with no sleep"
        );
    }

    #[test]
    fn ticker_clamps_fps_to_at_least_one() {
        // fps = 0 must not produce a zero interval (busy-wait); it is
        // clamped to a 1s cadence. The bound is deliberately loose so a
        // multi-ms stall between `new` and `next` cannot flake the test.
        let mut ticker = Ticker::new(0);
        assert!(ticker.next() >= Duration::from_millis(950));
    }

    // --- EasedValue / eased ---

    #[test]
    fn eased_value_converges_without_overshoot() {
        let mut e = EasedValue::new(0.0, TAU);
        e.set_target(1.0);

        // ~10s of 30fps frames: geometrically converges to the target.
        for _ in 0..300 {
            e.update(FRAME);
        }
        assert!((e.value() - 1.0).abs() < 1e-6, "value {}", e.value());

        // Monotone approach: every step moves strictly toward the target and
        // never past it.
        let mut e = EasedValue::new(0.0, TAU);
        e.set_target(1.0);
        let mut prev = e.value();
        for _ in 0..200 {
            e.update(FRAME);
            let v = e.value();
            assert!(v >= prev && v <= 1.0, "overshoot: {prev} -> {v}");
            prev = v;
        }
    }

    #[test]
    fn eased_value_retargets_mid_flight() {
        let mut e = EasedValue::new(0.0, TAU);
        e.set_target(1.0);
        for _ in 0..20 {
            e.update(FRAME);
        }
        let mid = e.value();
        assert!(mid > 0.1 && mid < 1.0, "should be partway, got {mid}");

        // Retargeting mid-flight eases from the current value, no jump.
        e.set_target(0.0);
        for _ in 0..300 {
            e.update(FRAME);
        }
        assert!(e.value().abs() < 1e-6, "left target, got {}", e.value());

        // And the way down never overshoots below zero.
        let mut e = EasedValue::new(0.0, TAU);
        e.set_target(1.0);
        for _ in 0..20 {
            e.update(FRAME);
        }
        e.set_target(0.0);
        let mut prev = e.value();
        for _ in 0..100 {
            e.update(FRAME);
            let v = e.value();
            assert!(v <= prev && v >= 0.0, "overshoot down: {prev} -> {v}");
            prev = v;
        }
    }

    #[test]
    fn eased_never_overshoots_and_handles_edge_durations() {
        // One step moves strictly toward the target, never past it.
        let up = eased(0.0, 1.0, FRAME, TAU);
        assert!(up > 0.0 && up < 1.0, "up {up}");
        let down = eased(1.0, 0.0, FRAME, TAU);
        assert!(down > 0.0 && down < 1.0, "down {down}");

        // Zero dt: no movement at all (blend factor is exactly 0).
        assert_eq!(eased(0.5, 1.0, Duration::ZERO, TAU), 0.5);

        // Zero tau: instant snap to the target, not a division-by-zero NaN.
        assert_eq!(eased(0.5, 1.0, FRAME, Duration::ZERO), 1.0);

        // Long dt converges to within epsilon of the target.
        let v = eased(0.0, 1.0, Duration::from_secs(5), TAU);
        assert!((v - 1.0).abs() < 1e-6, "long dt {v}");
    }

    // --- Spinner ---

    #[test]
    fn spinner_advances_only_after_interval() {
        let mut s = Spinner::new(vec!["⠋".into(), "⠙".into(), "⠹".into()]);
        assert_eq!(s.current(), "⠋");

        // t0 is captured after construction so the interval gate is measured
        // from construction, not from an earlier instant. Margins of 40ms
        // keep the test robust even if construction takes a few ms.
        let t0 = Instant::now();
        s.advance(t0, Duration::from_millis(80));
        assert_eq!(s.current(), "⠋", "no advance at t0");

        s.advance(t0 + Duration::from_millis(40), Duration::from_millis(80));
        assert_eq!(s.current(), "⠋", "40ms < 80ms");

        s.advance(t0 + Duration::from_millis(80), Duration::from_millis(80));
        assert_eq!(s.current(), "⠙", "first advance at the interval");

        s.advance(t0 + Duration::from_millis(80), Duration::from_millis(80));
        assert_eq!(s.current(), "⠙", "no time passed since last advance");

        s.advance(t0 + Duration::from_millis(120), Duration::from_millis(80));
        assert_eq!(s.current(), "⠙", "40ms since last advance");

        s.advance(t0 + Duration::from_millis(160), Duration::from_millis(80));
        assert_eq!(s.current(), "⠹", "second advance");

        s.advance(t0 + Duration::from_millis(240), Duration::from_millis(80));
        assert_eq!(s.current(), "⠋", "wraps around");
    }

    #[test]
    #[should_panic(expected = "at least one frame")]
    fn spinner_rejects_empty_frames() {
        let _ = Spinner::new(Vec::new());
    }

    // --- with_sync_output ---

    #[test]
    fn sync_output_runs_closure_and_propagates_result() {
        let mut ran = false;
        let ok = with_sync_output(|| {
            ran = true;
            Ok(())
        });
        assert!(ok.is_ok());
        assert!(ran, "closure must run between Begin and End");

        let err = with_sync_output(|| Err(io::Error::other("boom")));
        assert!(err.is_err(), "error must propagate");
    }

    #[test]
    fn sync_output_brackets_f_with_dec_2026_commands() {
        let mut out: Vec<u8> = Vec::new();
        let mut ran = false;
        let res = with_sync_output_on(&mut out, || {
            ran = true;
            Ok(())
        });
        assert!(res.is_ok());
        assert!(ran);

        // BeginSynchronizedUpdate -> ESC[?2026h, End -> ESC[?2026l; End must
        // come after Begin (commands are generic over Write, so a Vec<u8>
        // captures the exact bytes).
        let bytes = String::from_utf8(out).expect("sync commands are ASCII");
        let begin = bytes.find("\x1b[?2026h").expect("BeginSynchronizedUpdate");
        let end = bytes.find("\x1b[?2026l").expect("EndSynchronizedUpdate");
        assert!(begin < end, "End must be emitted after Begin");
    }

    #[test]
    fn sync_output_closes_bracket_even_when_f_errors() {
        let mut out: Vec<u8> = Vec::new();
        let res = with_sync_output_on(&mut out, || Err(io::Error::other("boom")));
        assert!(res.is_err());
        // The End command must still be written, or the terminal stays
        // wedged in synchronized-update mode.
        assert!(
            String::from_utf8(out).unwrap().contains("\x1b[?2026l"),
            "End must be emitted even on failure"
        );
    }
}
