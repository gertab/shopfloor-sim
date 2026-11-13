//! The output event stream. This is the contract with consumers (see `README.md`).
//!
//! Every event is one flat JSON object: an envelope (`v`, `seq`, `timestamp_ms`, `sim_ms`, `kind`)
//! plus the fields of its kind. Measurements are always JSON floats.
//!
//! The keys follow the Liminal Desk traces where they describe the same thing (`device_id`, `event`,
//! `type`, `target`, `object`, `source`, `destination`, `timestamp_ms`), so a rule written for a
//! trace payload also matches the simulator's events (see `docs/EVENT_SCHEMA_V2.md`).

use crate::command::Origin;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimEvent {
    /// Schema version.
    pub v: u32,
    /// Per-run sequence number, +1 per event.
    pub seq: u64,
    /// Wall-clock time, epoch milliseconds.
    pub timestamp_ms: u64,
    /// Simulation time in milliseconds (excludes pauses; resets with the simulation).
    pub sim_ms: u64,
    #[serde(flatten)]
    pub body: EventBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventBody {
    /// A moving MM's pose, every `position_interval_ms`, plus once on arrival.
    Position {
        device_id: String,
        x: f64,
        y: f64,
        heading_rad: f64,
        speed_mps: f64,
    },
    /// An MM was given a new destination.
    MoveCommand {
        device_id: String,
        event: MoveEvent,
        /// The machine the MM is at when the move starts, if any.
        source: Option<String>,
        /// The machine it is sent to (or whose dock the point is at), if any.
        destination: Option<String>,
        command_id: u64,
        origin: Origin,
        from: Point,
        to: Point,
        distance_m: f64,
        eta_ms: u64,
        /// The command this one replaced, if the MM was already moving.
        replaces: Option<u64>,
    },
    /// An MM reached the destination of `command_id`.
    Arrived {
        device_id: String,
        command_id: u64,
        x: f64,
        y: f64,
        at: Option<String>,
    },
    /// An MM was stopped before reaching its destination.
    Stopped {
        device_id: String,
        command_id: u64,
        x: f64,
        y: f64,
        at: Option<String>,
        origin: Origin,
    },
    /// A docked MM started a move away from its machine (just before the `move_command`).
    Undock {
        device_id: String,
        event: UndockEvent,
        /// The machine it leaves.
        target: String,
        /// The move that takes it away.
        command_id: u64,
        x: f64,
        y: f64,
    },
    /// An MM came to rest at a machine's dock (just after the `arrived` or `stopped`).
    Dock {
        device_id: String,
        event: DockEvent,
        /// The machine it docked at.
        target: String,
        /// The move that brought it there.
        command_id: u64,
        x: f64,
        y: f64,
    },
    /// A sensor reading, with where it was taken (MM position, machine centre or fixed position).
    Sensor {
        sensor: String,
        device_id: String,
        /// The sensor's kind, e.g. `"vibration"`.
        #[serde(rename = "type")]
        sensor_type: String,
        value: f64,
        unit: String,
        x: f64,
        y: f64,
    },
    /// A sensor's setpoint, noise or pause state was changed (ground truth for what the readings
    /// should show).
    SensorSet {
        sensor: String,
        device_id: String,
        #[serde(rename = "type")]
        sensor_type: String,
        setpoint: f64,
        noise_std: f64,
        previous_setpoint: f64,
        /// Whether its readings are paused, so it reports nothing.
        paused: bool,
        origin: Origin,
    },
    /// An MM performed one of the scenario's actions (e.g. pick). Silent: nothing else changes.
    Action {
        device_id: String,
        /// The action's id, e.g. `"pick"`.
        event: String,
        /// What was picked, placed, ..., if the command said.
        object: Option<String>,
        command_id: u64,
        x: f64,
        y: f64,
        at: Option<String>,
        origin: Origin,
    },
    /// Simulation lifecycle.
    Sim { state: Lifecycle, scenario: String },
    /// Scene lifecycle. Commands a scene runs appear as their own events with `origin: "scene"`.
    Scene {
        scene: String,
        state: SceneState,
        /// 1-based iteration (scenes can loop or repeat).
        iteration: u32,
        /// Why the scene failed (only for `failed`).
        error: Option<String>,
    },
}

/// A point on the floor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// The `event` of a `move_command`: always `"move"`, as in the traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveEvent {
    Move,
}

