//! Plane geometry in floor coordinates: metres, origin bottom-left, y up, heading 0 = +x (CCW positive).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn distance(self, other: Vec2) -> f64 {
        (other - self).length()
    }

    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }

    /// Heading (radians) of the vector, `atan2(y, x)`.
    pub fn heading(self) -> f64 {
        self.y.atan2(self.x)
    }

    pub fn scale(self, factor: f64) -> Vec2 {
        Vec2::new(self.x * factor, self.y * factor)
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl From<[f64; 2]> for Vec2 {
    fn from([x, y]: [f64; 2]) -> Self {
        Vec2::new(x, y)
    }
}

impl std::ops::Add for Vec2 {
    type Output = Vec2;
    fn add(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl std::ops::Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

/// Axis-aligned rectangle given by its minimum and maximum corners.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    pub fn from_center(center: Vec2, size: Vec2) -> Self {
        let half = size.scale(0.5);
        Self {
            min: center - half,
            max: center + half,
        }
    }

    /// Shrinks the rectangle by `margin` on every side.
    pub fn inset(self, margin: f64) -> Self {
        Self {
            min: Vec2::new(self.min.x + margin, self.min.y + margin),
            max: Vec2::new(self.max.x - margin, self.max.y - margin),
        }
    }

    /// Inclusive containment, with a tiny tolerance for values computed from floats.
    pub fn contains(self, point: Vec2) -> bool {
        const EPS: f64 = 1e-9;
        point.x >= self.min.x - EPS
            && point.x <= self.max.x + EPS
            && point.y >= self.min.y - EPS
            && point.y <= self.max.y + EPS
    }

    pub fn contains_rect(self, other: Rect) -> bool {
        self.contains(other.min) && self.contains(other.max)
    }
}

/// Total length of the polyline `start → points[0] → points[1] → …`.
pub fn path_length(start: Vec2, points: &[Vec2]) -> f64 {
    let mut total = 0.0;
    let mut current = start;
    for &point in points {
        total += current.distance(point);
        current = point;
    }
    total
}

/// Rounds to a fixed number of decimals (for readable event output; state keeps full precision).
pub fn round_to(value: f64, decimals: i32) -> f64 {
    let factor = 10f64.powi(decimals);
    // `+ 0.0` turns -0.0 into 0.0 so it never serialises as "-0.0".
    (value * factor).round() / factor + 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_and_inset() {
        let floor = Rect {
            min: Vec2::new(0.0, 0.0),
            max: Vec2::new(10.0, 5.0),
        };
        assert!(floor.contains(Vec2::new(0.0, 0.0)));
        assert!(floor.contains(Vec2::new(10.0, 5.0)));
        assert!(!floor.contains(Vec2::new(10.1, 1.0)));
        assert!(!floor.inset(0.5).contains(Vec2::new(0.2, 2.0)));
        assert!(floor.contains_rect(Rect::from_center(Vec2::new(5.0, 2.5), Vec2::new(2.0, 1.0))));
    }

    #[test]
    fn path_length_sums_segments() {
        let length = path_length(Vec2::new(0.0, 0.0), &[Vec2::new(3.0, 4.0), Vec2::new(3.0, 0.0)]);
        assert!((length - 9.0).abs() < 1e-12);
    }

    #[test]
    fn round_to_decimals() {
        assert_eq!(round_to(1.23456, 3), 1.235);
        assert!(round_to(-0.0004, 3).is_sign_positive());
    }
}
