//! Connected-component grouping of canvas surfaces for cluster movement.
//!
//! `adjacent_side` decides whether two rects sit flush against each other at
//! a given gap; `cluster_of` gathers the transitive connected component of a
//! key; `resolve_cluster_shifts` produces the shifted copy of a rect array
//! when one member is moved.
//!
//! Fresh, original implementation (std-only, f64).

use std::collections::{HashSet, VecDeque};

use crate::canvas_engine::geom::{Pt, Rect};
use crate::canvas_engine::snap::SnapRect;

/// Separation considered exactly equal to the requested gap on the parallel
/// axis (after the perpendicular-overlap check).
const EPS: f64 = 1e-9;

/// Which side of `a` a neighboring rect sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }
}

/// The side of `a` that `b` sits on, when the two rects share an edge pair
/// separated by exactly `gap` (within 1e-9) with strictly positive overlap
/// on the perpendicular axis. `None` when they do not touch at that gap.
///
/// - `Side::Left`: `a.x_low - b.x_high == gap` (b is to the left of a)
/// - `Side::Right`: `b.x_low - a.x_high == gap`
/// - `Side::Top`: `a.y_low - b.y_high == gap`
/// - `Side::Bottom`: `b.y_low - a.y_high == gap`
pub fn adjacent_side(a: &SnapRect, b: &SnapRect, gap: f64) -> Option<Side> {
    let y_overlap = a.y_low < b.y_high && b.y_low < a.y_high;
    let x_overlap = a.x_low < b.x_high && b.x_low < a.x_high;
    if y_overlap {
        if (a.x_low - b.x_high - gap).abs() <= EPS {
            return Some(Side::Left);
        }
        if (b.x_low - a.x_high - gap).abs() <= EPS {
            return Some(Side::Right);
        }
    }
    if x_overlap {
        if (a.y_low - b.y_high - gap).abs() <= EPS {
            return Some(Side::Top);
        }
        if (b.y_low - a.y_high - gap).abs() <= EPS {
            return Some(Side::Bottom);
        }
    }
    None
}

/// The transitive connected component containing `root`: every key whose
/// rect is reachable from `root`'s rect through `adjacent_side` links at
/// `gap`. Includes `root` when it is present; empty when it is unknown.
pub fn cluster_of<K: Clone + Eq + std::hash::Hash>(
    root: &K,
    windows: &[(K, SnapRect)],
    gap: f64,
) -> HashSet<K> {
    let Some(start) = windows.iter().position(|(k, _)| k == root) else {
        return HashSet::new();
    };
    let n = windows.len();
    let mut seen = vec![false; n];
    let mut queue = VecDeque::from([start]);
    seen[start] = true;
    while let Some(i) = queue.pop_front() {
        for j in 0..n {
            if seen[j] {
                continue;
            }
            if adjacent_side(&windows[i].1, &windows[j].1, gap).is_some() {
                seen[j] = true;
                queue.push_back(j);
            }
        }
    }
    let mut out = HashSet::new();
    for (i, (k, _)) in windows.iter().enumerate() {
        if seen[i] {
            out.insert(k.clone());
        }
    }
    out
}

