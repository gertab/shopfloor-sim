//! Commands: the only way to change the simulated world.
//!
//! The UI, a future scene player and any external API all send the same serialisable commands,
//! tagged with their [`Origin`]. A scene file is just a list of timed `SimCommand`s.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SimCommand {
    /// Send an MM somewhere. Replaces any move in progress.
    MoveTo { mm: String, target: Target },
    /// Stop an MM where it is.
    Stop { mm: String },
    /// Set the value a sensor reports (before noise).
    SetSensor { sensor: String, value: f64 },
    /// Set the standard deviation of the Gaussian noise added to a sensor's readings.
    SetSensorNoise { sensor: String, std: f64 },
    /// Pause or resume a sensor's readings. A paused sensor reports nothing, as if it had failed,
    /// and carries on with its usual schedule and current setpoint when resumed.
    SetSensorPaused { sensor: String, paused: bool },
    /// Perform one of the scenario's `[actions]` (e.g. pick). Silent: it only emits an `action`
    /// event, if the action's conditions hold.
    Perform { mm: String, action: String },
}

/// JSON: `{"point": {"x": 1.0, "y": 2.0}}` or `{"machine": "machine2"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Point { x: f64, y: f64 },
    /// The machine's dock point.
    Machine(String),
}

/// Who issued a command. Carried into the events it causes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Ui,
    Scene,
    Api,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub origin: Origin,
    pub command: SimCommand,
}

impl CommandEnvelope {
    pub fn new(origin: Origin, command: SimCommand) -> Self {
        Self { origin, command }
    }
}

/// Engine control (not part of scenes). JSON: `"pause"`, `{"play_scene": "production"}`, …
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    Pause,
    Resume,
    /// Back to the scenario's initial state (positions, setpoints, sim time 0, RNG seed). Stops
    /// any playing scene.
    Reset,
    /// Play one of the scenario's scenes, replacing any scene that is playing.
    PlayScene(String),
    /// Stop the playing scene. The world keeps running as it is.
    StopScene,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commands_have_a_stable_json_shape() {
        let command = SimCommand::MoveTo {
            mm: "mm1".into(),
            target: Target::Machine("machine2".into()),
        };
        assert_eq!(
            serde_json::to_value(&command).unwrap(),
            json!({"type": "move_to", "mm": "mm1", "target": {"machine": "machine2"}})
        );
        let parsed: SimCommand = serde_json::from_value(json!({
            "type": "move_to", "mm": "mm2", "target": {"point": {"x": 3, "y": 4.5}}
        }))
        .unwrap();
        assert_eq!(
            parsed,
            SimCommand::MoveTo {
                mm: "mm2".into(),
                target: Target::Point { x: 3.0, y: 4.5 }
            }
        );
        let parsed: SimCommand =
            serde_json::from_value(json!({"type": "set_sensor", "sensor": "mm1.vibration", "value": 0.8}))
                .unwrap();
        assert_eq!(
            parsed,
            SimCommand::SetSensor {
                sensor: "mm1.vibration".into(),
                value: 0.8
            }
        );
        let perform = SimCommand::Perform {
            mm: "mm1".into(),
            action: "pick".into(),
        };
        assert_eq!(
            serde_json::to_value(&perform).unwrap(),
            json!({"type": "perform", "mm": "mm1", "action": "pick"})
        );
    }

    #[test]
    fn controls_have_a_stable_json_shape() {
        assert_eq!(serde_json::to_value(Control::Pause).unwrap(), json!("pause"));
        assert_eq!(serde_json::to_value(Control::StopScene).unwrap(), json!("stop_scene"));
        assert_eq!(
            serde_json::from_value::<Control>(json!({"play_scene": "production"})).unwrap(),
            Control::PlayScene("production".into())
        );
    }

    #[test]
    fn commands_can_be_written_in_toml() {
        // The shape future scene files will use.
        #[derive(Deserialize)]
        struct Step {
            at_ms: u64,
            command: SimCommand,
        }
        let step: Step = toml::from_str(
            r#"
            at_ms = 1500
            command = { type = "move_to", mm = "mm1", target = { machine = "machine2" } }
            "#,
        )
        .unwrap();
        assert_eq!(step.at_ms, 1500);
        assert!(matches!(step.command, SimCommand::MoveTo { .. }));

        let step: Step = toml::from_str(
            r#"
            at_ms = 3000
            command = { type = "perform", mm = "mm1", action = "pick" }
            "#,
        )
        .unwrap();
        assert_eq!(
            step.command,
            SimCommand::Perform {
                mm: "mm1".into(),
                action: "pick".into()
            }
        );
    }
}
