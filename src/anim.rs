//! Animation cadence, frame budgeting, adaptive backpressure, spinner timing,
//! and eased progress.

use std::time::Duration;

/// Base frame budget for 30 frames per second (~33.33ms).
pub const BASE_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 30);

/// Cadence interval for status spinner advance (80ms, ~12.5fps).
pub const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

/// Public snapshot of rendering performance and adaptive backpressure state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameStats {
    /// Total number of rendered frames completed.
    pub frame_count: u64,
    /// Duration cost of the most recent frame draw.
    pub last_frame_cost: Duration,
    /// Current scheduled interval between frames (stretched if backpressure is active).
    pub current_interval: Duration,
    /// Baseline target budget (33.33ms for 30fps).
    pub base_budget: Duration,
    /// True when the previous frame exceeded budget and stretched the interval.
    pub backpressure_stretched: bool,
    /// Instantaneous FPS calculated from the current scheduled interval.
    pub fps: f64,
}

/// Fixed-tick cadence controller managing render requests, coalescing, and backpressure.
#[derive(Debug, Clone)]
pub struct Cadence {
    base_budget: Duration,
    current_interval: Duration,
    accumulated: Duration,
    render_requested: bool,
    frame_count: u64,
    last_frame_cost: Duration,
    backpressure_stretched: bool,
}

impl Default for Cadence {
    fn default() -> Self {
        Self::new()
    }
}

impl Cadence {
    /// Create a new 30fps cadence controller.
    pub fn new() -> Self {
        Self::with_budget(BASE_INTERVAL)
    }

    /// Create a cadence controller with a custom base interval.
    pub fn with_budget(budget: Duration) -> Self {
        Self {
            base_budget: budget,
            current_interval: budget,
            accumulated: Duration::ZERO,
            render_requested: false,
            frame_count: 0,
            last_frame_cost: Duration::ZERO,
            backpressure_stretched: false,
        }
    }

    /// Request that a frame be rendered on the next tick.
    ///
    /// Multiple render requests within the same tick interval coalesce into a
    /// single rendered frame.
    pub fn request_render(&mut self) {
        self.render_requested = true;
    }

