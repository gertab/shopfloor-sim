//! Path planning. v1 drives in a straight line; an obstacle-avoiding planner can replace it
//! without touching the engine.

use crate::geometry::Vec2;
use crate::scenario::Scenario;

pub trait PathPlanner: Send + Sync {
    /// Waypoints from `from` (exclusive) to `to` (inclusive). The last waypoint must be `to`.
    fn plan(&self, from: Vec2, to: Vec2, scenario: &Scenario) -> Vec<Vec2>;
}

/// Drives straight to the target, ignoring obstacles.
#[derive(Debug, Default, Clone, Copy)]
pub struct StraightLine;

impl PathPlanner for StraightLine {
    fn plan(&self, _from: Vec2, to: Vec2, _scenario: &Scenario) -> Vec<Vec2> {
        vec![to]
    }
}
