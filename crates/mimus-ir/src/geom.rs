//! Rectangles and affine transforms in PDF user space.

use serde::{Deserialize, Serialize};

/// An axis-aligned box from `(x0, y0)` to `(x1, y1)`, y growing upwards.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Box2 {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Box2 {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    pub fn area(&self) -> f32 {
        (self.width() * self.height()).max(0.0)
    }

    pub fn intersection(&self, other: &Box2) -> Option<Box2> {
        let b = Box2 {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (b.x1 > b.x0 && b.y1 > b.y0).then_some(b)
    }

    /// Fraction of `self` covered by `other`. Layout attribution ranks by this
    /// rather than by symmetric IoU, because a character is tiny next to the
    /// region that contains it.
    pub fn coverage_by(&self, other: &Box2) -> f32 {
        match self.intersection(other) {
            Some(i) if self.area() > 0.0 => i.area() / self.area(),
            _ => 0.0,
        }
    }
}

/// A PDF `cm`-style matrix `[a b c d e f]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_is_asymmetric() {
        let glyph = Box2 {
            x0: 10.0,
            y0: 10.0,
            x1: 20.0,
            y1: 20.0,
        };
        let region = Box2 {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 100.0,
        };
        assert_eq!(glyph.coverage_by(&region), 1.0);
        assert!(region.coverage_by(&glyph) < 0.02);
    }

    #[test]
    fn disjoint_boxes_do_not_intersect() {
        let a = Box2 {
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 1.0,
        };
        let b = Box2 {
            x0: 2.0,
            y0: 2.0,
            x1: 3.0,
            y1: 3.0,
        };
        assert!(a.intersection(&b).is_none());
        assert_eq!(a.coverage_by(&b), 0.0);
    }
}
