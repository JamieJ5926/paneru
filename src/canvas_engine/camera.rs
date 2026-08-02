//! Camera transforms and zoom/fit math for the infinite canvas.
//!
//! Pure `f64` math, no external dependencies. This is a fresh original
//! implementation of the infinite-canvas camera math (feature port); it is
//! not copied from any upstream source.
//!
//! Coordinate convention (pinned): `screen = world * zoom + camera`, so
//! `world = (screen - camera) / zoom`. `camera` is the screen position of
//! world origin `(0, 0)`.

use crate::canvas_engine::geom::{Pt, Rect};

/// Hard floor for zoom-out. Never allow the canvas to shrink below this.
pub const MIN_ZOOM_FLOOR: f64 = 0.001;

/// Hard ceiling for zoom-in. 1.0 means world units map 1:1 to screen pixels.
pub const MAX_ZOOM: f64 = 1.0;

/// Clamps a zoom value into `[MIN_ZOOM_FLOOR, MAX_ZOOM]`.
pub fn clamp_zoom(z: f64) -> f64 {
    z.clamp(MIN_ZOOM_FLOOR, MAX_ZOOM)
}

/// Converts a screen point into canvas/world coordinates:
/// `world = (screen - camera) / zoom`.
pub fn screen_to_canvas(screen: Pt, camera: Pt, zoom: f64) -> Pt {
    screen.sub(camera).mul(1.0 / zoom)
}

/// Converts a canvas/world point into screen coordinates:
/// `screen = world * zoom + camera`.
pub fn canvas_to_screen(canvas: Pt, camera: Pt, zoom: f64) -> Pt {
    canvas.mul(zoom).add(camera)
}

/// Returns the camera for a cursor-anchored zoom: the world point currently
/// under `anchor` stays under `anchor` after the zoom changes from `zoom` to
/// `new_zoom`. Formula: `camera_new = anchor - (anchor - camera) * (new_zoom / zoom)`.
pub fn zoom_anchor_camera(camera: Pt, zoom: f64, anchor: Pt, new_zoom: f64) -> Pt {
    anchor.sub(anchor.sub(camera).mul(new_zoom / zoom))
}

/// Returns the camera such that `center` (world space) appears at the center
/// of `viewport`: `camera = viewport.center() - center * zoom`.
pub fn camera_for_center(center: Pt, viewport: Rect, zoom: f64) -> Pt {
    viewport.center().sub(center.mul(zoom))
}

/// The world-space rectangle currently visible inside `viewport`, computed by
/// transforming the viewport corners back through the camera.
pub fn visible_canvas_rect(camera: Pt, viewport: Rect, zoom: f64) -> Rect {
    let a = screen_to_canvas(Pt::new(viewport.x, viewport.y), camera, zoom);
    let b = screen_to_canvas(
        Pt::new(viewport.x + viewport.w, viewport.y + viewport.h),
        camera,
        zoom,
    );
    Rect::from_corners(a.x, a.y, b.x, b.y)
}

/// The bounding box of every rectangle in `rects`, or `None` when empty.
pub fn all_windows_bbox(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    Rect::bbox(rects)
}

/// Computes the camera and zoom that fit `bbox` entirely inside `viewport`
/// with `margin` absolute pixels inset on every side of the viewport.
///
/// The zoom is `min(fit_w / bbox.w, fit_h / bbox.h)` clamped into
/// `[MIN_ZOOM_FLOOR, MAX_ZOOM]`, and the camera centers `bbox` in the
/// shrunken viewport.
pub fn zoom_to_fit(bbox: Rect, viewport: Rect, margin: f64) -> (Pt, f64) {
    // Guard against a margin that would collapse the fit viewport.
    let m = margin.max(0.0).min(viewport.w.min(viewport.h) / 2.0);
    let fit_view = Rect::new(
        viewport.x + m,
        viewport.y + m,
        viewport.w - 2.0 * m,
        viewport.h - 2.0 * m,
    );
    // A degenerate (zero-area) bbox fits at the maximum zoom.
    let bw = bbox.w.max(f64::EPSILON);
    let bh = bbox.h.max(f64::EPSILON);
    let zoom = clamp_zoom((fit_view.w / bw).min(fit_view.h / bh));
    let camera = camera_for_center(bbox.center(), fit_view, zoom);
    (camera, zoom)
}

/// The home camera: world origin appears at the viewport center at zoom 1.0.
pub fn home_camera(viewport: Rect) -> Pt {
    viewport.center()
}

