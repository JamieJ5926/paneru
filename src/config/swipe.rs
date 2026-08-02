use serde::Deserialize;

use crate::{config::deserialize_modifier, platform::Modifiers};

#[derive(Clone, Debug, Deserialize)]
pub enum SwipeGestureDirection {
    Natural,
    Reversed,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct SwipeOptions {
    /// Swipe sensitivity multiplier. Lower values = less distance per finger
    /// movement. Range: 0.1–2.0. Default: 0.35.
    pub sensitivity: Option<f64>,

    /// Swipe inertia deceleration rate. Higher values = faster stop.
    /// Range: 1.0–10.0. Default: 4.0.
    pub deceleration: Option<f64>,

    /// Swiping keeps sliding windows until the first or last window.
    /// Set to false to clamp so edge windows stay on-screen. Default: true.
    #[allow(dead_code)]
    pub continuous: Option<bool>,

    pub gesture: Option<GestureOptions>,
    pub scroll: Option<ScrollOptions>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct GestureOptions {
    /// The number of fingers required for swipe gestures to move windows.
    pub fingers_count: Option<usize>,

    /// Which direction swipe gestures should move windows.
    pub direction: Option<SwipeGestureDirection>,

    /// Whether to intercept vertical swipes.
    pub vertical: Option<bool>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct ScrollOptions {
    /// Modifier key(s) required for scroll wheel swiping.
    /// Accepts the same format as keybindings: "alt", "cmd", "alt + cmd", "alt + rcmd" etc.
    #[serde(default, deserialize_with = "deserialize_modifier")]
    pub modifier: Option<Modifiers>,

    /// Additional modifier key(s) that, combined with the scroll modifier,
    /// switches virtual workspaces vertically instead of scrolling horizontally.
    #[serde(default, deserialize_with = "deserialize_modifier")]
    pub vertical_modifier: Option<Modifiers>,

    /// Mos-style smoothing for discrete physical scroll-wheel ticks.
    /// Continuous trackpad/Magic Mouse events are unaffected.
    pub smoothing: Option<SmoothScrollOptions>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct SmoothScrollOptions {
    /// Master switch for smoothing discrete physical scroll-wheel ticks.
    /// When false, discrete wheel deltas move the strip immediately (Paneru's
    /// pre-smoothing behavior). Continuous trackpad/Magic Mouse events are
    /// never smoothed. Default: true.
    pub enabled: Option<bool>,

    /// Minimum normalized tick distance; small physical notches are raised to
    /// this value so a single notch always moves the strip. Range: 0.01-100.0.
    /// Default: 33.6.
    pub step: Option<f64>,

    /// Multiplier applied to each normalized tick before swipe.sensitivity.
    /// Range: 1.0-10.0. Default: 2.7.
    pub speed: Option<f64>,

    /// Time (ms) for interpolation to cover 95% of the remaining distance.
    /// Also sets the fling decay rate after tracking ends. Range: 50-2000.
    /// Default: 560.
    pub duration_ms: Option<u64>,

    /// Fling lands (and stops) when the projected remaining distance is at or
    /// below this many pixels. Range: 0.1-20.0. Default: 1.0.
    pub dead_zone: Option<f64>,
}
