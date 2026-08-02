//! Frame-rate-independent fling momentum for the infinite canvas.
//!
//! `VelocityTracker` turns a burst of raw pan deltas (with event
//! timestamps) into a launch velocity in px/sec over a fixed 80 ms window.
//! `MomentumState` integrates that velocity with exponential half-life
//! decay; displacement is integrated exactly across each tick, so the total
//! distance travelled over a fixed wall-clock duration is independent of the
//! tick rate.
//!
//! Fresh, original implementation (std-only, f64).

use std::collections::VecDeque;
use std::time::Duration;

use crate::canvas_engine::geom::Pt;

/// Samples older than this (by event timestamp) are dropped from the window.
const VELOCITY_WINDOW_MS: u32 = 80;
/// The window duration in seconds; launch velocity is total delta / window.
const VELOCITY_WINDOW_SECS: f64 = 0.08;
/// Velocities below this magnitude (px/sec) count as stopped.
const STOP_THRESHOLD: f64 = 0.05;

/// Collects raw pan deltas with event timestamps and derives a launch
/// velocity from the motion inside the trailing 80 ms window.
#[derive(Debug, Default)]
pub struct VelocityTracker {
    samples: VecDeque<(u32, Pt)>,
}

impl VelocityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a raw pan delta stamped with its event time (ms); samples
    /// older than the 80 ms window are dropped first.
    pub fn push(&mut self, time_ms: u32, delta: Pt) {
        while let Some(&(t, _)) = self.samples.front() {
            if time_ms.saturating_sub(t) > VELOCITY_WINDOW_MS {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        self.samples.push_back((time_ms, delta));
    }

    /// Total delta accumulated over the window divided by the window
    /// duration, in px/sec. `Pt::ZERO` when no samples are retained.
    pub fn launch_velocity(&self) -> Pt {
        let mut total = Pt::ZERO;
        for &(_, d) in &self.samples {
            total = total.add(d);
        }
        total.mul(1.0 / VELOCITY_WINDOW_SECS)
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

/// An exponential-decaying fling velocity (px/sec) integrated per tick.
#[derive(Debug)]
pub struct MomentumState {
    velocity: Pt,
    half_life: f64,
    tracker: VelocityTracker,
    moving: bool,
}

impl MomentumState {
    pub fn new(half_life_secs: f64) -> Self {
        Self {
            velocity: Pt::ZERO,
            half_life: half_life_secs,
            tracker: VelocityTracker::new(),
            moving: false,
        }
    }

    /// Feed a raw pan delta with its event timestamp (ms). Motion
    /// accumulates until [`Self::launch`] converts it into a velocity.
    pub fn accumulate(&mut self, delta: Pt, time_ms: u32) {
        self.tracker.push(time_ms, delta);
    }

    /// Convert the accumulated pan motion into a px/sec launch velocity and
    /// start the exponential decay. Does not start a fling for negligible
    /// motion.
    pub fn launch(&mut self) {
        self.velocity = self.tracker.launch_velocity();
        self.tracker.clear();
        self.moving = self.velocity.length() >= STOP_THRESHOLD;
    }

    /// Advance the fling by `dt`, returning the displacement to apply this
    /// tick, or `None` once stopped. The velocity decays by `0.5^(dt/half_life)`
    /// each tick and is zeroed once below [`STOP_THRESHOLD`]. Displacement is
    /// the exact integral of the continuous decay over the tick, which keeps
    /// the total travelled distance tick-rate independent.
    pub fn tick(&mut self, dt: Duration) -> Option<Pt> {
        if !self.moving {
            return None;
        }
        let dt_secs = dt.as_secs_f64();
        let decay = 0.5_f64.powf(dt_secs / self.half_life);
        let disp = self
            .velocity
            .mul((1.0 - decay) * self.half_life / std::f64::consts::LN_2);
        self.velocity = self.velocity.mul(decay);
        if self.velocity.length() < STOP_THRESHOLD {
            self.velocity = Pt::ZERO;
            self.moving = false;
        }
        Some(disp)
    }

    /// Stop the fling immediately.
    pub fn stop(&mut self) {
        self.velocity = Pt::ZERO;
        self.tracker.clear();
        self.moving = false;
    }

    pub fn velocity(&self) -> Pt {
        self.velocity
    }

    pub fn is_moving(&self) -> bool {
        self.moving
    }
}

impl Default for MomentumState {
    /// Default half-life of 160 ms.
    fn default() -> Self {
        Self::new(0.16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64, eps: f64) {
        assert!(
            (actual - expected).abs() <= eps,
            "expected {actual} within {eps} of {expected}"
        );
    }

    #[test]
    fn launch_velocity_from_80ms_window() {
        let mut tracker = VelocityTracker::new();
        for time_ms in [0u32, 20, 40, 60] {
            tracker.push(time_ms, Pt::new(25.0, 0.0));
        }
        // 100 px over the 80 ms window -> 1250 px/sec.
        let v = tracker.launch_velocity();
        assert_close(v.x, 1250.0, 1e-9);
        assert_eq!(v.y, 0.0);
    }

    #[test]
    fn drops_samples_older_than_window() {
        let mut tracker = VelocityTracker::new();
        tracker.push(0, Pt::new(1000.0, 0.0));
        // 200 ms later the old sample is outside the 80 ms window.
        tracker.push(200, Pt::new(0.0, 0.0));
        assert_eq!(tracker.launch_velocity(), Pt::ZERO);
        tracker.push(240, Pt::new(40.0, 0.0));
        assert_close(tracker.launch_velocity().x, 500.0, 1e-9);
        tracker.clear();
        assert_eq!(tracker.launch_velocity(), Pt::ZERO);
    }

    #[test]
    fn frame_rate_independent_convergence() {
        let run = |hz: u32| {
            let mut m = MomentumState::new(0.16);
            // 64 px of pan across 60 ms -> 64 / 0.08 = 800 px/sec launch.
            for time_ms in [0u32, 20, 40, 60] {
                m.accumulate(Pt::new(16.0, 0.0), time_ms);
            }
            m.launch();
            assert_close(m.velocity().x, 800.0, 1e-9);
            let dt = Duration::from_secs_f64(1.0 / f64::from(hz));
            let mut total = 0.0;
            for _ in 0..hz {
                if let Some(disp) = m.tick(dt) {
                    total += disp.x;
                }
            }
            total
        };
        let t30 = run(30);
        let t60 = run(60);
        let t120 = run(120);
        let max = t30.max(t60).max(t120);
        let min = t30.min(t60).min(t120);
        assert!(max > 0.0, "expected forward motion");
        let rel = (max - min) / max;
        assert!(
            rel < 0.005,
            "cumulative displacement drifts across tick rates: {rel} (30Hz={t30}, 60Hz={t60}, 120Hz={t120})"
        );
    }

    #[test]
    fn tick_past_threshold_stops() {
        let mut m = MomentumState::new(0.16);
        assert!(!m.is_moving());
        assert_eq!(m.tick(Duration::from_millis(16)), None);
        // 0.016 px over 80 ms -> 0.2 px/sec launch.
        m.accumulate(Pt::new(0.016, 0.0), 0);
        m.launch();
        assert!(m.is_moving());
        // One second of half-life decay crushes the velocity below 0.05.
        assert!(m.tick(Duration::from_secs(1)).is_some());
        assert!(!m.is_moving());
        assert_eq!(m.velocity(), Pt::ZERO);
        assert_eq!(m.tick(Duration::from_secs(1)), None);
        // stop() ends a fling immediately.
        m.accumulate(Pt::new(1.0, 0.0), 0);
        m.launch();
        assert!(m.is_moving());
        m.stop();
        assert!(!m.is_moving());
        assert_eq!(m.velocity(), Pt::ZERO);
        assert_eq!(m.tick(Duration::from_secs(1)), None);
    }

    #[test]
    fn launch_without_motion_does_not_start_fling() {
        let mut m = MomentumState::new(0.16);
        m.launch();
        assert!(!m.is_moving());
        assert_eq!(m.tick(Duration::from_secs(1)), None);
    }

    #[test]
    fn default_momentum_state_is_stopped() {
        let mut m = MomentumState::default();
        assert!(!m.is_moving());
        assert_eq!(m.velocity(), Pt::ZERO);
        assert_eq!(m.tick(Duration::from_secs(1)), None);
    }
}
