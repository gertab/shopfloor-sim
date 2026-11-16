//! Checking a scenario file and resolving it into a [`Scenario`]. Every problem found is reported,
//! all together.

use super::outputs::resolve_output;
use super::spec::{
    ActionSpec, FloorSpec, MachineSpec, MmSpec, ReplaySettings, ScenarioSpec, SensorSpec, SimSettings,
};
use super::{ActionDef, Floor, HostKind, MachineDef, MmDef, Scenario, SensorDef};
use crate::geometry::{Rect, Vec2};
use crate::scene::resolve_scenes;
use anyhow::bail;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Distance between a machine's footprint and its default dock point.
const DEFAULT_DOCK_GAP_M: f64 = 0.5;

/// The `event` values of `move_command`, `dock` and `undock`, so no action may use them.
const RESERVED_ACTION_IDS: &[&str] = &["move", "dock", "undock"];

pub(super) fn resolve(spec: ScenarioSpec, source_path: Option<&Path>) -> anyhow::Result<Scenario> {
    let mut errors = Vec::new();
    let sim = spec.sim;
    check_sim(&sim, &mut errors);
    let floor = resolve_floor(&spec.floor, &mut errors)?;
    let entity_ids = check_ids(
        &spec.machines,
        &spec.mobile_manipulators,
        &spec.sensors,
        &mut errors,
    );
    let actions = resolve_actions(&spec.actions, &mut errors);

    let mut resolver = Resolver {
        sim,
        floor: floor.rect(),
        sensors: Vec::new(),
        errors,
    };
    let machines = resolver.machines(&spec.machines, &spec.mobile_manipulators);
    let mobile_manipulators =
        resolver.mobile_manipulators(&spec.mobile_manipulators, &spec.actions, &actions, &machines);
    resolver.floor_sensors(&spec.sensors);
    let Resolver {
        sim,
        sensors,
        mut errors,
        ..
    } = resolver;

    let base_dir = source_path.and_then(Path::parent);
    let output = resolve_output(&spec.output, &sim, base_dir, &entity_ids, &mut errors);
    let replay = spec
        .replay
        .map(|replay| resolve_replay(replay, base_dir, &mut errors));

    let mut scenario = Scenario {
        name: spec.name,
        source_path: source_path.map(Path::to_path_buf),
        sim,
        floor,
        machines,
        mobile_manipulators,
        sensors,
        actions,
        scenes: Vec::new(),
        output,
        replay,
    };
    // Scenes refer to everything above, so they are checked against the resolved scenario.
    scenario.scenes = resolve_scenes(&spec.scenes, &scenario, &mut errors);

    if !errors.is_empty() {
        bail!("Invalid scenario:\n  - {}", errors.join("\n  - "));
    }
    Ok(scenario)
}

fn check_sim(sim: &SimSettings, errors: &mut Vec<String>) {
    if sim.tick_ms == 0 || sim.tick_ms > 1000 {
        errors.push(format!("sim.tick_ms must be in 1..=1000 (got {})", sim.tick_ms));
    }
    if !(sim.dock_tolerance_m.is_finite() && sim.dock_tolerance_m >= 0.0) {
        errors.push("sim.dock_tolerance_m must be >= 0".to_string());
    }
}

/// The floor's size is checked first: nothing else can be placed without it.
fn resolve_floor(spec: &FloorSpec, errors: &mut Vec<String>) -> anyhow::Result<Floor> {
    let floor = Floor {
        width_m: spec.width_m,
        height_m: spec.height_m,
        grid_m: spec.grid_m,
    };
    if !(positive(floor.width_m) && positive(floor.height_m)) {
        bail!("floor.width_m and floor.height_m must be > 0");
    }
    if !positive(floor.grid_m) {
        errors.push("floor.grid_m must be > 0".to_string());
    }
    Ok(floor)
}

/// Ids must be unique across all entities: machines, MMs and floor sensors share one namespace.
/// Returns them all.
fn check_ids<'a>(
    machines: &'a BTreeMap<String, MachineSpec>,
    mms: &'a BTreeMap<String, MmSpec>,
    sensors: &'a BTreeMap<String, SensorSpec>,
    errors: &mut Vec<String>,
) -> HashSet<&'a str> {
    let mut seen_ids = HashSet::new();
    for id in machines.keys().chain(mms.keys()).chain(sensors.keys()) {
        if !valid_id(id) {
            errors.push(format!("id '{id}' may only contain letters, digits, '_' and '-'"));
        }
        if !seen_ids.insert(id.as_str()) {
            errors.push(format!(
                "id '{id}' is used more than once (machines, mobile manipulators and floor \
                 sensors share one namespace)"
            ));
        }
    }
    seen_ids
}

