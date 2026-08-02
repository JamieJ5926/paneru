//! Edge snapping with hysteresis plus candidate extraction for the infinite
//! canvas.
//!
//! `SnapRect` is the axis-aligned low/high-bound geometry used by the snap
//! engine. `EdgeSnap` tracks one snapped edge with engage/hold/release
//! tolerances so a dragged edge latches onto a candidate and only lets go
//! after clearly leaving it. `snap_candidates` gathers the positions a
//! rect's four edges may snap to, rejecting diagonal (corner-only) contacts.
//!
//! Fresh, original implementation (std-only, f64).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapRect {
    pub x_low: f64,
    pub x_high: f64,
    pub y_low: f64,
    pub y_high: f64,
}

impl SnapRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self {
            x_low: x,
            x_high: x + w,
            y_low: y,
            y_high: y + h,
        }
    }

    pub fn width(self) -> f64 {
        self.x_high - self.x_low
    }

    pub fn height(self) -> f64 {
        self.y_high - self.y_low
    }

    /// True when the shared area is strictly positive; rects that merely
    /// touch along an edge or at a corner do not overlap.
    pub fn overlaps(self, o: Self) -> bool {
        self.x_low < o.x_high && o.x_low < self.x_high && self.y_low < o.y_high && o.y_low < self.y_high
    }
}

/// Engage/hold/release tolerances for one snapped edge, in the same unit as
/// the edge values. Invariant: `engage <= hold <= release`.
#[derive(Clone, Copy, Debug)]
pub struct SnapTolerance {
    pub engage: f64,
    pub hold: f64,
    pub release: f64,
}

/// Hysteresis state for a single snapping edge.
///
/// - Not held: engages the nearest candidate within `engage`.
/// - Held: reports the held position while the value stays within `release`
///   of it (the `hold` tolerance marks the comfortable band; between `hold`
///   and `release` the snap is preserved), and releases once the value has
///   drifted `>= release` away.
#[derive(Clone, Copy, Debug, Default)]
pub struct EdgeSnap {
    held: Option<f64>,
}

impl EdgeSnap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, value: f64, candidates: &[f64], tol: &SnapTolerance) -> Option<f64> {
        debug_assert!(
            tol.engage <= tol.hold && tol.hold <= tol.release,
            "snap tolerance invariant violated: engage <= hold <= release"
        );
        if let Some(h) = self.held {
            // Keep snapping while close enough to the held position; release
            // once the drift reaches the release threshold.
            if (value - h).abs() < tol.release {
                return Some(h);
            }
            self.held = None;
            return None;
        }
        // Not held: engage the nearest candidate within the engage tolerance.
        let mut best: Option<(f64, f64)> = None;
        for &c in candidates {
            let d = (value - c).abs();
            if d <= tol.engage {
                match best {
                    Some((bd, _)) if bd <= d => {}
                    _ => best = Some((d, c)),
                }
            }
        }
        match best {
            Some((_, c)) => {
                self.held = Some(c);
                Some(c)
            }
            None => None,
        }
    }

    pub fn release(&mut self) {
        self.held = None;
    }

    pub fn is_held(&self) -> bool {
        self.held.is_some()
    }
}

/// Candidate snap positions for the four edges of a rect. Each list holds
/// absolute positions the corresponding edge can align to; ordering is
/// unspecified (callers pick the nearest).
#[derive(Clone, Debug, Default)]
pub struct SnapCandidates {
    pub left: Vec<f64>,
    pub right: Vec<f64>,
    pub top: Vec<f64>,
    pub bottom: Vec<f64>,
}