/// The `event` of an `undock`: always `"undock"`, as in the traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndockEvent {
    Undock,
}

/// The `event` of a `dock`: always `"dock"`, as in the traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockEvent {
    Dock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneState {
    Started,
    /// A looping or repeating scene began its next iteration.
    Restarted,
    Finished,
    /// Stopped by a `stop_scene`, a reset, or another scene being played.
    Stopped,
    /// A step was rejected (e.g. an action whose conditions didn't hold).
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Started,
    Paused,
    Resumed,
    Reset,
}

impl SimEvent {
    pub fn kind(&self) -> &'static str {
        match self.body {
            EventBody::Position { .. } => "position",
            EventBody::MoveCommand { .. } => "move_command",
            EventBody::Arrived { .. } => "arrived",
            EventBody::Stopped { .. } => "stopped",
            EventBody::Undock { .. } => "undock",
            EventBody::Dock { .. } => "dock",
            EventBody::Sensor { .. } => "sensor",
            EventBody::SensorSet { .. } => "sensor_set",
            EventBody::Action { .. } => "action",
            EventBody::Sim { .. } => "sim",
            EventBody::Scene { .. } => "scene",
        }
    }

    /// The machine, MM or free-standing sensor the event is about (`None` for `sim` and `scene`).
    pub fn device_id(&self) -> Option<&str> {
        match &self.body {
            EventBody::Position { device_id, .. }
            | EventBody::MoveCommand { device_id, .. }
            | EventBody::Arrived { device_id, .. }
            | EventBody::Stopped { device_id, .. }
            | EventBody::Undock { device_id, .. }
            | EventBody::Dock { device_id, .. }
            | EventBody::Sensor { device_id, .. }
            | EventBody::SensorSet { device_id, .. }
            | EventBody::Action { device_id, .. } => Some(device_id),
            EventBody::Sim { .. } | EventBody::Scene { .. } => None,
        }
    }
}

/// Every value of `kind`, as [`SimEvent::kind`] returns it.
pub const EVENT_KINDS: &[&str] = &[
    "position",
    "move_command",
    "arrived",
    "stopped",
    "undock",
    "dock",
    "sensor",
    "sensor_set",
    "action",
    "sim",
    "scene",
];

#[cfg(test)]
mod tests {
    //! Golden JSON: these strings are the wire contract. Changing them is a schema change.
    use super::*;

    fn event(seq: u64, sim_ms: u64, body: EventBody) -> SimEvent {
        SimEvent {
            v: SCHEMA_VERSION,
            seq,
            timestamp_ms: 1_761_730_000_000 + sim_ms,
            sim_ms,
            body,
        }
    }

