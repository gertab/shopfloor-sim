//! The output event stream. This is the contract with consumers (see `README.md`).
//!
//! Every event is one flat JSON object: an envelope (`v`, `seq`, `ts`, `sim_ms`, `kind`) plus the
//! fields of its kind. Measurements are always JSON floats.

use crate::command::Origin;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimEvent {
    /// Schema version.
    pub v: u32,
    /// Per-run sequence number, +1 per event.
    pub seq: u64,
    /// Wall-clock time, epoch milliseconds.
    pub ts: u64,
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
        entity: String,
        x: f64,
        y: f64,
        heading_rad: f64,
        speed_mps: f64,
    },
    /// An MM was given a new destination.
    MoveCommand {
        entity: String,
        command_id: u64,
        origin: Origin,
        from: Place,
        to: Place,
        distance_m: f64,
        eta_ms: u64,
        /// The command this one replaced, if the MM was already moving.
        replaces: Option<u64>,
    },
    /// An MM reached the destination of `command_id`.
    Arrived {
        entity: String,
        command_id: u64,
        x: f64,
        y: f64,
        at: Option<String>,
    },
    /// An MM was stopped before reaching its destination.
    Stopped {
        entity: String,
        command_id: u64,
        x: f64,
        y: f64,
        at: Option<String>,
        origin: Origin,
    },
    /// A sensor reading, with where it was taken (MM position, machine centre or fixed position).
    Sensor {
        sensor: String,
        entity: String,
        sensor_kind: String,
        value: f64,
        unit: String,
        x: f64,
        y: f64,
    },
    /// A sensor's setpoint, noise or pause state was changed (ground truth for what the readings
    /// should show).
    SensorSet {
        sensor: String,
        entity: String,
        sensor_kind: String,
        setpoint: f64,
        noise_std: f64,
        previous_setpoint: f64,
        /// Whether its readings are paused, so it reports nothing.
        paused: bool,
        origin: Origin,
    },
    /// An MM performed one of the scenario's actions (e.g. pick). Silent: nothing else changes.
    Action {
        entity: String,
        action: String,
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

/// A point on the floor, with the machine it is docked at (if any).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub x: f64,
    pub y: f64,
    pub at: Option<String>,
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
            EventBody::Sensor { .. } => "sensor",
            EventBody::SensorSet { .. } => "sensor_set",
            EventBody::Action { .. } => "action",
            EventBody::Sim { .. } => "sim",
            EventBody::Scene { .. } => "scene",
        }
    }

    /// The machine, MM or free-standing sensor the event is about (`None` for `sim` and `scene`).
    pub fn entity(&self) -> Option<&str> {
        match &self.body {
            EventBody::Position { entity, .. }
            | EventBody::MoveCommand { entity, .. }
            | EventBody::Arrived { entity, .. }
            | EventBody::Stopped { entity, .. }
            | EventBody::Sensor { entity, .. }
            | EventBody::SensorSet { entity, .. }
            | EventBody::Action { entity, .. } => Some(entity),
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
            ts: 1_761_730_000_000 + sim_ms,
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
        assert_eq!(value.get("entity").and_then(|entity| entity.as_str()), event.entity());
    }

    #[test]
    fn move_command() {
        assert_golden(
            event(
                41,
                4200,
                EventBody::MoveCommand {
                    entity: "mm1".into(),
                    command_id: 3,
                    origin: Origin::Ui,
                    from: Place { x: 4.0, y: 8.0, at: Some("machine1".into()) },
                    to: Place { x: 10.0, y: 8.0, at: Some("machine2".into()) },
                    distance_m: 6.0,
                    eta_ms: 7500,
                    replaces: None,
                },
            ),
            r#"{"v":1,"seq":41,"ts":1761730004200,"sim_ms":4200,"kind":"move_command","entity":"mm1","command_id":3,"origin":"ui","from":{"x":4.0,"y":8.0,"at":"machine1"},"to":{"x":10.0,"y":8.0,"at":"machine2"},"distance_m":6.0,"eta_ms":7500,"replaces":null}"#,
        );
    }

    #[test]
    fn position() {
        assert_golden(
            event(
                42,
                4300,
                EventBody::Position {
                    entity: "mm1".into(),
                    x: 4.08,
                    y: 8.0,
                    heading_rad: 0.0,
                    speed_mps: 0.8,
                },
            ),
            r#"{"v":1,"seq":42,"ts":1761730004300,"sim_ms":4300,"kind":"position","entity":"mm1","x":4.08,"y":8.0,"heading_rad":0.0,"speed_mps":0.8}"#,
        );
    }

    #[test]
    fn arrived_and_stopped() {
        assert_golden(
            event(
                57,
                11700,
                EventBody::Arrived {
                    entity: "mm1".into(),
                    command_id: 3,
                    x: 10.0,
                    y: 8.0,
                    at: Some("machine2".into()),
                },
            ),
            r#"{"v":1,"seq":57,"ts":1761730011700,"sim_ms":11700,"kind":"arrived","entity":"mm1","command_id":3,"x":10.0,"y":8.0,"at":"machine2"}"#,
        );
        assert_golden(
            event(
                58,
                12000,
                EventBody::Stopped {
                    entity: "mm2".into(),
                    command_id: 4,
                    x: 6.5,
                    y: 3.25,
                    at: None,
                    origin: Origin::Scene,
                },
            ),
            r#"{"v":1,"seq":58,"ts":1761730012000,"sim_ms":12000,"kind":"stopped","entity":"mm2","command_id":4,"x":6.5,"y":3.25,"at":null,"origin":"scene"}"#,
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
                    entity: "mm1".into(),
                    sensor_kind: "vibration".into(),
                    value: 1.0,
                    unit: "g".into(),
                    x: 4.4,
                    y: 8.0,
                },
            ),
            r#"{"v":1,"seq":43,"ts":1761730004500,"sim_ms":4500,"kind":"sensor","sensor":"mm1.vibration","entity":"mm1","sensor_kind":"vibration","value":1.0,"unit":"g","x":4.4,"y":8.0}"#,
        );
        assert_golden(
            event(
                60,
                13000,
                EventBody::SensorSet {
                    sensor: "mm1.vibration".into(),
                    entity: "mm1".into(),
                    sensor_kind: "vibration".into(),
                    setpoint: 0.8,
                    noise_std: 0.0,
                    previous_setpoint: 0.1,
                    paused: false,
                    origin: Origin::Ui,
                },
            ),
            r#"{"v":1,"seq":60,"ts":1761730013000,"sim_ms":13000,"kind":"sensor_set","sensor":"mm1.vibration","entity":"mm1","sensor_kind":"vibration","setpoint":0.8,"noise_std":0.0,"previous_setpoint":0.1,"paused":false,"origin":"ui"}"#,
        );
    }

    #[test]
    fn action() {
        assert_golden(
            event(
                88,
                12300,
                EventBody::Action {
                    entity: "mm1".into(),
                    action: "pick".into(),
                    command_id: 7,
                    x: 10.0,
                    y: 8.0,
                    at: Some("machine2".into()),
                    origin: Origin::Ui,
                },
            ),
            r#"{"v":1,"seq":88,"ts":1761730012300,"sim_ms":12300,"kind":"action","entity":"mm1","action":"pick","command_id":7,"x":10.0,"y":8.0,"at":"machine2","origin":"ui"}"#,
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
            r#"{"v":1,"seq":120,"ts":1761730030000,"sim_ms":30000,"kind":"scene","scene":"production","state":"restarted","iteration":2,"error":null}"#,
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
            r#"{"v":1,"seq":121,"ts":1761730031000,"sim_ms":31000,"kind":"scene","scene":"production","state":"failed","iteration":2,"error":"'pick' needs mm1 at a machine; it is at (6.0, 3.0)"}"#,
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
            r#"{"v":1,"seq":1,"ts":1761730000000,"sim_ms":0,"kind":"sim","state":"started","scenario":"Default cell"}"#,
        );
    }

    /// Every name in `EVENT_KINDS` is a real `kind` (the golden tests check the other direction).
    #[test]
    fn event_kinds_are_real_kinds() {
        for kind in EVENT_KINDS {
            let json = format!(r#"{{"v":1,"seq":1,"ts":0,"sim_ms":0,"kind":"{kind}"}}"#);
            let error = serde_json::from_str::<SimEvent>(&json).unwrap_err().to_string();
            assert!(!error.contains("unknown variant"), "{kind}: {error}");
        }
    }
}