/// Shift every rect transitively adjacent to `rects[moved]` (at `gap`) by
/// `delta`, including the moved rect itself, and return the shifted copy of
/// the whole array (same length and order; unrelated rects unchanged).
///
/// Panics when `moved` is out of bounds.
pub fn resolve_cluster_shifts(rects: &[SnapRect], gap: f64, moved: usize, delta: Pt) -> Vec<Rect> {
    let n = rects.len();
    let mut seen = vec![false; n];
    let mut queue = VecDeque::from([moved]);
    seen[moved] = true;
    while let Some(i) = queue.pop_front() {
        for j in 0..n {
            if seen[j] {
                continue;
            }
            if adjacent_side(&rects[i], &rects[j], gap).is_some() {
                seen[j] = true;
                queue.push_back(j);
            }
        }
    }
    rects
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if seen[i] {
                Rect::new(r.x_low + delta.x, r.y_low + delta.y, r.width(), r.height())
            } else {
                Rect::new(r.x_low, r.y_low, r.width(), r.height())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> SnapRect {
        SnapRect::new(x, y, w, h)
    }

    #[test]
    fn sides_and_opposites() {
        assert_eq!(Side::Left.opposite(), Side::Right);
        assert_eq!(Side::Right.opposite(), Side::Left);
        assert_eq!(Side::Top.opposite(), Side::Bottom);
        assert_eq!(Side::Bottom.opposite(), Side::Top);
        let a = rect(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            adjacent_side(&a, &rect(-100.0, 0.0, 100.0, 100.0), 0.0),
            Some(Side::Left)
        );
        assert_eq!(
            adjacent_side(&a, &rect(100.0, 0.0, 100.0, 100.0), 0.0),
            Some(Side::Right)
        );
        assert_eq!(
            adjacent_side(&a, &rect(0.0, -100.0, 100.0, 100.0), 0.0),
            Some(Side::Top)
        );
        assert_eq!(
            adjacent_side(&a, &rect(0.0, 100.0, 100.0, 100.0), 0.0),
            Some(Side::Bottom)
        );
    }

    #[test]
    fn cluster_is_transitive() {
        let a = rect(0.0, 0.0, 100.0, 100.0);
        let b = rect(100.0, 0.0, 100.0, 100.0);
        let c = rect(200.0, 0.0, 100.0, 100.0);
        // 100 px away from c: not adjacent at gap 0.
        let d = rect(400.0, 0.0, 100.0, 100.0);
        let windows = [("A", a), ("B", b), ("C", c), ("D", d)];
        let cluster = cluster_of(&"A", &windows, 0.0);
        assert!(cluster.contains(&"A"));
        assert!(cluster.contains(&"B"));
        assert!(cluster.contains(&"C"));
        assert!(!cluster.contains(&"D"));
        assert_eq!(cluster.len(), 3);
        // The component is the same seen from any member.
        assert_eq!(cluster_of(&"C", &windows, 0.0), cluster);
        // Unknown root -> empty component.
        assert!(cluster_of(&"ZZZ", &windows, 0.0).is_empty());
    }

    #[test]
    fn gap_rule() {
        let a = rect(0.0, 0.0, 100.0, 100.0);
        let b = rect(105.0, 0.0, 100.0, 100.0); // 5 px gap
        assert!(adjacent_side(&a, &b, 0.0).is_none());
        assert_eq!(adjacent_side(&a, &b, 5.0), Some(Side::Right));
        // Just below the gap is not adjacent.
        assert!(adjacent_side(&a, &b, 4.999).is_none());
        let windows = [("A", a), ("B", b)];
        assert_eq!(cluster_of(&"A", &windows, 0.0).len(), 1);
        assert_eq!(cluster_of(&"A", &windows, 5.0).len(), 2);
    }

    #[test]
    fn cluster_shift_moves_component_only() {
        let a = rect(0.0, 0.0, 100.0, 100.0);
        let b = rect(100.0, 0.0, 100.0, 100.0); // adjacent to a
        let c = rect(500.0, 500.0, 50.0, 50.0); // far away
        let rects = vec![a, b, c];
        // Moving A shifts A and B, leaves C.
        let out = resolve_cluster_shifts(&rects, 0.0, 0, Pt::new(10.0, 0.0));
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], Rect::new(10.0, 0.0, 100.0, 100.0));
        assert_eq!(out[1], Rect::new(110.0, 0.0, 100.0, 100.0));
        assert_eq!(out[2], Rect::new(500.0, 500.0, 50.0, 50.0));
        // Moving the lone rect moves only it.
        let out2 = resolve_cluster_shifts(&rects, 0.0, 2, Pt::new(0.0, -10.0));
        assert_eq!(out2[0], Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(out2[1], Rect::new(100.0, 0.0, 100.0, 100.0));
        assert_eq!(out2[2], Rect::new(500.0, 490.0, 50.0, 50.0));
        // Vertical adjacency shifts vertically.
        let d = rect(0.0, 100.0, 100.0, 100.0);
        let out3 = resolve_cluster_shifts(&[a, d], 0.0, 0, Pt::new(0.0, 20.0));
        assert_eq!(out3[0], Rect::new(0.0, 20.0, 100.0, 100.0));
        assert_eq!(out3[1], Rect::new(0.0, 120.0, 100.0, 100.0));
    }
}
