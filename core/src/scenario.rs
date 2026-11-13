//! Scenario files: the TOML schema (`*Spec`), loading, validation, and the resolved [`Scenario`].
//!
//! Every table uses `deny_unknown_fields` so a typo is an error instead of a silently ignored key.

use crate::event::{EVENT_KINDS, SimEvent};
use crate::geometry::{Rect, Vec2};
use crate::scene::{SceneDef, SceneSpec, resolve_scenes};
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Distance between a machine's footprint and its default dock point.
const DEFAULT_DOCK_GAP_M: f64 = 0.5;

// ---------------------------------------------------------------------------------------------
// File schema
// ---------------------------------------------------------------------------------------------

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
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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
}

impl Default for SimSettings {
    fn default() -> Self {
        Self {
            tick_ms: default_tick_ms(),
            seed: default_seed(),
            dock_tolerance_m: default_dock_tolerance_m(),
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSpec {
    /// How often a moving MM reports its position (sim time).
    #[serde(default = "default_position_interval_ms")]
    pub position_interval_ms: u64,
    #[serde(default)]
    pub sinks: BTreeMap<String, SinkKind>,
}

impl Default for OutputSpec {
    fn default() -> Self {
        Self {
            position_interval_ms: default_position_interval_ms(),
            sinks: BTreeMap::new(),
        }
    }
}

/// Where the event stream goes, besides the UI.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SinkKind {
    /// One JSON line per event on stdout.
    Stdout,
    /// One JSON line per event in a file. Relative paths are resolved against the scenario file.
    Jsonl {
        path: PathBuf,
        #[serde(default = "default_true")]
        append: bool,
    },
    /// Listen on `bind` (e.g. `127.0.0.1:9000`) and stream every event to each connected client
    /// as a length-prefixed JSON frame.
    Tcp { bind: String },
    /// Publish each event, as JSON, to every topic it matches on the MQTT broker at `broker`
    /// (`host:port` or `mqtt://host:port`; the port defaults to 1883).
    Mqtt {
        broker: String,
        #[serde(default)]
        topics: Vec<MqttTopic>,
        /// 0, 1 or 2.
        #[serde(default)]
        qos: u8,
        /// Defaults to `shopfloor-<sink name>-<process id>`.
        #[serde(default)]
        client_id: Option<String>,
    },
}

impl SinkKind {
    /// Streams to consumers as events happen (TCP, MQTT), so it's only useful in real time.
    pub fn is_live(&self) -> bool {
        matches!(self, SinkKind::Tcp { .. } | SinkKind::Mqtt { .. })
    }
}

/// An MQTT topic and the events it carries. An empty filter matches everything.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MqttTopic {
    pub topic: String,
    /// Event kinds, e.g. `["sensor", "sensor_set"]`.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Values of the event's `device_id` field (machine, MM or free-standing sensor ids). Events
    /// without one (`sim`, `scene`) only match topics with no entity filter.
    #[serde(default)]
    pub entities: Vec<String>,
}

impl MqttTopic {
    pub fn matches(&self, event: &SimEvent) -> bool {
        let kind_ok = self.kinds.is_empty() || self.kinds.iter().any(|kind| kind == event.kind());
        let entity_ok = self.entities.is_empty()
            || event
                .device_id()
                .is_some_and(|device_id| self.entities.iter().any(|id| id == device_id));
        kind_ok && entity_ok
    }
}

/// Host and port of an MQTT broker address: `host:port`, `mqtt://host:port` or just `host`
/// (port 1883).
pub fn parse_broker(broker: &str) -> Result<(String, u16), String> {
    let address = broker.trim();
    let address = address.strip_prefix("mqtt://").unwrap_or(address);
    let (host, port) = match address.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or_else(|| format!("'{broker}' has an invalid port"))?;
            (host, port)
        }
        None => (address, 1883),
    };
    if host.is_empty() || host.contains('/') {
        return Err(format!("'{broker}' is not a broker address (host:port)"));
    }
    Ok((host.to_string(), port))
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
fn default_position_interval_ms() -> u64 {
    100
}
fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------------------------
// Resolved model (validated; also what the UI receives as the static layout)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
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
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct MachineDef {
    pub id: String,
    pub label: String,
    pub position: Vec2,
    pub size: Vec2,
    pub dock: Vec2,
    /// Ids of the sensors attached to this machine.
    pub sensors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct ActionDef {
    pub id: String,
    pub label: String,
    pub requires_machine: bool,
    pub allowed_while_moving: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Machine,
    MobileManipulator,
    /// A free-standing sensor: it is its own entity, at a fixed position.
    Floor,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct OutputSettings {
    pub position_interval_ms: u64,
    pub sinks: Vec<SinkDef>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SinkDef {
    pub name: String,
    #[serde(flatten)]
    pub kind: SinkKind,
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
}

// ---------------------------------------------------------------------------------------------
// Loading and validation
// ---------------------------------------------------------------------------------------------

pub fn load_scenario(path: &Path) -> anyhow::Result<Scenario> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read scenario file {}", path.display()))?;
    parse_scenario(&text, Some(path))
        .with_context(|| format!("Failed to load scenario {}", path.display()))
}

/// Parses and validates scenario TOML. `source_path` is used to resolve relative sink paths.
pub fn parse_scenario(text: &str, source_path: Option<&Path>) -> anyhow::Result<Scenario> {
    let spec: ScenarioSpec = toml::from_str(text)?;
    resolve(spec, source_path)
}

fn resolve(spec: ScenarioSpec, source_path: Option<&Path>) -> anyhow::Result<Scenario> {
    let mut errors = Vec::new();
    let sim = spec.sim;

    if sim.tick_ms == 0 || sim.tick_ms > 1000 {
        errors.push(format!("sim.tick_ms must be in 1..=1000 (got {})", sim.tick_ms));
    }
    if !(sim.dock_tolerance_m.is_finite() && sim.dock_tolerance_m >= 0.0) {
        errors.push("sim.dock_tolerance_m must be >= 0".to_string());
    }

    let floor = Floor {
        width_m: spec.floor.width_m,
        height_m: spec.floor.height_m,
        grid_m: spec.floor.grid_m,
    };
    if !(positive(floor.width_m) && positive(floor.height_m)) {
        bail!("floor.width_m and floor.height_m must be > 0");
    }
    if !positive(floor.grid_m) {
        errors.push("floor.grid_m must be > 0".to_string());
    }
    let floor_rect = floor.rect();

    // Ids must be unique across all entities: machines, MMs and floor sensors share one namespace.
    let mut seen_ids = HashSet::new();
    for id in spec
        .machines
        .keys()
        .chain(spec.mobile_manipulators.keys())
        .chain(spec.sensors.keys())
    {
        if !valid_id(id) {
            errors.push(format!(
                "id '{id}' may only contain letters, digits, '_' and '-'"
            ));
        }
        if !seen_ids.insert(id.as_str()) {
            errors.push(format!(
                "id '{id}' is used more than once (machines, mobile manipulators and floor \
                 sensors share one namespace)"
            ));
        }
    }

    let mut actions = Vec::new();
    for (id, action) in &spec.actions {
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

    let largest_radius = spec
        .mobile_manipulators
        .values()
        .map(|mm| mm.radius_m)
        .filter(|radius| radius.is_finite())
        .fold(0.0, f64::max);

    let mut sensors = Vec::new();

    let mut machines = Vec::new();
    for (id, machine) in &spec.machines {
        let position = Vec2::from(machine.position);
        let size = Vec2::from(machine.size);
        if !(positive(size.x) && positive(size.y)) {
            errors.push(format!("machines.{id}.size must be > 0"));
        }
        let footprint = Rect::from_center(position, size);
        if !floor_rect.contains_rect(footprint) {
            errors.push(format!("machines.{id} footprint lies outside the floor"));
        }
        let dock = machine.dock.map(Vec2::from).unwrap_or_else(|| {
            Vec2::new(position.x, position.y - size.y / 2.0 - DEFAULT_DOCK_GAP_M)
        });
        if !floor_rect.inset(largest_radius).contains(dock) {
            errors.push(format!(
                "machines.{id}.dock ({:.2}, {:.2}) is not reachable (outside the floor, \
                 allowing for MM radius {largest_radius})",
                dock.x, dock.y
            ));
        }
        let sensor_ids = collect_mounted_sensors(
            id,
            HostKind::Machine,
            &machine.sensors,
            &sim,
            &mut sensors,
            &mut errors,
        );
        machines.push(MachineDef {
            id: id.clone(),
            label: machine.label.clone().unwrap_or_else(|| id.clone()),
            position,
            size,
            dock,
            sensors: sensor_ids,
        });
    }

    let mut mobile_manipulators = Vec::new();
    for (id, mm) in &spec.mobile_manipulators {
        if !positive(mm.max_speed_mps) {
            errors.push(format!("mobile_manipulators.{id}.max_speed_mps must be > 0"));
        }
        if !positive(mm.radius_m) {
            errors.push(format!("mobile_manipulators.{id}.radius_m must be > 0"));
        }
        let start = match (&mm.start, &mm.start_at) {
            (Some(start), None) => Some(Vec2::from(*start)),
            (None, Some(machine_id)) => match machines.iter().find(|m| &m.id == machine_id) {
                Some(machine) => Some(machine.dock),
                None => {
                    errors.push(format!(
                        "mobile_manipulators.{id}.start_at names unknown machine '{machine_id}'"
                    ));
                    None
                }
            },
            _ => {
                errors.push(format!(
                    "mobile_manipulators.{id} needs exactly one of `start` or `start_at`"
                ));
                None
            }
        };
        let start = start.unwrap_or_default();
        if positive(mm.radius_m) && !floor_rect.inset(mm.radius_m).contains(start) {
            errors.push(format!(
                "mobile_manipulators.{id} starts outside the floor ({:.2}, {:.2})",
                start.x, start.y
            ));
        }
        let sensor_ids = collect_mounted_sensors(
            id,
            HostKind::MobileManipulator,
            &mm.sensors,
            &sim,
            &mut sensors,
            &mut errors,
        );
        let mm_actions = match &mm.actions {
            None => actions.iter().map(|action| action.id.clone()).collect(),
            Some(list) => {
                let mut seen = HashSet::new();
                for action_id in list {
                    if !spec.actions.contains_key(action_id) {
                        errors.push(format!(
                            "mobile_manipulators.{id}.actions names unknown action '{action_id}' \
                             (known: {})",
                            spec.actions.keys().cloned().collect::<Vec<_>>().join(", ")
                        ));
                    }
                    if !seen.insert(action_id) {
                        errors.push(format!(
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

    for (id, spec) in &spec.sensors {
        let def = resolve_sensor(id.clone(), id, id, HostKind::Floor, spec, &sim, &mut errors);
        match spec.position.map(Vec2::from) {
            None => errors.push(format!(
                "sensor '{id}': free-standing sensors need a `position = [x, y]`"
            )),
            Some(position) if !floor_rect.contains(position) => errors.push(format!(
                "sensor '{id}': position ({:.2}, {:.2}) is outside the floor",
                position.x, position.y
            )),
            Some(_) => {}
        }
        sensors.push(def);
    }

    let output = &spec.output;
    if output.position_interval_ms < sim.tick_ms {
        errors.push(format!(
            "output.position_interval_ms ({}) must be >= sim.tick_ms ({})",
            output.position_interval_ms, sim.tick_ms
        ));
    }
    let base_dir = source_path.and_then(Path::parent);
    let mut sinks = Vec::new();
    for (name, kind) in &output.sinks {
        let kind = match kind {
            SinkKind::Stdout => SinkKind::Stdout,
            SinkKind::Tcp { bind } => {
                if bind.trim().is_empty() {
                    errors.push(format!("output.sinks.{name}.bind must not be empty"));
                }
                SinkKind::Tcp { bind: bind.clone() }
            }
            SinkKind::Jsonl { path, append } => {
                if path.as_os_str().is_empty() {
                    errors.push(format!("output.sinks.{name}.path must not be empty"));
                }
                let path = match base_dir {
                    Some(base) if path.is_relative() => base.join(path),
                    _ => path.clone(),
                };
                SinkKind::Jsonl {
                    path,
                    append: *append,
                }
            }
            SinkKind::Mqtt {
                broker,
                topics,
                qos,
                client_id,
            } => {
                check_mqtt_sink(name, broker, topics, *qos, &seen_ids, &mut errors);
                SinkKind::Mqtt {
                    broker: broker.clone(),
                    topics: topics.clone(),
                    qos: *qos,
                    client_id: client_id.clone(),
                }
            }
        };
        sinks.push(SinkDef {
            name: name.clone(),
            kind,
        });
    }

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
        output: OutputSettings {
            position_interval_ms: output.position_interval_ms,
            sinks,
        },
    };
    // Scenes refer to everything above, so they are checked against the resolved scenario.
    scenario.scenes = resolve_scenes(&spec.scenes, &scenario, &mut errors);

    if !errors.is_empty() {
        bail!("Invalid scenario:\n  - {}", errors.join("\n  - "));
    }
    Ok(scenario)
}

fn check_mqtt_sink(
    name: &str,
    broker: &str,
    topics: &[MqttTopic],
    qos: u8,
    entity_ids: &HashSet<&str>,
    errors: &mut Vec<String>,
) {
    let at = format!("output.sinks.{name}");
    if let Err(error) = parse_broker(broker) {
        errors.push(format!("{at}.broker: {error}"));
    }
    if qos > 2 {
        errors.push(format!("{at}.qos must be 0, 1 or 2 (got {qos})"));
    }
    if topics.is_empty() {
        errors.push(format!(
            "{at} must list at least one topic ([[{at}.topics]] with a `topic`)"
        ));
    }
    let mut seen = HashSet::new();
    for topic in topics {
        let name = &topic.topic;
        if name.is_empty() {
            errors.push(format!("{at}.topics: a topic name must not be empty"));
        } else if name.contains(['+', '#', '\0']) {
            errors.push(format!(
                "{at}.topics: '{name}' contains a wildcard ('+' or '#'), which only \
                 subscribers may use"
            ));
        }
        if !seen.insert(name) {
            errors.push(format!("{at}.topics: '{name}' is listed more than once"));
        }
        for kind in &topic.kinds {
            if !EVENT_KINDS.contains(&kind.as_str()) {
                errors.push(format!(
                    "{at}.topics: '{name}' names unknown event kind '{kind}' (known: {})",
                    EVENT_KINDS.join(", ")
                ));
            }
        }
        for entity in &topic.entities {
            if !entity_ids.contains(entity.as_str()) {
                errors.push(format!(
                    "{at}.topics: '{name}' names unknown entity '{entity}' (machines, mobile \
                     manipulators and free-standing sensors)"
                ));
            }
        }
    }
}

/// Sensors nested under a machine or MM: id `<host>.<name>`, no `position` of their own.
fn collect_mounted_sensors(
    host: &str,
    host_kind: HostKind,
    specs: &BTreeMap<String, SensorSpec>,
    sim: &SimSettings,
    sensors: &mut Vec<SensorDef>,
    errors: &mut Vec<String>,
) -> Vec<String> {
    let mut ids = Vec::new();
    for (name, spec) in specs {
        let id = format!("{host}.{name}");
        if spec.position.is_some() {
            errors.push(format!(
                "sensor '{id}': `position` is only for free-standing sensors \
                 (a mounted sensor is where its host is)"
            ));
        }
        sensors.push(resolve_sensor(id.clone(), name, host, host_kind, spec, sim, errors));
        ids.push(id);
    }
    ids
}

/// Validates the parts of a sensor spec common to mounted and floor sensors.
fn resolve_sensor(
    id: String,
    name: &str,
    host: &str,
    host_kind: HostKind,
    spec: &SensorSpec,
    sim: &SimSettings,
    errors: &mut Vec<String>,
) -> SensorDef {
    if !valid_id(name) {
        errors.push(format!(
            "sensor '{id}': name may only contain letters, digits, '_' and '-'"
        ));
    }
    let [min, max] = spec.range;
    if !(min.is_finite() && max.is_finite() && min < max) {
        errors.push(format!("sensor '{id}': range must be [min, max] with min < max"));
    } else if !(spec.default >= min && spec.default <= max) {
        errors.push(format!(
            "sensor '{id}': default {} is outside range [{min}, {max}]",
            spec.default
        ));
    }
    if spec.period_ms < sim.tick_ms {
        errors.push(format!(
            "sensor '{id}': period_ms ({}) must be >= sim.tick_ms ({})",
            spec.period_ms, sim.tick_ms
        ));
    }
    if !(spec.noise_std.is_finite() && spec.noise_std >= 0.0) {
        errors.push(format!("sensor '{id}': noise_std must be >= 0"));
    }
    let kind = spec.kind.clone().unwrap_or_else(|| name.to_string());
    if kind.trim().is_empty() {
        errors.push(format!("sensor '{id}': kind must not be empty"));
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

/// The `event` values of `move_command`, `dock` and `undock`, so no action may use them.
const RESERVED_ACTION_IDS: &[&str] = &["move", "dock", "undock"];

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        name = "Test cell"

        [floor]
        width_m = 10.0
        height_m = 6.0

        [machines.machine1]
        position = [2.0, 4.5]
        size = [2.0, 1.0]

        [machines.machine2]
        label = "Press"
        position = [8.0, 4.5]
        size = [2.0, 1.0]
        dock = [8.0, 3.0]

        [mobile_manipulators.mm1]
        start_at = "machine1"
        max_speed_mps = 1.0

        [mobile_manipulators.mm1.sensors.vibration]
        unit = "g"
        default = 0.1
        range = [0.0, 2.0]
        period_ms = 500
    "#;

    fn errors_of(text: &str) -> String {
        format!("{:#}", parse_scenario(text, None).unwrap_err())
    }

    #[test]
    fn parses_minimal_scenario_with_defaults() {
        let scenario = parse_scenario(MINIMAL, None).unwrap();
        assert_eq!(scenario.name, "Test cell");
        assert_eq!(scenario.sim, SimSettings::default());
        assert_eq!(scenario.machines.len(), 2);
        assert_eq!(scenario.machines[0].label, "machine1");
        // Default dock: centred, 0.5 m below the footprint.
        assert_eq!(scenario.machines[0].dock, Vec2::new(2.0, 3.5));
        assert_eq!(scenario.machines[1].dock, Vec2::new(8.0, 3.0));

        let mm = &scenario.mobile_manipulators[0];
        assert_eq!(mm.start, Vec2::new(2.0, 3.5));
        assert_eq!(mm.radius_m, 0.35);
        assert_eq!(mm.sensors, vec!["mm1.vibration".to_string()]);

        let sensor = &scenario.sensors[0];
        assert_eq!(sensor.id, "mm1.vibration");
        assert_eq!(sensor.kind, "vibration");
        assert_eq!(sensor.label, "Vibration");
        assert_eq!(sensor.host, "mm1");
        assert_eq!(sensor.host_kind, HostKind::MobileManipulator);
        assert_eq!((sensor.min, sensor.max), (0.0, 2.0));
        assert_eq!(scenario.output.position_interval_ms, 100);
    }

    #[test]
    fn rejects_unknown_keys() {
        let text = MINIMAL.replace("max_speed_mps = 1.0", "max_sped_mps = 1.0");
        assert!(errors_of(&text).contains("max_sped_mps"));
    }

    #[test]
    fn rejects_shared_ids_between_machines_and_mms() {
        let text = format!(
            "{MINIMAL}\n[mobile_manipulators.machine2]\nstart = [5.0, 1.0]\n"
        );
        assert!(errors_of(&text).contains("id 'machine2' is used more than once"));
    }

    #[test]
    fn rejects_bad_sensor_ranges_and_periods() {
        let text = MINIMAL
            .replace("default = 0.1", "default = 5.0")
            .replace("period_ms = 500", "period_ms = 5");
        let errors = errors_of(&text);
        assert!(errors.contains("default 5 is outside range"), "{errors}");
        assert!(errors.contains("period_ms (5) must be >= sim.tick_ms (20)"), "{errors}");
    }

    #[test]
    fn rejects_unknown_start_machine_and_out_of_bounds_layout() {
        let text = MINIMAL
            .replace("start_at = \"machine1\"", "start_at = \"machine9\"")
            .replace("position = [8.0, 4.5]", "position = [9.5, 4.5]");
        let errors = errors_of(&text);
        assert!(errors.contains("unknown machine 'machine9'"), "{errors}");
        assert!(errors.contains("machines.machine2 footprint lies outside"), "{errors}");
    }

    #[test]
    fn requires_exactly_one_start() {
        let text = MINIMAL.replace(
            "start_at = \"machine1\"",
            "start_at = \"machine1\"\nstart = [1.0, 1.0]",
        );
        assert!(errors_of(&text).contains("exactly one of `start` or `start_at`"));
    }

    #[test]
    fn resolves_relative_sink_paths_against_scenario_file() {
        let text = format!(
            "{MINIMAL}\n[output.sinks.rec]\ntype = \"jsonl\"\npath = \"../recordings/a.jsonl\"\n\n[output.sinks.term]\ntype = \"stdout\"\n"
        );
        let scenario = parse_scenario(&text, Some(Path::new("/x/scenarios/s.toml"))).unwrap();
        assert_eq!(
            scenario.output.sinks,
            vec![
                SinkDef {
                    name: "rec".into(),
                    kind: SinkKind::Jsonl {
                        path: PathBuf::from("/x/scenarios/../recordings/a.jsonl"),
                        append: true,
                    },
                },
                SinkDef {
                    name: "term".into(),
                    kind: SinkKind::Stdout,
                },
            ]
        );
    }

    #[test]
    fn parses_tcp_sinks() {
        let text = format!("{MINIMAL}\n[output.sinks.tcp]\ntype = \"tcp\"\nbind = \"127.0.0.1:9000\"\n");
        let scenario = parse_scenario(&text, None).unwrap();
        assert_eq!(
            scenario.output.sinks,
            vec![SinkDef {
                name: "tcp".into(),
                kind: SinkKind::Tcp {
                    bind: "127.0.0.1:9000".into()
                },
            }]
        );
    }

    #[test]
    fn rejects_unknown_sink_type() {
        let text = format!("{MINIMAL}\n[output.sinks.x]\ntype = \"carrier_pigeon\"\n");
        assert!(errors_of(&text).contains("carrier_pigeon"));
    }

    #[test]
    fn parses_mqtt_sinks() {
        let text = format!(
            "{MINIMAL}
[output.sinks.mqtt]
type = \"mqtt\"
broker = \"mqtt://127.0.0.1:1883\"

[[output.sinks.mqtt.topics]]
topic = \"shopfloor/sensors\"
kinds = [\"sensor\", \"sensor_set\"]

[[output.sinks.mqtt.topics]]
topic = \"shopfloor/mm1\"
entities = [\"mm1\"]
"
        );
        let scenario = parse_scenario(&text, None).unwrap();
        assert_eq!(
            scenario.output.sinks,
            vec![SinkDef {
                name: "mqtt".into(),
                kind: SinkKind::Mqtt {
                    broker: "mqtt://127.0.0.1:1883".into(),
                    topics: vec![
                        MqttTopic {
                            topic: "shopfloor/sensors".into(),
                            kinds: vec!["sensor".into(), "sensor_set".into()],
                            entities: vec![],
                        },
                        MqttTopic {
                            topic: "shopfloor/mm1".into(),
                            kinds: vec![],
                            entities: vec!["mm1".into()],
                        },
                    ],
                    qos: 0,
                    client_id: None,
                },
            }]
        );
        assert!(scenario.output.sinks[0].kind.is_live());
    }

    #[test]
    fn rejects_invalid_mqtt_sinks() {
        let topic = "[[output.sinks.m.topics]]\ntopic = \"t\"\n";
        let cases = [
            (format!("broker = \"localhost:99999\"\n{topic}"), "invalid port"),
            (format!("broker = \"mqtt://\"\n{topic}"), "not a broker address"),
            (format!("broker = \"localhost\"\nqos = 3\n{topic}"), "qos must be 0, 1 or 2"),
            ("broker = \"localhost\"\n".to_string(), "must list at least one topic"),
            (
                "broker = \"localhost\"\n[[output.sinks.m.topics]]\ntopic = \"a/#\"\n".to_string(),
                "'a/#' contains a wildcard",
            ),
            (format!("broker = \"localhost\"\n{topic}{topic}"), "'t' is listed more than once"),
            (
                format!("broker = \"localhost\"\n{topic}kinds = [\"sensors\"]\n"),
                "unknown event kind 'sensors'",
            ),
            (
                format!("broker = \"localhost\"\n{topic}entities = [\"mm9\"]\n"),
                "unknown entity 'mm9'",
            ),
        ];
        for (sink, expected) in cases {
            let text = format!("{MINIMAL}\n[output.sinks.m]\ntype = \"mqtt\"\n{sink}");
            let errors = errors_of(&text);
            assert!(errors.contains(expected), "expected {expected:?} in:\n{errors}");
        }
    }

    #[test]
    fn parses_broker_addresses() {
        assert_eq!(parse_broker("localhost"), Ok(("localhost".into(), 1883)));
        assert_eq!(parse_broker("mqtt://10.0.0.5:1884"), Ok(("10.0.0.5".into(), 1884)));
        assert_eq!(parse_broker("broker.local:8883"), Ok(("broker.local".into(), 8883)));
        assert!(parse_broker("localhost:0").is_err());
        assert!(parse_broker("").is_err());
    }

    #[test]
    fn machine_docked_at_uses_tolerance() {
        let scenario = parse_scenario(MINIMAL, None).unwrap();
        assert_eq!(
            scenario.machine_docked_at(Vec2::new(2.1, 3.6)).map(|m| m.id.as_str()),
            Some("machine1")
        );
        assert!(scenario.machine_docked_at(Vec2::new(5.0, 3.5)).is_none());
    }

    #[test]
    fn actions_default_to_all_and_can_be_limited_per_mm() {
        let text = format!(
            "{MINIMAL}\n[actions.pick]\nrequires_machine = true\n\n[actions.scan]\nlabel = \"Scan area\"\nallowed_while_moving = true\n\n[mobile_manipulators.mm2]\nstart = [5.0, 1.0]\nactions = [\"scan\"]\n"
        );
        let scenario = parse_scenario(&text, None).unwrap();
        let pick = scenario.action("pick").unwrap();
        assert_eq!(pick.label, "Pick");
        assert!(pick.requires_machine && !pick.allowed_while_moving);
        let scan = scenario.action("scan").unwrap();
        assert_eq!(scan.label, "Scan area");
        assert!(!scan.requires_machine && scan.allowed_while_moving);
        assert_eq!(scenario.mobile_manipulators[0].actions, vec!["pick", "scan"]);
        assert_eq!(scenario.mobile_manipulators[1].actions, vec!["scan"]);
    }

    #[test]
    fn rejects_unknown_or_duplicate_mm_actions() {
        let text = MINIMAL.replace(
            "max_speed_mps = 1.0",
            "max_speed_mps = 1.0\nactions = [\"pick\", \"pick\", \"fly\"]",
        );
        let text = format!("{text}\n[actions.pick]\n");
        let errors = errors_of(&text);
        assert!(errors.contains("unknown action 'fly' (known: pick)"), "{errors}");
        assert!(errors.contains("lists 'pick' twice"), "{errors}");
    }

    #[test]
    fn rejects_reserved_action_ids() {
        let errors = errors_of(&format!("{MINIMAL}\n[actions.dock]\n\n[actions.pick]\n"));
        assert!(errors.contains("action 'dock' is reserved"), "{errors}");
        assert!(!errors.contains("'pick'"), "{errors}");
    }

    #[test]
    fn parses_free_standing_sensors() {
        let text = format!(
            "{MINIMAL}\n[sensors.hall_temp]\nkind = \"temperature\"\nunit = \"C\"\nposition = [4.0, 5.0]\ndefault = 22.0\nrange = [0.0, 50.0]\n"
        );
        let scenario = parse_scenario(&text, None).unwrap();
        let sensor = scenario.sensors.iter().find(|s| s.id == "hall_temp").unwrap();
        assert_eq!(sensor.host, "hall_temp");
        assert_eq!(sensor.host_kind, HostKind::Floor);
        assert_eq!(sensor.kind, "temperature");
        assert_eq!(sensor.label, "Hall temp");
        assert_eq!(sensor.position, Some(Vec2::new(4.0, 5.0)));
        // Mounted sensors have no position of their own.
        assert_eq!(scenario.sensors[0].position, None);
    }

    #[test]
    fn rejects_bad_sensor_positions() {
        let text = format!(
            "{MINIMAL}\n[sensors.nowhere]\ndefault = 1.0\nrange = [0.0, 2.0]\n\n[sensors.outside]\nposition = [11.0, 1.0]\ndefault = 1.0\nrange = [0.0, 2.0]\n\n[sensors.machine1]\nposition = [1.0, 1.0]\ndefault = 1.0\nrange = [0.0, 2.0]\n"
        )
        .replace("period_ms = 500", "period_ms = 500\nposition = [1.0, 1.0]");
        let errors = errors_of(&text);
        assert!(errors.contains("sensor 'nowhere': free-standing sensors need a `position"), "{errors}");
        assert!(errors.contains("sensor 'outside': position (11.00, 1.00) is outside the floor"), "{errors}");
        assert!(errors.contains("id 'machine1' is used more than once"), "{errors}");
        assert!(errors.contains("sensor 'mm1.vibration': `position` is only for free-standing"), "{errors}");
    }

    #[test]
    fn bundled_scenarios_are_valid() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scenarios");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                load_scenario(&path).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
                count += 1;
            }
        }
        assert!(count > 0, "no scenarios found in {}", dir.display());
    }
}
