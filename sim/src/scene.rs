//! Scenes: scripted runs of commands and waits, defined in the scenario file and played by the
//! engine in simulation time.
//!
//! A scene has one or more *tracks* that run in parallel. Each track is a list of *steps* run in
//! order: commands (the same [`SimCommand`]s the UI sends) and waits. An *iteration* ends when every
//! track has run out of steps; a scene runs once, a fixed number of times, or forever.

use crate::command::{SimCommand, Target};
use crate::geometry::Vec2;
use crate::scenario::Scenario;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------------------------
// File schema
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSpec {
    pub label: Option<String>,
    /// Start over every time all tracks finish, until stopped.
    #[serde(default, rename = "loop")]
    pub looping: bool,
    /// Run this many iterations (default 1). Exclusive with `loop`.
    pub repeat: Option<u32>,
    pub tracks: Vec<TrackSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackSpec {
    pub name: Option<String>,
    pub steps: Vec<StepSpec>,
}

/// One step, written as a single-key table: `{ wait_ms = 1500 }`, `{ move_to = { mm = "mm1", … } }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepSpec {
    MoveTo(MoveToSpec),
    Stop(String),
    Perform(PerformSpec),
    SetSensor(SetSensorSpec),
    SetSensorNoise(SetSensorNoiseSpec),
    /// Stop this sensor's readings, as if it had failed.
    PauseSensor(String),
    /// Start this sensor's readings again.
    ResumeSensor(String),
    /// Wait this many milliseconds of simulation time.
    WaitMs(u64),
    /// Wait until this MM has no move in progress.
    WaitIdle(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveToSpec {
    pub mm: String,
    pub machine: Option<String>,
    pub point: Option<[f64; 2]>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformSpec {
    pub mm: String,
    pub action: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSensorSpec {
    pub sensor: String,
    pub value: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSensorNoiseSpec {
    pub sensor: String,
    pub std: f64,
}

// ---------------------------------------------------------------------------------------------
// Resolved model
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    Once,
    Times(u32),
    Forever,
}

#[derive(Debug, Clone, Serialize)]
pub struct SceneDef {
    pub id: String,
    pub label: String,
    pub repeat: Repeat,
    pub tracks: Vec<TrackDef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackDef {
    pub name: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Command(SimCommand),
    WaitMs(u64),
    WaitIdle(String),
}

impl Step {
    pub fn is_wait(&self) -> bool {
        matches!(self, Step::WaitMs(_) | Step::WaitIdle(_))
    }

    /// Short human description, e.g. "move mm1 → Deflasher" or "wait until mm1 is idle".
    pub fn describe(&self, scenario: &Scenario) -> String {
        match self {
            Step::Command(command) => match command {
                SimCommand::MoveTo { mm, target } => match target {
                    Target::Machine(id) => {
                        let label = scenario.machine(id).map_or(id.as_str(), |m| m.label.as_str());
                        format!("move {mm} → {label}")
                    }
                    Target::Point { x, y } => format!("move {mm} → ({x:.1}, {y:.1})"),
                },
                SimCommand::Stop { mm } => format!("stop {mm}"),
                SimCommand::Perform { mm, action } => format!("{mm}: {action}"),
                SimCommand::SetSensor { sensor, value } => format!("set {sensor} = {value}"),
                SimCommand::SetSensorNoise { sensor, std } => format!("set {sensor} noise σ = {std}"),
                SimCommand::SetSensorPaused { sensor, paused: true } => format!("pause {sensor}"),
                SimCommand::SetSensorPaused { sensor, paused: false } => format!("resume {sensor}"),
            },
            Step::WaitMs(ms) => format!("wait {}", format_seconds(*ms)),
            Step::WaitIdle(mm) => format!("wait until {mm} is idle"),
        }
    }
}

pub(crate) fn format_seconds(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

// ---------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------

/// Resolves and validates scenes against an otherwise complete scenario. Problems are appended to
/// `errors` (all reported together with the rest of the scenario's errors).
pub(crate) fn resolve_scenes(
    specs: &BTreeMap<String, SceneSpec>,
    scenario: &Scenario,
    errors: &mut Vec<String>,
) -> Vec<SceneDef> {
    let mut scenes = Vec::new();
    for (id, spec) in specs {
        let at = format!("scene '{id}'");
        if !crate::scenario::valid_id(id) {
            errors.push(format!("{at}: id may only contain letters, digits, '_' and '-'"));
        }
        let repeat = match (spec.looping, spec.repeat) {
            (true, Some(_)) => {
                errors.push(format!("{at}: use either `loop` or `repeat`, not both"));
                Repeat::Forever
            }
            (true, None) => Repeat::Forever,
            (false, None) | (false, Some(1)) => Repeat::Once,
            (false, Some(0)) => {
                errors.push(format!("{at}: repeat must be at least 1"));
                Repeat::Once
            }
            (false, Some(n)) => Repeat::Times(n),
        };
        if spec.tracks.is_empty() {
            errors.push(format!("{at}: needs at least one [[scenes.{id}.tracks]]"));
        }

        let mut tracks = Vec::new();
        for (track_index, track) in spec.tracks.iter().enumerate() {
            let name = track
                .name
                .clone()
                .unwrap_or_else(|| format!("track {}", track_index + 1));
            if track.steps.is_empty() {
                errors.push(format!("{at}, track '{name}': has no steps"));
            }
            let steps = track
                .steps
                .iter()
                .enumerate()
                .map(|(step_index, step)| {
                    resolve_step(step, scenario).unwrap_or_else(|error| {
                        errors.push(format!(
                            "{at}, track '{name}', step {}: {error}",
                            step_index + 1
                        ));
                        Step::WaitMs(0)
                    })
                })
                .collect();
            tracks.push(TrackDef { name, steps });
        }

        let has_wait = tracks
            .iter()
            .any(|track| track.steps.iter().any(Step::is_wait));
        if repeat != Repeat::Once && !has_wait {
            errors.push(format!(
                "{at}: repeats but has no wait steps, so it would restart every tick"
            ));
        }

        scenes.push(SceneDef {
            id: id.clone(),
            label: spec.label.clone().unwrap_or_else(|| id.clone()),
            repeat,
            tracks,
        });
    }
    scenes
}

fn resolve_step(spec: &StepSpec, scenario: &Scenario) -> Result<Step, String> {
    Ok(match spec {
        StepSpec::MoveTo(MoveToSpec { mm, machine, point }) => {
            let def = mm_def(scenario, mm)?;
            let target = match (machine, point) {
                (Some(machine), None) => {
                    scenario
                        .machine(machine)
                        .ok_or_else(|| format!("unknown machine '{machine}'"))?;
                    Target::Machine(machine.clone())
                }
                (None, Some([x, y])) => {
                    let point = Vec2::new(*x, *y);
                    if !(point.is_finite() && scenario.reachable_area(def.radius_m).contains(point)) {
                        return Err(format!(
                            "point ({x}, {y}) is outside the area {mm} can reach"
                        ));
                    }
                    Target::Point { x: *x, y: *y }
                }
                _ => return Err("move_to needs exactly one of `machine` or `point`".to_string()),
            };
            Step::Command(SimCommand::MoveTo {
                mm: mm.clone(),
                target,
            })
        }
        StepSpec::Stop(mm) => {
            mm_def(scenario, mm)?;
            Step::Command(SimCommand::Stop { mm: mm.clone() })
        }
        StepSpec::Perform(PerformSpec { mm, action }) => {
            let def = mm_def(scenario, mm)?;
            scenario
                .action(action)
                .ok_or_else(|| format!("unknown action '{action}'"))?;
            if !def.actions.contains(action) {
                return Err(format!("{mm} cannot perform '{action}' (not in its actions)"));
            }
            Step::Command(SimCommand::Perform {
                mm: mm.clone(),
                action: action.clone(),
            })
        }
        StepSpec::SetSensor(SetSensorSpec { sensor, value }) => {
            let index = scenario
                .sensor_index(sensor)
                .ok_or_else(|| format!("unknown sensor '{sensor}'"))?;
            let def = &scenario.sensors[index];
            if !(value.is_finite() && *value >= def.min && *value <= def.max) {
                return Err(format!(
                    "value {value} for '{sensor}' is outside its range [{}, {}]",
                    def.min, def.max
                ));
            }
            Step::Command(SimCommand::SetSensor {
                sensor: sensor.clone(),
                value: *value,
            })
        }
        StepSpec::SetSensorNoise(SetSensorNoiseSpec { sensor, std }) => {
            scenario
                .sensor_index(sensor)
                .ok_or_else(|| format!("unknown sensor '{sensor}'"))?;
            if !(std.is_finite() && *std >= 0.0) {
                return Err(format!("noise for '{sensor}' must be >= 0"));
            }
            Step::Command(SimCommand::SetSensorNoise {
                sensor: sensor.clone(),
                std: *std,
            })
        }
        StepSpec::PauseSensor(sensor) | StepSpec::ResumeSensor(sensor) => {
            scenario
                .sensor_index(sensor)
                .ok_or_else(|| format!("unknown sensor '{sensor}'"))?;
            Step::Command(SimCommand::SetSensorPaused {
                sensor: sensor.clone(),
                paused: matches!(spec, StepSpec::PauseSensor(_)),
            })
        }
        StepSpec::WaitMs(ms) => Step::WaitMs(*ms),
        StepSpec::WaitIdle(mm) => {
            mm_def(scenario, mm)?;
            Step::WaitIdle(mm.clone())
        }
    })
}

fn mm_def<'a>(scenario: &'a Scenario, mm: &str) -> Result<&'a crate::scenario::MmDef, String> {
    scenario
        .mm_index(mm)
        .map(|index| &scenario.mobile_manipulators[index])
        .ok_or_else(|| format!("unknown mobile manipulator '{mm}'"))
}

// ---------------------------------------------------------------------------------------------
// Playback state (driven by the engine)
// ---------------------------------------------------------------------------------------------

/// Where a playing scene is. Owned by the engine, advanced in simulation time.
#[derive(Debug, Clone)]
pub(crate) struct ScenePlayer {
    /// Index into `Scenario::scenes`.
    pub scene: usize,
    /// 1-based.
    pub iteration: u32,
    pub iteration_started_ms: u64,
    pub tracks: Vec<TrackCursor>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TrackCursor {
    /// Index of the next step to run.
    pub next: usize,
    /// The wait set by step `next - 1`, if it hasn't finished yet.
    pub waiting: Option<Wait>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Wait {
    /// Until simulation time reaches this value.
    Until(u64),
    /// Until the MM with this index has no move in progress.
    Idle(usize),
}

impl ScenePlayer {
    pub fn new(scene: usize, tracks: usize, now_ms: u64) -> Self {
        Self {
            scene,
            iteration: 1,
            iteration_started_ms: now_ms,
            tracks: vec![TrackCursor::default(); tracks],
        }
    }

    pub fn restart(&mut self, now_ms: u64) {
        self.iteration += 1;
        self.iteration_started_ms = now_ms;
        self.tracks.fill(TrackCursor::default());
    }
}

/// Progress of the playing scene, for the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SceneStatus {
    pub id: String,
    pub label: String,
    pub iteration: u32,
    pub tracks: Vec<TrackStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrackStatus {
    pub name: String,
    /// 1-based index of the step being waited on (equals `steps` when the track is done).
    pub step: usize,
    pub steps: usize,
    /// What the track is waiting on, e.g. "wait 1.5 s (0.4 s left)"; `None` when done.
    pub current: Option<String>,
    /// Finished this iteration (waiting for the other tracks).
    pub done: bool,
}

#[cfg(test)]
mod tests {
    use crate::scenario::parse_scenario;

    const BASE: &str = r#"
        [floor]
        width_m = 10.0
        height_m = 6.0

        [machines.m1]
        position = [2.0, 4.5]
        size = [2.0, 1.0]

        [mobile_manipulators.mm1]
        start_at = "m1"
        actions = ["pick"]

        [mobile_manipulators.mm1.sensors.vibration]
        default = 0.1
        range = [0.0, 2.0]

        [actions.pick]
        requires_machine = true

        [actions.wave]
    "#;

    fn errors_of(scenes: &str) -> String {
        format!("{:#}", parse_scenario(&format!("{BASE}\n{scenes}"), None).unwrap_err())
    }

    #[test]
    fn parses_scenes_with_defaults() {
        let scenario = parse_scenario(
            &format!(
                "{BASE}\n{}",
                r#"
                [scenes.demo]
                [[scenes.demo.tracks]]
                steps = [
                  { move_to = { mm = "mm1", point = [5.0, 2.0] } },
                  { wait_idle = "mm1" },
                  { move_to = { mm = "mm1", machine = "m1" } },
                  { perform = { mm = "mm1", action = "pick" } },
                  { set_sensor = { sensor = "mm1.vibration", value = 1.0 } },
                  { set_sensor_noise = { sensor = "mm1.vibration", std = 0.1 } },
                  { stop = "mm1" },
                  { wait_ms = 500 },
                ]

                [scenes.looping]
                label = "Looping"
                loop = true
                [[scenes.looping.tracks]]
                name = "waiter"
                steps = [{ wait_ms = 100 }]

                [scenes.twice]
                repeat = 2
                [[scenes.twice.tracks]]
                steps = [{ wait_ms = 100 }]
                "#
            ),
            None,
        )
        .unwrap();
        let demo = &scenario.scenes[0];
        assert_eq!((demo.id.as_str(), demo.label.as_str()), ("demo", "demo"));
        assert_eq!(demo.repeat, super::Repeat::Once);
        assert_eq!(demo.tracks[0].name, "track 1");
        assert_eq!(demo.tracks[0].steps.len(), 8);
        let describe: Vec<String> = demo.tracks[0]
            .steps
            .iter()
            .map(|step| step.describe(&scenario))
            .collect();
        assert_eq!(
            describe,
            vec![
                "move mm1 → (5.0, 2.0)",
                "wait until mm1 is idle",
                "move mm1 → m1",
                "mm1: pick",
                "set mm1.vibration = 1",
                "set mm1.vibration noise σ = 0.1",
                "stop mm1",
                "wait 0.5 s",
            ]
        );
        assert_eq!(scenario.scenes[1].repeat, super::Repeat::Forever);
        assert_eq!(scenario.scenes[1].tracks[0].name, "waiter");
        assert_eq!(scenario.scenes[2].repeat, super::Repeat::Times(2));
    }

    #[test]
    fn rejects_steps_that_reference_unknown_or_invalid_things() {
        let errors = errors_of(
            r#"
            [scenes.s]
            [[scenes.s.tracks]]
            steps = [
              { move_to = { mm = "mm9", machine = "m1" } },
              { move_to = { mm = "mm1", machine = "m9" } },
              { move_to = { mm = "mm1", machine = "m1", point = [1.0, 1.0] } },
              { move_to = { mm = "mm1", point = [0.1, 1.0] } },
              { perform = { mm = "mm1", action = "fly" } },
              { perform = { mm = "mm1", action = "wave" } },
              { set_sensor = { sensor = "mm1.vibration", value = 5.0 } },
              { set_sensor = { sensor = "mm1.nope", value = 1.0 } },
              { set_sensor_noise = { sensor = "mm1.vibration", std = -1.0 } },
              { wait_idle = "mm9" },
              { pause_sensor = "mm1.nope" },
              { resume_sensor = "mm1.gone" },
            ]
            "#,
        );
        for expected in [
            "scene 's', track 'track 1', step 1: unknown mobile manipulator 'mm9'",
            "step 2: unknown machine 'm9'",
            "step 3: move_to needs exactly one of `machine` or `point`",
            "step 4: point (0.1, 1) is outside the area mm1 can reach",
            "step 5: unknown action 'fly'",
            "step 6: mm1 cannot perform 'wave' (not in its actions)",
            "step 7: value 5 for 'mm1.vibration' is outside its range [0, 2]",
            "step 8: unknown sensor 'mm1.nope'",
            "step 9: noise for 'mm1.vibration' must be >= 0",
            "step 10: unknown mobile manipulator 'mm9'",
            "step 11: unknown sensor 'mm1.nope'",
            "step 12: unknown sensor 'mm1.gone'",
        ] {
            assert!(errors.contains(expected), "missing {expected:?} in:\n{errors}");
        }
    }

    #[test]
    fn rejects_bad_scene_structure() {
        let errors = errors_of(
            r#"
            [scenes.both]
            loop = true
            repeat = 2
            [[scenes.both.tracks]]
            steps = [{ wait_ms = 1 }]

            [scenes.zero]
            repeat = 0
            [[scenes.zero.tracks]]
            steps = [{ wait_ms = 1 }]

            [scenes.no_tracks]
            tracks = []

            [scenes.empty_track]
            [[scenes.empty_track.tracks]]
            steps = []

            [scenes.spin]
            loop = true
            [[scenes.spin.tracks]]
            steps = [{ perform = { mm = "mm1", action = "pick" } }]
            "#,
        );
        for expected in [
            "scene 'both': use either `loop` or `repeat`, not both",
            "scene 'zero': repeat must be at least 1",
            "scene 'no_tracks': needs at least one [[scenes.no_tracks.tracks]]",
            "scene 'empty_track', track 'track 1': has no steps",
            "scene 'spin': repeats but has no wait steps",
        ] {
            assert!(errors.contains(expected), "missing {expected:?} in:\n{errors}");
        }
    }

    #[test]
    fn rejects_unknown_step_kinds() {
        let errors = errors_of(
            r#"
            [scenes.s]
            [[scenes.s.tracks]]
            steps = [{ teleport = "mm1" }]
            "#,
        );
        assert!(errors.contains("teleport"), "{errors}");
    }
}