/// Returns the closest item (by angular score) whose center lies within a
/// 45-degree cone pointing in `dir` from `origin`.
///
/// A candidate `(id, center)` qualifies when `dot(dir, d) > 0` and
/// `|cross(dir, d)| <= dot(dir, d)`, where `d = center - origin`. The score is
/// `|d|^2 / dot` (distance divided by `cos(angle)`, without a square root);
/// the item with the minimum score wins. `skip` excludes one specific item.
pub fn nearest_in_direction<W: PartialEq>(
    origin: Pt,
    dir: (f64, f64),
    items: impl Iterator<Item = (W, Pt)>,
    skip: Option<&W>,
) -> Option<W> {
    let (dx, dy) = dir;
    let mut best: Option<(W, f64)> = None;
    for (id, center) in items {
        if let Some(s) = skip {
            if s == &id {
                continue;
            }
        }
        let d = center.sub(origin);
        let dot = d.x * dx + d.y * dy;
        if dot <= 0.0 {
            continue;
        }
        let cross = d.x * dy - d.y * dx;
        if cross.abs() > dot {
            continue;
        }
        let score = (d.x * d.x + d.y * d.y) / dot;
        match &best {
            Some((_, best_score)) if *best_score <= score => {}
            _ => best = Some((id, score)),
        }
    }
    best.map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn approx_pt(a: Pt, b: Pt) -> bool {
        approx(a.x, b.x) && approx(a.y, b.y)
    }

    #[test]
    fn screen_canvas_round_trip_across_zooms_and_cameras() {
        let cameras = [
            Pt::new(0.0, 0.0),
            Pt::new(1234.5, -987.25),
            Pt::new(-300.0, 42.0),
        ];
        let screens = [
            Pt::new(0.0, 0.0),
            Pt::new(1920.0, 1080.0),
            Pt::new(640.5, -128.0),
        ];
        for zoom in [0.05, 0.25, 1.0] {
            for camera in cameras {
                for s in screens {
                    let world = screen_to_canvas(s, camera, zoom);
                    assert!(approx_pt(canvas_to_screen(world, camera, zoom), s));
                    // Direct formula check: screen = world * zoom + camera.
                    assert!(approx(world.x * zoom + camera.x, s.x));
                    assert!(approx(world.y * zoom + camera.y, s.y));
                }
            }
        }
    }

    #[test]
    fn cursor_anchored_zoom_keeps_world_point_fixed() {
        // (camera, zoom, anchor, new_zoom): zoom in and zoom out cases.
        let cases = [
            (Pt::new(500.0, 300.0), 0.25, Pt::new(777.0, 123.0), 0.8),
            (Pt::new(-200.0, 900.0), 0.8, Pt::new(1024.0, 512.0), 0.3),
        ];
        for (camera, zoom, anchor, new_zoom) in cases {
            let world = screen_to_canvas(anchor, camera, zoom);
            let new_camera = zoom_anchor_camera(camera, zoom, anchor, new_zoom);
            let after = screen_to_canvas(anchor, new_camera, new_zoom);
            assert!(approx_pt(after, world));
            // And the world point must land exactly on the anchor.
            assert!(approx_pt(
                canvas_to_screen(world, new_camera, new_zoom),
                anchor
            ));
        }
    }

    #[test]
    fn clamp_zoom_floors_and_caps() {
        assert_eq!(clamp_zoom(0.0001), MIN_ZOOM_FLOOR);
        assert_eq!(clamp_zoom(5.0), MAX_ZOOM);
        assert_eq!(clamp_zoom(0.001), MIN_ZOOM_FLOOR);
        assert_eq!(clamp_zoom(1.0), MAX_ZOOM);
        assert_eq!(clamp_zoom(0.25), 0.25);
        assert_eq!(clamp_zoom(-3.0), MIN_ZOOM_FLOOR);
    }

    #[test]
    fn zoom_to_fit_wide_and_tall_bboxes() {
        let viewport = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let margin = 50.0;
        let fit_view = Rect::new(50.0, 50.0, 900.0, 700.0);

        // Wide bbox: the fit is limited by width (900 / 1200 < 700 / 100).
        let wide = Rect::new(-600.0, -50.0, 1200.0, 100.0);
        let (camera, zoom) = zoom_to_fit(wide, viewport, margin);
        assert!(approx(zoom, 900.0 / 1200.0));
        assert!(zoom <= MAX_ZOOM);
        assert!(approx_pt(
            canvas_to_screen(wide.center(), camera, zoom),
            fit_view.center()
        ));

        // Tall bbox: the fit is limited by height (700 / 900 < 900 / 100).
        let tall = Rect::new(-50.0, -450.0, 100.0, 900.0);
        let (camera, zoom) = zoom_to_fit(tall, viewport, margin);
        assert!(approx(zoom, 700.0 / 900.0));
        assert!(zoom <= MAX_ZOOM);
        assert!(approx_pt(
            canvas_to_screen(tall.center(), camera, zoom),
            fit_view.center()
        ));
    }

    #[test]
    fn zoom_to_fit_clamps_above_max() {
        let viewport = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let small = Rect::new(0.0, 0.0, 100.0, 100.0);
        let (_, zoom) = zoom_to_fit(small, viewport, 0.0);
        assert_eq!(zoom, MAX_ZOOM);
    }

    #[test]
    fn nearest_in_direction_picks_aligned_and_skips() {
        let origin = Pt::new(0.0, 0.0);
        let dir = (1.0, 0.0);
        // A: closest aligned; B: aligned but farther; C: ~50.2 deg (outside
        // the 45 deg cone); D: behind the origin.
        let items = [
            ("A", Pt::new(100.0, 0.0)),
            ("B", Pt::new(200.0, 0.0)),
            ("C", Pt::new(50.0, 60.0)),
            ("D", Pt::new(-100.0, 0.0)),
        ];

        assert_eq!(
            nearest_in_direction(origin, dir, items.into_iter(), None),
            Some("A")
        );
        assert_eq!(
            nearest_in_direction(origin, dir, items.into_iter(), Some(&"A")),
            Some("B")
        );
    }

    #[test]
    fn visible_canvas_rect_and_home_camera() {
        let camera = Pt::new(100.0, 50.0);
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        let zoom = 0.5;
        let visible = visible_canvas_rect(camera, viewport, zoom);
        assert_eq!(
            visible,
            Rect::from_corners(-200.0, -100.0, 1400.0, 1100.0)
        );
        assert_eq!(home_camera(viewport), viewport.center());
        let cam = camera_for_center(Pt::new(400.0, 300.0), viewport, 0.5);
        assert!(approx_pt(
            canvas_to_screen(Pt::new(400.0, 300.0), cam, 0.5),
            viewport.center()
        ));
    }
}