/// Collect the snap candidates for `rect` against `others`.
///
/// Diagonal rejection: an edge pair is only a candidate when the two rects
/// have strictly positive overlap on the perpendicular axis, so rects that
/// merely meet at a corner never produce candidates. Left edge candidates
/// are `others'` right edges (`x_high`) where y-overlap is positive; right
/// are `others'` left edges (`x_low`) where y-overlap is positive; top are
/// `others'` bottom edges (`y_high`) where x-overlap is positive; bottom
/// are `others'` top edges (`y_low`) where x-overlap is positive.
pub fn snap_candidates(rect: &SnapRect, others: &[SnapRect]) -> SnapCandidates {
    let mut out = SnapCandidates::default();
    for o in others {
        let y_overlap = rect.y_low < o.y_high && o.y_low < rect.y_high;
        let x_overlap = rect.x_low < o.x_high && o.x_low < rect.x_high;
        if y_overlap {
            out.left.push(o.x_high);
            out.right.push(o.x_low);
        }
        if x_overlap {
            out.top.push(o.y_high);
            out.bottom.push(o.y_low);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engage_hold_break_sequence() {
        let mut snap = EdgeSnap::new();
        let tol = SnapTolerance {
            engage: 2.0,
            hold: 4.0,
            release: 8.0,
        };
        assert!(!snap.is_held());
        // 100 is within engage (1.5 <= 2.0) of candidate 101.5.
        assert_eq!(snap.update(100.0, &[101.5], &tol), Some(101.5));
        assert!(snap.is_held());
        // Drift to 103: still within hold of 101.5 (1.5 <= 4.0).
        assert_eq!(snap.update(103.0, &[101.5], &tol), Some(101.5));
        assert!(snap.is_held());
        // Drift to 109.5: 8.0 away from the held position, exactly the
        // release threshold -> break.
        assert_eq!(snap.update(109.5, &[101.5], &tol), None);
        assert!(!snap.is_held());
        // A fresh engage works after the break, and release() clears it.
        assert_eq!(snap.update(100.0, &[101.5], &tol), Some(101.5));
        snap.release();
        assert!(!snap.is_held());
    }

    #[test]
    fn nearest_candidate_engages() {
        let mut snap = EdgeSnap::new();
        let tol = SnapTolerance {
            engage: 2.0,
            hold: 4.0,
            release: 8.0,
        };
        // 101.0 is 1.0 away; 98.0 is 2.0 away; both within engage.
        assert_eq!(snap.update(100.0, &[98.0, 101.0], &tol), Some(101.0));
        // No candidate within engage -> no snap.
        assert_eq!(snap.update(200.0, &[101.5], &tol), None);
        assert!(!snap.is_held());
    }

    #[test]
    fn diagonal_corner_contact_rejected() {
        let a = SnapRect::new(0.0, 0.0, 100.0, 100.0);
        // Touches `a` only at its bottom-right corner:
        // a.x_high == b.x_low && a.y_high == b.y_low.
        let b = SnapRect::new(100.0, 100.0, 50.0, 50.0);
        let c = snap_candidates(&a, &[b]);
        assert!(c.left.is_empty());
        assert!(c.right.is_empty());
        assert!(c.top.is_empty());
        assert!(c.bottom.is_empty());
    }

    #[test]
    fn edge_candidates_require_perpendicular_overlap() {
        let a = SnapRect::new(0.0, 0.0, 100.0, 50.0);
        // Same y band as `a`: y-overlap is positive -> left/right candidates.
        let b = SnapRect::new(150.0, 0.0, 100.0, 50.0);
        // y band fully below `a`: no y-overlap -> contributes nothing.
        let c = SnapRect::new(150.0, 100.0, 100.0, 50.0);
        let cand = snap_candidates(&a, &[b, c]);
        assert_eq!(cand.left, vec![250.0]); // b.x_high
        assert_eq!(cand.right, vec![150.0]); // b.x_low
        assert!(cand.top.is_empty());
        assert!(cand.bottom.is_empty());
        // Full x-overlap with a rect below yields top/bottom candidates.
        let d = SnapRect::new(0.0, 60.0, 100.0, 50.0);
        let cand2 = snap_candidates(&a, &[d]);
        assert_eq!(cand2.top, vec![110.0]); // d.y_high
        assert_eq!(cand2.bottom, vec![60.0]); // d.y_low
        assert!(cand2.left.is_empty() && cand2.right.is_empty());
    }

    #[test]
    fn snap_rect_overlap_is_strictly_positive() {
        let a = SnapRect::new(0.0, 0.0, 10.0, 10.0);
        assert!(a.overlaps(SnapRect::new(5.0, 5.0, 10.0, 10.0)));
        assert!(!a.overlaps(SnapRect::new(10.0, 0.0, 10.0, 10.0))); // shared edge
        assert!(!a.overlaps(SnapRect::new(10.0, 10.0, 10.0, 10.0))); // shared corner
        assert_eq!(a.width(), 10.0);
        assert_eq!(a.height(), 10.0);
    }
}
