//! Integer rectangles in desktop coordinates (pure, no OS calls).

/// Axis-aligned rectangle; `x`/`y` is the top-left corner, Y grows downwards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width (never negative).
    pub width: i32,
    /// Height (never negative).
    pub height: i32,
}

impl Rect {
    /// Builds a rectangle; negative sizes are clamped to zero.
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width: if width < 0 { 0 } else { width },
            height: if height < 0 { 0 } else { height },
        }
    }

    /// Builds a rectangle from its edges (`left, top, right, bottom`, right/bottom exclusive).
    pub const fn from_edges(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self::new(
            left,
            top,
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        )
    }

    /// Exclusive right edge.
    pub const fn right(&self) -> i32 {
        self.x.saturating_add(self.width)
    }

    /// Exclusive bottom edge.
    pub const fn bottom(&self) -> i32 {
        self.y.saturating_add(self.height)
    }

    /// True when the rectangle has no area.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// True when `other` lies entirely inside `self` (edges may touch).
    pub const fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// True when the point lies inside (right/bottom edges excluded).
    pub const fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    /// Area of the intersection with `other` (0 when disjoint).
    pub fn intersection_area(&self, other: &Rect) -> i64 {
        let w = i64::from(self.right().min(other.right())) - i64::from(self.x.max(other.x));
        let h = i64::from(self.bottom().min(other.bottom())) - i64::from(self.y.max(other.y));
        if w <= 0 || h <= 0 { 0 } else { w * h }
    }

    /// Translated copy.
    pub const fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(
            self.x.saturating_add(dx),
            self.y.saturating_add(dy),
            self.width,
            self.height,
        )
    }

    /// Smallest rectangle containing both (an empty rectangle is ignored).
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        Rect::from_edges(
            self.x.min(other.x),
            self.y.min(other.y),
            self.right().max(other.right()),
            self.bottom().max(other.bottom()),
        )
    }
}

/// Index of the rectangle in `candidates` that overlaps `target` the most (ties: first one).
pub fn best_overlap(target: &Rect, candidates: impl IntoIterator<Item = Rect>) -> Option<usize> {
    let mut best: Option<(usize, i64)> = None;
    for (i, r) in candidates.into_iter().enumerate() {
        let area = r.intersection_area(target);
        if area > 0 && best.is_none_or(|(_, a)| area > a) {
            best = Some((i, area));
        }
    }
    best.map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_and_containment() {
        let m = Rect::new(1920, 0, 2560, 1440);
        assert_eq!(m.right(), 4480);
        assert_eq!(m.bottom(), 1440);
        assert!(m.contains_rect(&m));
        assert!(Rect::new(1912, -8, 2576, 1456).contains_rect(&m));
        assert!(!Rect::new(1920, 0, 2560, 1400).contains_rect(&m));
        assert!(m.contains_point(1920, 0));
        assert!(!m.contains_point(4480, 0));
    }

    #[test]
    fn negative_sizes_clamp() {
        assert_eq!(Rect::new(0, 0, -5, 3).width, 0);
        assert!(Rect::from_edges(10, 10, 5, 20).is_empty());
    }

    #[test]
    fn overlap_and_union() {
        let a = Rect::new(0, 0, 1920, 1080);
        let b = Rect::new(1920, 0, 1920, 1080);
        assert_eq!(a.intersection_area(&b), 0);
        assert_eq!(a.union(&b), Rect::new(0, 0, 3840, 1080));
        let w = Rect::new(1800, 100, 400, 300);
        assert_eq!(best_overlap(&w, [a, b]), Some(1));
        assert_eq!(best_overlap(&Rect::new(-500, -500, 10, 10), [a, b]), None);
        assert_eq!(Rect::default().union(&a), a);
    }
}