    /// Advance time by `dt` and determine whether a frame should render.
    ///
    /// Returns `true` if a coalesced frame is ready to render.
    pub fn tick(&mut self, dt: Duration) -> bool {
        self.accumulated += dt;
        if self.accumulated >= self.current_interval {
            self.accumulated = self.accumulated.saturating_sub(self.current_interval);
            if self.render_requested {
                self.render_requested = false;
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Record the actual rendering time taken by a completed frame.
    ///
    /// Implements adaptive backpressure: if the frame draw cost exceeded
    /// the target budget, stretch the next interval to prevent CPU spiraling.
    pub fn record_frame(&mut self, cost: Duration) {
        self.frame_count += 1;
        self.last_frame_cost = cost;

        if cost > self.base_budget {
            self.current_interval = cost;
            self.backpressure_stretched = true;
        } else {
            self.current_interval = self.base_budget;
            self.backpressure_stretched = false;
        }
    }

    /// Retrieve current rendering statistics.
    pub fn stats(&self) -> FrameStats {
        let secs = self.current_interval.as_secs_f64();
        let fps = if secs > 0.0 { 1.0 / secs } else { 0.0 };
        FrameStats {
            frame_count: self.frame_count,
            last_frame_cost: self.last_frame_cost,
            current_interval: self.current_interval,
            base_budget: self.base_budget,
            backpressure_stretched: self.backpressure_stretched,
            fps,
        }
    }

    /// Current scheduled frame interval.
    pub fn current_interval(&self) -> Duration {
        self.current_interval
    }

    /// Base budget duration.
    pub fn base_budget(&self) -> Duration {
        self.base_budget
    }
}

/// 80ms status spinner advancing across a ring of glyph frames.
#[derive(Debug, Clone)]
pub struct Spinner {
    frames: Vec<String>,
    interval: Duration,
    accumulated: Duration,
    index: usize,
}

impl Default for Spinner {
    fn default() -> Self {
        Self::default_unicode()
    }
}

impl Spinner {
    /// Create a spinner with custom frames and 80ms advance interval.
    pub fn new(frames: Vec<String>) -> Self {
        Self::with_interval(frames, SPINNER_INTERVAL)
    }

    /// Create a spinner with custom frames and custom advance interval.
    pub fn with_interval(frames: Vec<String>, interval: Duration) -> Self {
        Self {
            frames,
            interval,
            accumulated: Duration::ZERO,
            index: 0,
        }
    }

    /// Default unicode spinner per docs/theming.md.
    pub fn default_unicode() -> Self {
        Self::new(vec![
            "⠋".into(),
            "⠙".into(),
            "⠹".into(),
            "⠸".into(),
            "⠼".into(),
            "⠴".into(),
            "⠦".into(),
            "⠧".into(),
            "⠇".into(),
            "⠏".into(),
        ])
    }

    /// Advance time by `dt`. Returns `true` if the glyph frame advanced.
    pub fn tick(&mut self, dt: Duration) -> bool {
        if self.frames.is_empty() || self.interval.is_zero() {
            return false;
        }
        self.accumulated += dt;
        let advances = (self.accumulated.as_nanos() / self.interval.as_nanos()) as usize;
        if advances > 0 {
            self.accumulated = Duration::from_nanos(
                (self.accumulated.as_nanos() % self.interval.as_nanos()) as u64,
            );
            self.index = (self.index + advances) % self.frames.len();
            true
        } else {
            false
        }
    }

    /// Current spinner glyph string.
    pub fn current_frame(&self) -> &str {
        if self.frames.is_empty() {
            ""
        } else {
            &self.frames[self.index]
        }
    }

    /// Current frame index.
    pub fn index(&self) -> usize {
        self.index
    }
}

/// Progress value that smoothly eases toward a target without jumping or overshooting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EasedProgress {
    current: f64,
    target: f64,
    easing_rate: f64,
    max_step_per_sec: f64,
}

impl Default for EasedProgress {
    fn default() -> Self {
        Self::new(0.0)
    }
}

impl EasedProgress {
    /// Create a new progress tracker initialized to `initial` (clamped to 0.0..=1.0).
    pub fn new(initial: f64) -> Self {
        let val = initial.clamp(0.0, 1.0);
        Self {
            current: val,
            target: val,
            easing_rate: 6.0,
            max_step_per_sec: 1.0,
        }
    }

    /// Create an eased progress tracker with custom easing rate and max speed.
    pub fn with_tuning(initial: f64, easing_rate: f64, max_step_per_sec: f64) -> Self {
        let val = initial.clamp(0.0, 1.0);
        Self {
            current: val,
            target: val,
            easing_rate,
            max_step_per_sec,
        }
    }

    /// Set a new target value (clamped to 0.0..=1.0).
    pub fn set_target(&mut self, target: f64) {
        self.target = target.clamp(0.0, 1.0);
    }

    /// Current interpolated progress value.
    pub fn current(&self) -> f64 {
        self.current
    }

    /// Target progress value.
    pub fn target(&self) -> f64 {
        self.target
    }

    /// Check if current value has settled at target.
    pub fn is_settled(&self) -> bool {
        (self.current - self.target).abs() < 1e-6
    }

    /// Advance time by `dt`, easing current value toward target.
    ///
    /// Invariants strictly enforced:
    /// - Never overshoots target.
    /// - Never jumps: step is bounded by `max_step_per_sec * dt`.
    /// - Deterministic: identical `dt` sequences yield identical values.
    pub fn tick(&mut self, dt: Duration) {
        let dt_secs = dt.as_secs_f64();
        if dt_secs <= 0.0 {
            return;
        }

        let diff = self.target - self.current;
        if diff.abs() < 1e-6 {
            self.current = self.target;
            return;
        }

        // Exponential ease curve: fraction of remaining distance to close
        let fraction = 1.0 - (-self.easing_rate * dt_secs).exp();
        let mut step = diff * fraction;

        // Bounded per-tick rate clamp to prevent jumps
        let max_step = self.max_step_per_sec * dt_secs;
        if step.abs() > max_step {
            step = step.signum() * max_step;
        }

        let candidate = self.current + step;

        // Guarantee no overshoot
        if diff > 0.0 {
            self.current = candidate.min(self.target).clamp(0.0, 1.0);
        } else {
            self.current = candidate.max(self.target).clamp(0.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cadence_coalescing() {
        let mut cadence = Cadence::new();
        let frame_dt = cadence.base_budget();

        // Multiple requests before tick fires
        cadence.request_render();
        cadence.request_render();
        cadence.request_render();

        // Before interval completes -> no render
        let dt1 = frame_dt / 2;
        let dt2 = frame_dt - dt1;
        assert!(!cadence.tick(dt1));

        // When interval completes -> exactly one render
        assert!(cadence.tick(dt2));

        // Next tick with no request -> no render
        assert!(!cadence.tick(frame_dt));
    }

    #[test]
    fn test_adaptive_backpressure_stretches_interval() {
        let mut cadence = Cadence::new();
        let base = cadence.base_budget();

        // Normal frame within budget
        cadence.record_frame(Duration::from_millis(10));
        let stats = cadence.stats();
        assert!(!stats.backpressure_stretched);
        assert_eq!(stats.current_interval, base);

        // Slow frame exceeding budget (e.g. 50ms > 33.33ms)
        let slow_cost = Duration::from_millis(50);
        cadence.record_frame(slow_cost);
        let stats = cadence.stats();
        assert!(stats.backpressure_stretched);
        assert_eq!(stats.current_interval, slow_cost);
        assert!(stats.fps < 25.0);

        // Next frame recovers
        cadence.record_frame(Duration::from_millis(15));
        let stats = cadence.stats();
        assert!(!stats.backpressure_stretched);
        assert_eq!(stats.current_interval, base);
    }

    #[test]
    fn test_spinner_advances_every_80ms() {
        let mut spinner = Spinner::default_unicode();
        assert_eq!(spinner.index(), 0);
        assert_eq!(spinner.current_frame(), "⠋");

        // 40ms: no advance
        assert!(!spinner.tick(Duration::from_millis(40)));
        assert_eq!(spinner.index(), 0);

        // +40ms (total 80ms): advances to 1
        assert!(spinner.tick(Duration::from_millis(40)));
        assert_eq!(spinner.index(), 1);
        assert_eq!(spinner.current_frame(), "⠙");

        // +80ms: advances to 2
        assert!(spinner.tick(Duration::from_millis(80)));
        assert_eq!(spinner.index(), 2);
        assert_eq!(spinner.current_frame(), "⠹");

        // Fast forward 800ms (10 frames, completes ring)
        assert!(spinner.tick(Duration::from_millis(800)));
        assert_eq!(spinner.index(), 2);
    }

    #[test]
    fn test_eased_progress_never_overshoots_or_jumps() {
        let mut progress = EasedProgress::new(0.0);
        progress.set_target(0.8);

        let dt = Duration::from_millis(33); // ~30fps tick
        let mut prev = progress.current();

        for _ in 0..100 {
            progress.tick(dt);
            let curr = progress.current();

            // Strictly never exceeds target
            assert!(
                curr <= 0.8 + 1e-9,
                "overshot target: curr={curr}, target=0.8"
            );
            // Strictly non-decreasing
            assert!(curr >= prev - 1e-9, "decreased: curr={curr}, prev={prev}");
            // Bounded step per frame (never jumps)
            let step = curr - prev;
            assert!(
                step <= 1.0 * dt.as_secs_f64() + 1e-9,
                "jumped too far: step={step}"
            );

            prev = curr;
        }

        // Settles close to target
        assert!((progress.current() - 0.8).abs() < 0.05);

        // Test decreasing
        progress.set_target(0.2);
        for _ in 0..100 {
            progress.tick(dt);
            let curr = progress.current();
            // Strictly never below target
            assert!(
                curr >= 0.2 - 1e-9,
                "undershot target: curr={curr}, target=0.2"
            );
            assert!(curr <= prev + 1e-9, "increased: curr={curr}, prev={prev}");
            prev = curr;
        }
    }

    #[test]
    fn test_fixed_tick_determinism() {
        let run_simulation = || {
            let mut progress = EasedProgress::new(0.0);
            let mut values = Vec::new();
            progress.set_target(0.75);
            for i in 1..=30 {
                progress.tick(Duration::from_millis(i * 2));
                values.push(progress.current());
            }
            values
        };

        let run1 = run_simulation();
        let run2 = run_simulation();
        assert_eq!(
            run1, run2,
            "identical tick sequence must produce identical outputs"
        );
    }
}