fn resolve_actions(specs: &BTreeMap<String, ActionSpec>, errors: &mut Vec<String>) -> Vec<ActionDef> {
    let mut actions = Vec::new();
    for (id, action) in specs {
        if !valid_id(id) {
            errors.push(format!(
                "action '{id}' may only contain letters, digits, '_' and '-'"
            ));
        }
        if RESERVED_ACTION_IDS.contains(&id.as_str()) {
            errors.push(format!(
                "action '{id}' is reserved: an action's id is its event's `event`, which is also \
                 \"move\", \"dock\" or \"undock\" for those events"
            ));
        }
        actions.push(ActionDef {
            id: id.clone(),
            label: action.label.clone().unwrap_or_else(|| humanize(id)),
            requires_machine: action.requires_machine,
            allowed_while_moving: action.allowed_while_moving,
        });
    }
    actions
}

/// `[replay]`: trace paths are resolved against the scenario file's folder.
fn resolve_replay(
    replay: ReplaySettings,
    base_dir: Option<&Path>,
    errors: &mut Vec<String>,
) -> ReplaySettings {
    if replay.traces.is_empty() {
        errors.push("replay.traces must list at least one trace file".to_string());
    }
    ReplaySettings {
        traces: replay
            .traces
            .iter()
            .map(|path| resolve_path(base_dir, path))
            .collect(),
        ..replay
    }
}

/// A path from the scenario file: relative ones are taken from the file's folder.
pub(super) fn resolve_path(base_dir: Option<&Path>, path: &Path) -> PathBuf {
    match base_dir {
        Some(base) if path.is_relative() => base.join(path),
        _ => path.to_path_buf(),
    }
}

/// What the machines, MMs and sensors are checked against, and what they add up to.
struct Resolver {
    sim: SimSettings,
    floor: Rect,
    /// Every sensor (mounted and free-standing), in the order they're resolved.
    sensors: Vec<SensorDef>,
    errors: Vec<String>,
}

impl Resolver {
    fn machines(
        &mut self,
        specs: &BTreeMap<String, MachineSpec>,
        mms: &BTreeMap<String, MmSpec>,
    ) -> Vec<MachineDef> {
        let largest_radius = mms
            .values()
            .map(|mm| mm.radius_m)
            .filter(|radius| radius.is_finite())
            .fold(0.0, f64::max);
        let mut machines = Vec::new();
        for (id, machine) in specs {
            let position = Vec2::from(machine.position);
            let size = Vec2::from(machine.size);
            if !(positive(size.x) && positive(size.y)) {
                self.errors.push(format!("machines.{id}.size must be > 0"));
            }
            let footprint = Rect::from_center(position, size);
            if !self.floor.contains_rect(footprint) {
                self.errors
                    .push(format!("machines.{id} footprint lies outside the floor"));
            }
            let dock = machine
                .dock
                .map(Vec2::from)
                .unwrap_or_else(|| Vec2::new(position.x, position.y - size.y / 2.0 - DEFAULT_DOCK_GAP_M));
            if !self.floor.inset(largest_radius).contains(dock) {
                self.errors.push(format!(
                    "machines.{id}.dock ({:.2}, {:.2}) is not reachable (outside the floor, \
                     allowing for MM radius {largest_radius})",
                    dock.x, dock.y
                ));
            }
            let sensor_ids = self.mounted_sensors(id, HostKind::Machine, &machine.sensors);
            machines.push(MachineDef {
                id: id.clone(),
                label: machine.label.clone().unwrap_or_else(|| id.clone()),
                position,
                size,
                dock,
                sensors: sensor_ids,
            });
        }
        machines
    }