    fn assert_golden(event: SimEvent, expected: &str) {
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, expected);
        let parsed: SimEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, event, "round trip");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["kind"], event.kind());
        assert!(EVENT_KINDS.contains(&event.kind()), "{} is missing from EVENT_KINDS", event.kind());
        assert_eq!(value.get("device_id").and_then(|id| id.as_str()), event.device_id());
    }

    #[test]
    fn move_command() {
        assert_golden(
            event(
                41,
                4200,
                EventBody::MoveCommand {
                    device_id: "mm1".into(),
                    event: MoveEvent::Move,
                    source: Some("machine1".into()),
                    destination: Some("machine2".into()),
                    command_id: 3,
                    origin: Origin::Ui,
                    from: Point { x: 4.0, y: 8.0 },
                    to: Point { x: 10.0, y: 8.0 },
                    distance_m: 6.0,
                    eta_ms: 7500,
                    replaces: None,
                },
            ),
            r#"{"v":2,"seq":41,"timestamp_ms":1761730004200,"sim_ms":4200,"kind":"move_command","device_id":"mm1","event":"move","source":"machine1","destination":"machine2","command_id":3,"origin":"ui","from":{"x":4.0,"y":8.0},"to":{"x":10.0,"y":8.0},"distance_m":6.0,"eta_ms":7500,"replaces":null}"#,
        );
    }

    #[test]
    fn position() {
        assert_golden(
            event(
                42,
                4300,
                EventBody::Position {
                    device_id: "mm1".into(),
                    x: 4.08,
                    y: 8.0,
                    heading_rad: 0.0,
                    speed_mps: 0.8,
                },
            ),
            r#"{"v":2,"seq":42,"timestamp_ms":1761730004300,"sim_ms":4300,"kind":"position","device_id":"mm1","x":4.08,"y":8.0,"heading_rad":0.0,"speed_mps":0.8}"#,
        );
    }

    #[test]
    fn arrived_and_stopped() {
        assert_golden(
            event(
                57,
                11700,
                EventBody::Arrived {
                    device_id: "mm1".into(),
                    command_id: 3,
                    x: 10.0,
                    y: 8.0,
                    at: Some("machine2".into()),
                },
            ),
            r#"{"v":2,"seq":57,"timestamp_ms":1761730011700,"sim_ms":11700,"kind":"arrived","device_id":"mm1","command_id":3,"x":10.0,"y":8.0,"at":"machine2"}"#,
        );
        assert_golden(
            event(
                58,
                12000,
                EventBody::Stopped {
                    device_id: "mm2".into(),
                    command_id: 4,
                    x: 6.5,
                    y: 3.25,
                    at: None,
                    origin: Origin::Scene,
                },
            ),
            r#"{"v":2,"seq":58,"timestamp_ms":1761730012000,"sim_ms":12000,"kind":"stopped","device_id":"mm2","command_id":4,"x":6.5,"y":3.25,"at":null,"origin":"scene"}"#,
        );
    }

    #[test]
    fn undock_and_dock() {
        assert_golden(
            event(
                40,
                4200,
                EventBody::Undock {
                    device_id: "mm1".into(),
                    event: UndockEvent::Undock,
                    target: "machine1".into(),
                    command_id: 3,
                    x: 4.0,
                    y: 8.0,
                },
            ),
            r#"{"v":2,"seq":40,"timestamp_ms":1761730004200,"sim_ms":4200,"kind":"undock","device_id":"mm1","event":"undock","target":"machine1","command_id":3,"x":4.0,"y":8.0}"#,
        );
        assert_golden(
            event(
                58,
                11700,
                EventBody::Dock {
                    device_id: "mm1".into(),
                    event: DockEvent::Dock,
                    target: "machine2".into(),
                    command_id: 3,
                    x: 10.0,
                    y: 8.0,
                },
            ),
            r#"{"v":2,"seq":58,"timestamp_ms":1761730011700,"sim_ms":11700,"kind":"dock","device_id":"mm1","event":"dock","target":"machine2","command_id":3,"x":10.0,"y":8.0}"#,
        );
    }

    #[test]
    fn sensor_and_sensor_set() {
        assert_golden(
            event(
                43,
                4500,
                EventBody::Sensor {
                    sensor: "mm1.vibration".into(),
                    device_id: "mm1".into(),
                    sensor_type: "vibration".into(),
                    value: 1.0,
                    unit: "g".into(),
                    x: 4.4,
                    y: 8.0,
                },
            ),
            r#"{"v":2,"seq":43,"timestamp_ms":1761730004500,"sim_ms":4500,"kind":"sensor","sensor":"mm1.vibration","device_id":"mm1","type":"vibration","value":1.0,"unit":"g","x":4.4,"y":8.0}"#,
        );
        assert_golden(
            event(
                60,
                13000,
                EventBody::SensorSet {
                    sensor: "mm1.vibration".into(),
                    device_id: "mm1".into(),
                    sensor_type: "vibration".into(),
                    setpoint: 0.8,
                    noise_std: 0.0,
                    previous_setpoint: 0.1,
                    paused: false,
                    origin: Origin::Ui,
                },
            ),
            r#"{"v":2,"seq":60,"timestamp_ms":1761730013000,"sim_ms":13000,"kind":"sensor_set","sensor":"mm1.vibration","device_id":"mm1","type":"vibration","setpoint":0.8,"noise_std":0.0,"previous_setpoint":0.1,"paused":false,"origin":"ui"}"#,
        );
    }

    #[test]
    fn action() {
        assert_golden(
            event(
                88,
                12300,
                EventBody::Action {
                    device_id: "mm1".into(),
                    event: "pick".into(),
                    object: Some("dies".into()),
                    command_id: 7,
                    x: 10.0,
                    y: 8.0,
                    at: Some("machine2".into()),
                    origin: Origin::Ui,
                },
            ),
            r#"{"v":2,"seq":88,"timestamp_ms":1761730012300,"sim_ms":12300,"kind":"action","device_id":"mm1","event":"pick","object":"dies","command_id":7,"x":10.0,"y":8.0,"at":"machine2","origin":"ui"}"#,
        );
    }

    #[test]
    fn scene_lifecycle() {
        assert_golden(
            event(
                120,
                30000,
                EventBody::Scene {
                    scene: "production".into(),
                    state: SceneState::Restarted,
                    iteration: 2,
                    error: None,
                },
            ),
            r#"{"v":2,"seq":120,"timestamp_ms":1761730030000,"sim_ms":30000,"kind":"scene","scene":"production","state":"restarted","iteration":2,"error":null}"#,
        );
        assert_golden(
            event(
                121,
                31000,
                EventBody::Scene {
                    scene: "production".into(),
                    state: SceneState::Failed,
                    iteration: 2,
                    error: Some("'pick' needs mm1 at a machine; it is at (6.0, 3.0)".into()),
                },
            ),
            r#"{"v":2,"seq":121,"timestamp_ms":1761730031000,"sim_ms":31000,"kind":"scene","scene":"production","state":"failed","iteration":2,"error":"'pick' needs mm1 at a machine; it is at (6.0, 3.0)"}"#,
        );
    }

    #[test]
    fn sim_lifecycle() {
        assert_golden(
            event(
                1,
                0,
                EventBody::Sim {
                    state: Lifecycle::Started,
                    scenario: "Default cell".into(),
                },
            ),
            r#"{"v":2,"seq":1,"timestamp_ms":1761730000000,"sim_ms":0,"kind":"sim","state":"started","scenario":"Default cell"}"#,
        );
    }

    /// A trace payload's keys, with the same values, are all in the matching event.
    #[test]
    fn events_carry_the_trace_keys() {
        let pick = serde_json::to_value(event(
            1,
            0,
            EventBody::Action {
                device_id: "mm-1".into(),
                event: "pick".into(),
                object: Some("dies".into()),
                command_id: 1,
                x: 4.0,
                y: 8.0,
                at: Some("stocker".into()),
                origin: Origin::Scene,
            },
        ))
        .unwrap();
        let trace: serde_json::Value = serde_json::from_str(
            r#"{"device_id": "mm-1", "event": "pick", "object": "dies", "timestamp_ms": 1761730000000}"#,
        )
        .unwrap();
        for (key, value) in trace.as_object().unwrap() {
            assert_eq!(&pick[key], value, "{key}");
        }
    }

    /// Every name in `EVENT_KINDS` is a real `kind` (the golden tests check the other direction).
    #[test]
    fn event_kinds_are_real_kinds() {
        for kind in EVENT_KINDS {
            let json = format!(r#"{{"v":2,"seq":1,"timestamp_ms":0,"sim_ms":0,"kind":"{kind}"}}"#);
            let error = serde_json::from_str::<SimEvent>(&json).unwrap_err().to_string();
            assert!(!error.contains("unknown variant"), "{kind}: {error}");
        }
    }
}
