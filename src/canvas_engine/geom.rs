//! Minimal 2D geometry primitives for the canvas engine.
//!
//! Pure `f64` math, no external dependencies. This is a fresh original
//! implementation of the infinite-canvas geometry (feature port); it is not
//! copied from any upstream source.
//!
//! Coordinate convention (pinned everywhere in this module): the screen
//! transform is `screen = world * zoom + camera`, so
//! `world = (screen - camera) / zoom`. `camera` is the screen position of
//! world origin `(0, 0)`.

/// A 2D point in f64 space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

impl Pt {
    /// The origin `(0, 0)`.
    pub const ZERO: Self = Pt { x: 0.0, y: 0.0 };

    /// Creates a point from its coordinates.
    pub const fn new(x: f64, y: f64) -> Self {
        Pt { x, y }
    }

    /// Component-wise addition.
    pub fn add(self, o: Pt) -> Pt {
        Pt::new(self.x + o.x, self.y + o.y)
    }

    /// Component-wise subtraction.
    pub fn sub(self, o: Pt) -> Pt {
        Pt::new(self.x - o.x, self.y - o.y)
    }

    /// Scalar multiplication.
    pub fn mul(self, s: f64) -> Pt {
        Pt::new(self.x * s, self.y * s)
    }

    /// Euclidean length (`sqrt(x^2 + y^2)`).
    pub fn length(self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
}

impl From<(f64, f64)> for Pt {
    fn from((x, y): (f64, f64)) -> Self {
        Pt::new(x, y)
    }
}

impl From<Pt> for (f64, f64) {
    fn from(p: Pt) -> Self {
        (p.x, p.y)
    }
}

/// An axis-aligned rectangle with a position and a size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    /// Creates a rectangle from position and size.
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect { x, y, w, h }
    }

    /// Creates a rectangle from two corner points, normalizing the order so
    /// `x`/`y` are the min corner and `w`/`h` are non-negative.
    pub fn from_corners(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Rect::new(
            x0.min(x1),
            y0.min(y1),
            (x1 - x0).abs(),
            (y1 - y0).abs(),
        )
    }

    /// The center point of the rectangle.
    pub fn center(self) -> Pt {
        Pt::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// True when `p` lies inside the rectangle, with inclusive edges.
    pub fn contains(self, p: Pt) -> bool {
        p.x >= self.x
            && p.x <= self.x + self.w
            && p.y >= self.y
            && p.y <= self.y + self.h
    }

    /// True when the two rectangles share a strictly positive area
    /// (edge- or corner-touching alone is not an overlap).
    pub fn overlaps(self, o: Rect) -> bool {
        self.x < o.x + o.w
            && o.x < self.x + self.w
            && self.y < o.y + o.h
            && o.y < self.y + self.h
    }

    /// The smallest rectangle containing both input rectangles.
    pub fn union(self, o: Rect) -> Rect {
        Rect::from_corners(
            self.x.min(o.x),
            self.y.min(o.y),
            (self.x + self.w).max(o.x + o.w),
            (self.y + self.h).max(o.y + o.h),
        )
    }

    /// The bounding box of every rectangle in `iter`, or `None` when the
    /// iterator is empty.
    pub fn bbox(iter: impl IntoIterator<Item = Rect>) -> Option<Rect> {
        iter.into_iter().reduce(|acc, r| acc.union(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pt_ops_and_conversions() {
        let a = Pt::new(3.0, 4.0);
        assert_eq!(a.length(), 5.0);
        assert_eq!(a.add(Pt::new(1.0, -1.0)), Pt::new(4.0, 3.0));
        assert_eq!(a.sub(Pt::new(1.0, 2.0)), Pt::new(2.0, 2.0));
        assert_eq!(a.mul(2.0), Pt::new(6.0, 8.0));
        assert_eq!(Pt::ZERO, Pt::new(0.0, 0.0));
        let t: (f64, f64) = a.into();
        assert_eq!(t, (3.0, 4.0));
        assert_eq!(Pt::from((7.0, 9.0)), Pt::new(7.0, 9.0));
    }

    #[test]
    fn rect_from_corners_normalizes() {
        let r = Rect::from_corners(10.0, 20.0, 2.0, 8.0);
        assert_eq!(r, Rect::new(2.0, 8.0, 8.0, 12.0));
        assert_eq!(r.center(), Pt::new(6.0, 14.0));
    }

    #[test]
    fn rect_contains_inclusive_edges() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        assert!(r.contains(Pt::new(0.0, 0.0)));
        assert!(r.contains(Pt::new(100.0, 50.0)));
        assert!(r.contains(Pt::new(50.0, 25.0)));
        assert!(!r.contains(Pt::new(100.5, 25.0)));
        assert!(!r.contains(Pt::new(-0.1, 25.0)));
    }

    #[test]
    fn rect_overlaps_requires_positive_area() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(a.overlaps(Rect::new(5.0, 5.0, 10.0, 10.0)));
        // Edge-touching only: zero shared area must not count as overlap.
        assert!(!a.overlaps(Rect::new(10.0, 0.0, 10.0, 10.0)));
        assert!(!a.overlaps(Rect::new(0.0, 10.0, 10.0, 10.0)));
        assert!(!a.overlaps(Rect::new(20.0, 20.0, 5.0, 5.0)));
    }

    #[test]
    fn rect_union_and_bbox() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, -5.0, 5.0, 30.0);
        assert_eq!(a.union(b), Rect::new(0.0, -5.0, 25.0, 30.0));
        assert_eq!(Rect::bbox([a, b]), Some(Rect::new(0.0, -5.0, 25.0, 30.0)));
        assert_eq!(Rect::bbox(std::iter::empty::<Rect>()), None);
    }
}
