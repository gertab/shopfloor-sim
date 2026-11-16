//! The scenario file's schema: what a scenario TOML holds, before it's checked.

use super::OutputSpec;
use crate::replay::Format;
use crate::scene::SceneSpec;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use ts_rs::TS;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioSpec {
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub sim: SimSettings,
    pub floor: FloorSpec,
    #[serde(default)]
    pub machines: BTreeMap<String, MachineSpec>,
    #[serde(default)]
    pub mobile_manipulators: BTreeMap<String, MmSpec>,
    /// Free-standing sensors at a fixed floor position (`position` required).
    #[serde(default)]
    pub sensors: BTreeMap<String, SensorSpec>,
    /// The catalogue of actions MMs can perform (silent: they only produce an `action` event).
    #[serde(default)]
    pub actions: BTreeMap<String, ActionSpec>,
    /// Scripted runs of commands and waits (see `scene.rs`).
    #[serde(default)]
    pub scenes: BTreeMap<String, SceneSpec>,
    #[serde(default)]
    pub output: OutputSpec,
    /// Liminal Desk traces to replay on this layout (see `replay.rs`): what the app's Replay section
    /// and `shopfloor replay` start with. The scenario still simulates until a replay is started.
    pub replay: Option<ReplaySettings>,
}

/// `[replay]`: the traces to play on this layout, and how.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ReplaySettings {
    /// Trace files, played side by side. Relative paths are resolved against the scenario file.
    pub traces: Vec<PathBuf>,
    /// Each payload as written (`payload`), or with `v`, `seq`, `sim_ms` and `kind` added
    /// (`envelope`).
    #[serde(default)]
    pub format: Format,
    /// Also report the positions of moving MMs.
    #[serde(default)]
    pub positions: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct SimSettings {
    /// Fixed engine step. Sim time advances by exactly this much per tick.
    #[serde(default = "default_tick_ms")]
    pub tick_ms: u64,
    /// Seed for sensor noise, so runs are reproducible.
    #[serde(default = "default_seed")]
    pub seed: u64,
    /// An MM within this distance of a machine's dock is "at" that machine.
    #[serde(default = "default_dock_tolerance_m")]
    pub dock_tolerance_m: f64,
    /// Open paused in the app: the clock waits at 0 until Resume. Real-time CLI runs ignore it.
    #[serde(default)]
    pub start_paused: bool,
}

impl Default for SimSettings {
    fn default() -> Self {
        Self {
            tick_ms: default_tick_ms(),
            seed: default_seed(),
            dock_tolerance_m: default_dock_tolerance_m(),
            start_paused: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FloorSpec {
    pub width_m: f64,
    pub height_m: f64,
    #[serde(default = "default_grid_m")]
    pub grid_m: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSpec {
    pub label: Option<String>,
    /// Centre of the footprint.
    pub position: [f64; 2],
    pub size: [f64; 2],
    /// Where an MM parks to service the machine. Default: centred, just below the footprint.
    pub dock: Option<[f64; 2]>,
    #[serde(default)]
    pub sensors: BTreeMap<String, SensorSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MmSpec {
    pub label: Option<String>,
    /// Start position. Exactly one of `start` / `start_at` must be given.
    pub start: Option<[f64; 2]>,
    /// Start parked at this machine's dock.
    pub start_at: Option<String>,
    #[serde(default)]
    pub start_heading_deg: f64,
    #[serde(default = "default_max_speed_mps")]
    pub max_speed_mps: f64,
    #[serde(default = "default_radius_m")]
    pub radius_m: f64,
    /// Actions this MM can perform. Default: every action in `[actions]`.
    pub actions: Option<Vec<String>>,
    #[serde(default)]
    pub sensors: BTreeMap<String, SensorSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorSpec {
    /// Sensor type, e.g. "vibration". Defaults to the table key.
    pub kind: Option<String>,
    pub label: Option<String>,
    #[serde(default)]
    pub unit: String,
    pub default: f64,
    /// `[min, max]` of the value (slider range, readings are clamped to it).
    pub range: [f64; 2],
    #[serde(default = "default_period_ms")]
    pub period_ms: u64,
    #[serde(default)]
    pub noise_std: f64,
    /// Fixed `[x, y]`: only for free-standing sensors (top-level `[sensors.<id>]`).
    pub position: Option<[f64; 2]>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSpec {
    pub label: Option<String>,
    /// Only allowed while the MM is docked at a machine.
    #[serde(default)]
    pub requires_machine: bool,
    /// Allowed while the MM is moving (otherwise it must be stopped).
    #[serde(default)]
    pub allowed_while_moving: bool,
}

fn default_name() -> String {
    "Untitled scenario".to_string()
}
fn default_tick_ms() -> u64 {
    20
}
fn default_seed() -> u64 {
    42
}
fn default_dock_tolerance_m() -> f64 {
    0.3
}
fn default_grid_m() -> f64 {
    1.0
}
fn default_max_speed_mps() -> f64 {
    0.5
}
fn default_radius_m() -> f64 {
    0.35
}
fn default_period_ms() -> u64 {
    1000
}
