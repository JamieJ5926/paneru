//! Auto-placement: arrange canvas surfaces in a deterministic flow grid.
//!
//! `grid_positions` assigns each surface the next free slot, filling rows
//! left-to-right then top-to-bottom. Surfaces smaller than the slot are
//! top-left aligned inside it; larger surfaces clamp the slot to their own
//! size (one per slot). Margins create breathing room between slots.

use crate::canvas_engine::geom::{Pt, Rect};

/// Computes the top-left world position for every surface when auto-placing
/// them in a flow grid inside `viewport`.
///
/// - `rects` is the stable (WinID-ordered) list of current world rects.
/// - `margin` is the gap between slots (and from the viewport edge).
/// - Slots are sized to the largest surface, clamped to the viewport.
/// - Rows wrap at `slot_w + margin`; columns wrap at `slot_h + margin`.
///
/// Returns the same count of positions, in input order.
pub fn grid_positions(
    rects: &[Rect],
    viewport: Rect,
    margin: f64,
) -> Vec<Pt> {
    if rects.is_empty() {
        return Vec::new();
    }
    let slot_w = rects
        .iter()
        .map(|rect| rect.w)
        .fold(0.0, f64::max)
        .min(viewport.w);
    let slot_h = rects
        .iter()
        .map(|rect| rect.h)
        .fold(0.0, f64::max)
        .min(viewport.h);

    let usable_w = (viewport.w - margin).max(0.0);
    let columns = ((usable_w + margin) / (slot_w + margin))
        .floor()
        .max(1.0) as usize;

    rects
        .iter()
        .enumerate()
        .map(|(index, _rect)| {
            let column = index % columns;
            let row = index / columns;
            Pt::new(
                viewport.x + margin + column as f64 * (slot_w + margin),
                viewport.y + margin + row as f64 * (slot_h + margin),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn grid_fills_rows_then_wraps() {
        let viewport = Rect::new(0.0, 0.0, 2000.0, 1000.0);
        let rects = vec![rect(0.0, 0.0, 640.0, 420.0); 5];
        let positions = grid_positions(&rects, viewport, 10.0);
        // Columns = floor((2000 - 10 + 10) / (640 + 10)) = floor(2000/650) = 3.
        assert_eq!(positions.len(), 5);
        assert_eq!(positions[0], Pt::new(10.0, 10.0));
        assert_eq!(positions[1], Pt::new(660.0, 10.0));
        assert_eq!(positions[2], Pt::new(1310.0, 10.0));
        // Row 2 starts under row 1.
        assert_eq!(positions[3], Pt::new(10.0, 440.0));
        assert_eq!(positions[4], Pt::new(660.0, 440.0));
    }

    #[test]
    fn grid_is_deterministic_and_preserves_order() {
        let viewport = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let rects = vec![
            rect(700.0, 700.0, 300.0, 200.0),
            rect(0.0, 0.0, 300.0, 200.0),
            rect(500.0, 500.0, 300.0, 200.0),
        ];
        let first = grid_positions(&rects, viewport, 0.0);
        let second = grid_positions(&rects, viewport, 0.0);
        assert_eq!(first, second);
        // Input order is preserved: the first rect gets the first slot.
        assert_eq!(first[0], Pt::new(0.0, 0.0));
        assert_eq!(first[1], Pt::new(300.0, 0.0));
        assert_eq!(first[2], Pt::new(600.0, 0.0));
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(grid_positions(&[], Rect::new(0.0, 0.0, 100.0, 100.0), 5.0).is_empty());
    }

    #[test]
    fn slot_size_tracks_largest_surface() {
        let viewport = Rect::new(0.0, 0.0, 1300.0, 500.0);
        let rects = vec![rect(0.0, 0.0, 400.0, 300.0), rect(0.0, 0.0, 600.0, 400.0)];
        let positions = grid_positions(&rects, viewport, 0.0);
        // Slot is 600x400 (largest); second surface starts at x=600.
        assert_eq!(positions[1], Pt::new(600.0, 0.0));
    }
}