    fn mobile_manipulators(
        &mut self,
        specs: &BTreeMap<String, MmSpec>,
        action_specs: &BTreeMap<String, ActionSpec>,
        actions: &[ActionDef],
        machines: &[MachineDef],
    ) -> Vec<MmDef> {
        let mut mobile_manipulators = Vec::new();
        for (id, mm) in specs {
            if !positive(mm.max_speed_mps) {
                self.errors
                    .push(format!("mobile_manipulators.{id}.max_speed_mps must be > 0"));
            }
            if !positive(mm.radius_m) {
                self.errors
                    .push(format!("mobile_manipulators.{id}.radius_m must be > 0"));
            }
            let start = match (&mm.start, &mm.start_at) {
                (Some(start), None) => Some(Vec2::from(*start)),
                (None, Some(machine_id)) => match machines.iter().find(|m| &m.id == machine_id) {
                    Some(machine) => Some(machine.dock),
                    None => {
                        self.errors.push(format!(
                            "mobile_manipulators.{id}.start_at names unknown machine '{machine_id}'"
                        ));
                        None
                    }
                },
                _ => {
                    self.errors.push(format!(
                        "mobile_manipulators.{id} needs exactly one of `start` or `start_at`"
                    ));
                    None
                }
            };
            let start = start.unwrap_or_default();
            if positive(mm.radius_m) && !self.floor.inset(mm.radius_m).contains(start) {
                self.errors.push(format!(
                    "mobile_manipulators.{id} starts outside the floor ({:.2}, {:.2})",
                    start.x, start.y
                ));
            }
            let sensor_ids = self.mounted_sensors(id, HostKind::MobileManipulator, &mm.sensors);
            let mm_actions = match &mm.actions {
                None => actions.iter().map(|action| action.id.clone()).collect(),
                Some(list) => {
                    let mut seen = HashSet::new();
                    for action_id in list {
                        if !action_specs.contains_key(action_id) {
                            self.errors.push(format!(
                                "mobile_manipulators.{id}.actions names unknown action '{action_id}' \
                                 (known: {})",
                                action_specs.keys().cloned().collect::<Vec<_>>().join(", ")
                            ));
                        }
                        if !seen.insert(action_id) {
                            self.errors.push(format!(
                                "mobile_manipulators.{id}.actions lists '{action_id}' twice"
                            ));
                        }
                    }
                    list.clone()
                }
            };
            mobile_manipulators.push(MmDef {
                id: id.clone(),
                label: mm.label.clone().unwrap_or_else(|| id.clone()),
                start,
                start_at: mm.start_at.clone(),
                start_heading_rad: mm.start_heading_deg.to_radians(),
                max_speed_mps: mm.max_speed_mps,
                radius_m: mm.radius_m,
                actions: mm_actions,
                sensors: sensor_ids,
            });
        }
        mobile_manipulators
    }

    /// Free-standing sensors: each at its own `position`, and its own entity.
    fn floor_sensors(&mut self, specs: &BTreeMap<String, SensorSpec>) {
        for (id, spec) in specs {
            let def = self.sensor(id.clone(), id, id, HostKind::Floor, spec);
            match spec.position.map(Vec2::from) {
                None => self.errors.push(format!(
                    "sensor '{id}': free-standing sensors need a `position = [x, y]`"
                )),
                Some(position) if !self.floor.contains(position) => self.errors.push(format!(
                    "sensor '{id}': position ({:.2}, {:.2}) is outside the floor",
                    position.x, position.y
                )),
                Some(_) => {}
            }
            self.sensors.push(def);
        }
    }

    /// Sensors nested under a machine or MM: id `<host>.<name>`, no `position` of their own.
    fn mounted_sensors(
        &mut self,
        host: &str,
        host_kind: HostKind,
        specs: &BTreeMap<String, SensorSpec>,
    ) -> Vec<String> {
        let mut ids = Vec::new();
        for (name, spec) in specs {
            let id = format!("{host}.{name}");
            if spec.position.is_some() {
                self.errors.push(format!(
                    "sensor '{id}': `position` is only for free-standing sensors \
                     (a mounted sensor is where its host is)"
                ));
            }
            let def = self.sensor(id.clone(), name, host, host_kind, spec);
            self.sensors.push(def);
            ids.push(id);
        }
        ids
    }

    /// Checks the parts of a sensor spec common to mounted and floor sensors.
    fn sensor(
        &mut self,
        id: String,
        name: &str,
        host: &str,
        host_kind: HostKind,
        spec: &SensorSpec,
    ) -> SensorDef {
        if !valid_id(name) {
            self.errors.push(format!(
                "sensor '{id}': name may only contain letters, digits, '_' and '-'"
            ));
        }
        let [min, max] = spec.range;
        if !(min.is_finite() && max.is_finite() && min < max) {
            self.errors
                .push(format!("sensor '{id}': range must be [min, max] with min < max"));
        } else if !(spec.default >= min && spec.default <= max) {
            self.errors.push(format!(
                "sensor '{id}': default {} is outside range [{min}, {max}]",
                spec.default
            ));
        }
        if spec.period_ms < self.sim.tick_ms {
            self.errors.push(format!(
                "sensor '{id}': period_ms ({}) must be >= sim.tick_ms ({})",
                spec.period_ms, self.sim.tick_ms
            ));
        }
        if !(spec.noise_std.is_finite() && spec.noise_std >= 0.0) {
            self.errors.push(format!("sensor '{id}': noise_std must be >= 0"));
        }
        let kind = spec.kind.clone().unwrap_or_else(|| name.to_string());
        if kind.trim().is_empty() {
            self.errors.push(format!("sensor '{id}': kind must not be empty"));
        }
        SensorDef {
            id,
            name: name.to_string(),
            host: host.to_string(),
            host_kind,
            kind,
            label: spec.label.clone().unwrap_or_else(|| humanize(name)),
            unit: spec.unit.clone(),
            default: spec.default,
            min,
            max,
            period_ms: spec.period_ms,
            noise_std: spec.noise_std,
            position: spec.position.map(Vec2::from),
        }
    }
}

/// Default display name from an id: `hall_temp` -> `Hall temp`.
fn humanize(id: &str) -> String {
    let text = id.replace(['_', '-'], " ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
