//! Scenario files: the TOML schema ([`spec`]), validation ([`resolve`]) and the resolved
//! [`Scenario`], which is also what the app receives as the static layout.
//!
//! Every table uses `deny_unknown_fields` so a typo is an error instead of a silently ignored key.

use crate::geometry::{Rect, Vec2};
use crate::scene::SceneDef;
use anyhow::Context;
use serde::Serialize;
use std::path::{Path, PathBuf};
use ts_rs::TS;

mod outputs;
mod resolve;
mod spec;
#[cfg(test)]
mod tests;

pub(crate) use outputs::parse_broker;
pub use outputs::{MqttTopic, OutputSettings, OutputSpec, SinkDef, SinkKind};
pub(crate) use resolve::valid_id;
pub use spec::{
    ActionSpec, FloorSpec, MachineSpec, MmSpec, ReplaySettings, ScenarioSpec, SensorSpec, SimSettings,
};

/// The ids of the two docking actions. They exist only in a scenario that defines them, and have
/// built-in rules: `dock` takes an MM near a machine's dock onto it, `undock` takes it back out.
pub const DOCK: &str = "dock";
pub const UNDOCK: &str = "undock";
/// How near a machine's dock an MM must be for `dock` to take it in (or `dock_tolerance_m`, if larger).
pub const DOCK_RANGE_M: f64 = 1.0;
/// Where an MM waits beside a machine (after an `undock`, and in a replay): this far out from the
/// dock, away from the machine, leaning to one side (alternating by MM) so that two MMs waiting at
/// the same machine stand apart.
pub const APPROACH_M: f64 = 1.0;
const APPROACH_LEAN_RAD: f64 = std::f64::consts::PI / 6.0;

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct Scenario {
    pub name: String,
    pub source_path: Option<PathBuf>,
    pub sim: SimSettings,
    pub floor: Floor,
    pub machines: Vec<MachineDef>,
    pub mobile_manipulators: Vec<MmDef>,
    pub sensors: Vec<SensorDef>,
    pub actions: Vec<ActionDef>,
    pub scenes: Vec<SceneDef>,
    pub output: OutputSettings,
    /// The default traces to replay on this layout, with paths resolved.
    pub replay: Option<ReplaySettings>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct Floor {
    pub width_m: f64,
    pub height_m: f64,
    pub grid_m: f64,
}

impl Floor {
    pub fn rect(&self) -> Rect {
        Rect {
            min: Vec2::new(0.0, 0.0),
            max: Vec2::new(self.width_m, self.height_m),
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct MachineDef {
    pub id: String,
    pub label: String,
    pub position: Vec2,
    pub size: Vec2,
    pub dock: Vec2,
    /// Ids of the sensors attached to this machine.
    pub sensors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct MmDef {
    pub id: String,
    pub label: String,
    pub start: Vec2,
    pub start_at: Option<String>,
    pub start_heading_rad: f64,
    pub max_speed_mps: f64,
    pub radius_m: f64,
    /// Ids of the actions this MM can perform.
    pub actions: Vec<String>,
    pub sensors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ActionDef {
    pub id: String,
    pub label: String,
    pub requires_machine: bool,
    pub allowed_while_moving: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Machine,
    MobileManipulator,
    /// A free-standing sensor: it is its own entity, at a fixed position.
    Floor,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct SensorDef {
    /// `<host>.<name>` for mounted sensors (e.g. `mm1.vibration`); just `<name>` for floor sensors.
    pub id: String,
    pub name: String,
    /// The entity the sensor belongs to (its own id for floor sensors).
    pub host: String,
    pub host_kind: HostKind,
    pub kind: String,
    pub label: String,
    pub unit: String,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub period_ms: u64,
    pub noise_std: f64,
    /// Fixed position of a floor sensor (`None` for mounted sensors).
    pub position: Option<Vec2>,
}

impl Scenario {
    pub fn machine(&self, id: &str) -> Option<&MachineDef> {
        self.machines.iter().find(|machine| machine.id == id)
    }

    pub fn mm_index(&self, id: &str) -> Option<usize> {
        self.mobile_manipulators.iter().position(|mm| mm.id == id)
    }

    pub fn sensor_index(&self, id: &str) -> Option<usize> {
        self.sensors.iter().position(|sensor| sensor.id == id)
    }

    pub fn action(&self, id: &str) -> Option<&ActionDef> {
        self.actions.iter().find(|action| action.id == id)
    }

    pub fn scene_index(&self, id: &str) -> Option<usize> {
        self.scenes.iter().position(|scene| scene.id == id)
    }

    /// The area an MM's centre may occupy: the floor shrunk by its radius.
    pub fn reachable_area(&self, radius_m: f64) -> Rect {
        self.floor.rect().inset(radius_m)
    }

    /// The machine whose dock is within `dock_tolerance_m` of `point` (closest wins).
    pub fn machine_docked_at(&self, point: Vec2) -> Option<&MachineDef> {
        self.machines
            .iter()
            .map(|machine| (machine, machine.dock.distance(point)))
            .filter(|(_, distance)| *distance <= self.sim.dock_tolerance_m + 1e-9)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(machine, _)| machine)
    }

    /// The machine whose dock is within `range_m` of `point` (closest wins).
    pub fn machine_dock_within(&self, point: Vec2, range_m: f64) -> Option<&MachineDef> {
        self.machines
            .iter()
            .map(|machine| (machine, machine.dock.distance(point)))
            .filter(|(_, distance)| *distance <= range_m + 1e-9)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(machine, _)| machine)
    }

    /// Where MM number `mm_index` waits beside `machine` (see [`APPROACH_M`]).
    pub fn approach_point(&self, machine: &MachineDef, mm_index: usize, radius_m: f64) -> Vec2 {
        let out = machine.dock - machine.position;
        let out = if out.length() > 1e-9 {
            out.heading()
        } else {
            -std::f64::consts::FRAC_PI_2
        };
        let lean = if mm_index.is_multiple_of(2) {
            -APPROACH_LEAN_RAD
        } else {
            APPROACH_LEAN_RAD
        };
        let direction = Vec2::new((out + lean).cos(), (out + lean).sin());
        self.reachable_area(radius_m)
            .clamp(machine.dock + direction.scale(APPROACH_M))
    }
}

pub fn load_scenario(path: &Path) -> anyhow::Result<Scenario> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read scenario file {}", path.display()))?;
    parse_scenario(&text, Some(path)).with_context(|| format!("Failed to load scenario {}", path.display()))
}

/// Parses and validates scenario TOML. `source_path` is used to resolve relative sink paths.
pub fn parse_scenario(text: &str, source_path: Option<&Path>) -> anyhow::Result<Scenario> {
    let spec: ScenarioSpec = toml::from_str(text)?;
    resolve::resolve(spec, source_path)
}
