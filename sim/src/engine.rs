//! The simulation engine: synchronous and deterministic.
//!
//! The engine owns the world state and changes it in exactly two ways:
//! - [`Engine::apply`] executes a [`CommandEnvelope`] (from the UI, a scene, an API…);
//! - [`Engine::step`] advances simulation time by one fixed tick (`sim.tick_ms`).
//!
//! It also plays scenes ([`Control::PlayScene`]): after each tick's physics, the scene player
//! resumes tracks whose waits are over and runs their commands through `apply`.
//!
//! Everything returns the [`SimEvent`]s it caused. Wall-clock time is passed in (`ts`) and only
//! used to stamp events, so tests and faster-than-real-time runs behave identically.

use crate::command::{CommandEnvelope, Control, Origin, SimCommand, Target};
use crate::event::{EventBody, Lifecycle, Place, SCHEMA_VERSION, SceneState, SimEvent};
use crate::geometry::{Vec2, path_length, round_to};
use crate::motion::{PathPlanner, StraightLine};
use crate::scenario::{HostKind, MmDef, Scenario};
use crate::scene::{
    Repeat, ScenePlayer, SceneStatus, Step, TrackStatus, Wait, format_seconds,
};
use anyhow::{anyhow, bail};
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand_distr::{Distribution, Normal};
use serde::Serialize;
use std::sync::Arc;

/// Distances below this count as "already there" (metres).
const ARRIVAL_EPS_M: f64 = 1e-9;
/// Decimals kept in event output: positions to the millimetre, headings to ~0.0001 rad.
const POSITION_DECIMALS: i32 = 3;
const HEADING_DECIMALS: i32 = 4;
const VALUE_DECIMALS: i32 = 6;

pub struct Engine {
    scenario: Arc<Scenario>,
    planner: Box<dyn PathPlanner>,
    sim_ms: u64,
    paused: bool,
    seq: u64,
    next_command_id: u64,
    mms: Vec<MmState>,
    sensors: Vec<SensorState>,
    /// Where each sensor (same order as `scenario.sensors`) takes its readings.
    anchors: Vec<SensorAnchor>,
    rng: StdRng,
    scene: Option<ScenePlayer>,
}

#[derive(Debug, Clone, Copy)]
enum SensorAnchor {
    /// Mounted on the MM with this index: readings are taken where the MM is.
    Mm(usize),
    /// Machine-mounted (machine centre) or free-standing (its position).
    Fixed(Vec2),
}

#[derive(Debug, Clone)]
struct MmState {
    position: Vec2,
    heading: f64,
    motion: Option<Motion>,
}

#[derive(Debug, Clone)]
struct Motion {
    command_id: u64,
    /// Planned path; `waypoints[next..]` is still ahead.
    waypoints: Vec<Vec2>,
    next: usize,
    target: Place,
    next_position_ms: u64,
}

impl Motion {
    fn remaining(&self) -> &[Vec2] {
        &self.waypoints[self.next..]
    }
}

#[derive(Debug, Clone)]
struct SensorState {
    setpoint: f64,
    noise_std: f64,
    paused: bool,
    next_due_ms: u64,
    last_value: Option<f64>,
    last_ms: Option<u64>,
}

/// Dynamic world state for the UI (the static layout is the [`Scenario`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorldState {
    pub sim_ms: u64,
    pub paused: bool,
    pub mobile_manipulators: Vec<MmView>,
    pub sensors: Vec<SensorView>,
    /// The playing scene, if any.
    pub scene: Option<SceneStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MmView {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub heading_rad: f64,
    pub speed_mps: f64,
    /// Machine the MM is docked at, if any.
    pub at: Option<String>,
    pub motion: Option<MotionView>,
    /// The MM's actions and whether each can be performed right now.
    pub actions: Vec<ActionAvailability>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionAvailability {
    pub id: String,
    pub available: bool,
    /// Why it isn't available (`None` when it is).
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MotionView {
    pub command_id: u64,
    pub target: Place,
    pub remaining_m: f64,
    pub eta_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SensorView {
    pub id: String,
    pub setpoint: f64,
    pub noise_std: f64,
    pub paused: bool,
    pub last_value: Option<f64>,
    pub last_ms: Option<u64>,
}

impl Engine {
    pub fn new(scenario: Arc<Scenario>) -> Self {
        Self::with_planner(scenario, Box::new(StraightLine))
    }

    pub fn with_planner(scenario: Arc<Scenario>, planner: Box<dyn PathPlanner>) -> Self {
        let anchors = scenario
            .sensors
            .iter()
            .map(|sensor| match sensor.host_kind {
                HostKind::MobileManipulator => SensorAnchor::Mm(
                    scenario
                        .mm_index(&sensor.host)
                        .expect("validated: mounted sensors have a host"),
                ),
                HostKind::Machine => SensorAnchor::Fixed(
                    scenario
                        .machine(&sensor.host)
                        .expect("validated: mounted sensors have a host")
                        .position,
                ),
                HostKind::Floor => SensorAnchor::Fixed(
                    sensor
                        .position
                        .expect("validated: floor sensors have a position"),
                ),
            })
            .collect();
        let mut engine = Self {
            rng: StdRng::seed_from_u64(scenario.sim.seed),
            anchors,
            scenario,
            planner,
            sim_ms: 0,
            paused: false,
            seq: 0,
            next_command_id: 1,
            mms: Vec::new(),
            sensors: Vec::new(),
            scene: None,
        };
        engine.reset_world();
        engine
    }

    pub fn scenario(&self) -> &Arc<Scenario> {
        &self.scenario
    }

    pub fn sim_ms(&self) -> u64 {
        self.sim_ms
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// The `sim started` event that opens a run.
    pub fn start(&mut self, ts: u64) -> Vec<SimEvent> {
        let body = self.lifecycle(Lifecycle::Started);
        vec![self.event(ts, body)]
    }

    /// Pause / resume / reset, and playing or stopping scenes. Only `PlayScene` can fail (unknown
    /// scene), and then nothing changes.
    pub fn control(&mut self, control: Control, ts: u64) -> anyhow::Result<Vec<SimEvent>> {
        let body = match control {
            Control::Pause if !self.paused => {
                self.paused = true;
                self.lifecycle(Lifecycle::Paused)
            }
            Control::Resume if self.paused => {
                self.paused = false;
                self.lifecycle(Lifecycle::Resumed)
            }
            Control::Pause | Control::Resume => return Ok(Vec::new()),
            Control::Reset => {
                // `stopped` is stamped with the pre-reset time. Sequence numbers and command ids
                // keep increasing so they stay unique in the stream.
                let mut events = self.stop_scene(ts);
                self.reset_world();
                let body = self.lifecycle(Lifecycle::Reset);
                events.push(self.event(ts, body));
                return Ok(events);
            }
            Control::PlayScene(id) => return self.play_scene(&id, ts),
            Control::StopScene => return Ok(self.stop_scene(ts)),
        };
        Ok(vec![self.event(ts, body)])
    }

    /// Executes a command. On error nothing changes and no events are produced.
    pub fn apply(&mut self, envelope: CommandEnvelope, ts: u64) -> anyhow::Result<Vec<SimEvent>> {
        let origin = envelope.origin;
        let bodies = match envelope.command {
            SimCommand::MoveTo { mm, target } => self.move_to(&mm, target, origin)?,
            SimCommand::Stop { mm } => self.stop(&mm, origin)?,
            SimCommand::SetSensor { sensor, value } => {
                self.set_sensor(&sensor, Some(value), None, None, origin)?
            }
            SimCommand::SetSensorNoise { sensor, std } => {
                self.set_sensor(&sensor, None, Some(std), None, origin)?
            }
            SimCommand::SetSensorPaused { sensor, paused } => {
                self.set_sensor(&sensor, None, None, Some(paused), origin)?
            }
            SimCommand::Perform { mm, action } => self.perform(&mm, &action, origin)?,
        };
        Ok(self.events(ts, bodies))
    }

    /// Advances simulation time by one tick. Does nothing while paused.
    pub fn step(&mut self, ts: u64) -> Vec<SimEvent> {
        if self.paused {
            return Vec::new();
        }
        let scenario = Arc::clone(&self.scenario);
        let tick_ms = scenario.sim.tick_ms;
        let interval_ms = scenario.output.position_interval_ms;
        self.sim_ms += tick_ms;
        let now = self.sim_ms;
        let mut bodies = Vec::new();

        for (def, mm) in scenario.mobile_manipulators.iter().zip(&mut self.mms) {
            let Some(motion) = mm.motion.as_mut() else {
                continue;
            };
            let mut budget = def.max_speed_mps * tick_ms as f64 / 1000.0;
            while budget > 0.0 {
                let Some(&waypoint) = motion.remaining().first() else {
                    break;
                };
                let delta = waypoint - mm.position;
                let distance = delta.length();
                if distance > ARRIVAL_EPS_M {
                    mm.heading = delta.heading();
                }
                if distance <= budget + ARRIVAL_EPS_M {
                    mm.position = waypoint;
                    budget -= distance;
                    motion.next += 1;
                } else {
                    mm.position = mm.position + delta.scale(budget / distance);
                    budget = 0.0;
                }
            }

            if motion.remaining().is_empty() {
                let command_id = motion.command_id;
                let at = motion.target.at.clone();
                mm.motion = None;
                bodies.push(position_body(&def.id, mm, 0.0));
                bodies.push(EventBody::Arrived {
                    entity: def.id.clone(),
                    command_id,
                    x: round_to(mm.position.x, POSITION_DECIMALS),
                    y: round_to(mm.position.y, POSITION_DECIMALS),
                    at,
                });
            } else if now >= motion.next_position_ms {
                while motion.next_position_ms <= now {
                    motion.next_position_ms += interval_ms;
                }
                bodies.push(position_body(&def.id, mm, def.max_speed_mps));
            }
        }

        for ((def, sensor), anchor) in scenario
            .sensors
            .iter()
            .zip(&mut self.sensors)
            .zip(&self.anchors)
        {
            if now < sensor.next_due_ms {
                continue;
            }
            while sensor.next_due_ms <= now {
                sensor.next_due_ms += def.period_ms;
            }
            // Drawn even while paused, so pausing one sensor doesn't change what the others report.
            let noise = if sensor.noise_std > 0.0 {
                Normal::new(0.0, sensor.noise_std)
                    .map(|normal| normal.sample(&mut self.rng))
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            if sensor.paused {
                continue;
            }
            let value = round_to((sensor.setpoint + noise).clamp(def.min, def.max), VALUE_DECIMALS);
            sensor.last_value = Some(value);
            sensor.last_ms = Some(now);
            let at = match *anchor {
                SensorAnchor::Mm(index) => self.mms[index].position,
                SensorAnchor::Fixed(position) => position,
            };
            bodies.push(EventBody::Sensor {
                sensor: def.id.clone(),
                entity: def.host.clone(),
                sensor_kind: def.kind.clone(),
                value,
                unit: def.unit.clone(),
                x: round_to(at.x, POSITION_DECIMALS),
                y: round_to(at.y, POSITION_DECIMALS),
            });
        }

        let mut events = self.events(ts, bodies);
        events.extend(self.run_scene(ts));
        events
    }

    pub fn state(&self) -> WorldState {
        let scenario = &self.scenario;
        WorldState {
            sim_ms: self.sim_ms,
            paused: self.paused,
            mobile_manipulators: scenario
                .mobile_manipulators
                .iter()
                .zip(&self.mms)
                .enumerate()
                .map(|(index, (def, mm))| {
                    let motion = mm.motion.as_ref().map(|motion| {
                        let remaining_m = path_length(mm.position, motion.remaining());
                        MotionView {
                            command_id: motion.command_id,
                            target: motion.target.clone(),
                            remaining_m,
                            eta_ms: eta_ms(remaining_m, def.max_speed_mps),
                        }
                    });
                    MmView {
                        id: def.id.clone(),
                        x: mm.position.x,
                        y: mm.position.y,
                        heading_rad: mm.heading,
                        speed_mps: if motion.is_some() { def.max_speed_mps } else { 0.0 },
                        at: self.docked_at(mm.position),
                        motion,
                        actions: def
                            .actions
                            .iter()
                            .map(|action| {
                                let reason = self.action_blocker(index, action);
                                ActionAvailability {
                                    id: action.clone(),
                                    available: reason.is_none(),
                                    reason,
                                }
                            })
                            .collect(),
                    }
                })
                .collect(),
            sensors: scenario
                .sensors
                .iter()
                .zip(&self.sensors)
                .map(|(def, sensor)| SensorView {
                    id: def.id.clone(),
                    setpoint: sensor.setpoint,
                    noise_std: sensor.noise_std,
                    paused: sensor.paused,
                    last_value: sensor.last_value,
                    last_ms: sensor.last_ms,
                })
                .collect(),
            scene: self.scene_status(),
        }
    }

    // --- Scenes ----------------------------------------------------------------------------

    fn play_scene(&mut self, id: &str, ts: u64) -> anyhow::Result<Vec<SimEvent>> {
        let scenario = Arc::clone(&self.scenario);
        let index = scenario.scene_index(id).ok_or_else(|| {
            let ids = known(scenario.scenes.iter().map(|s| s.id.as_str()));
            anyhow!(
                "Unknown scene '{id}' (known: {})",
                if ids.is_empty() { "none" } else { ids.as_str() }
            )
        })?;
        let mut events = self.stop_scene(ts);
        let player = ScenePlayer::new(index, scenario.scenes[index].tracks.len(), self.sim_ms);
        let body = scene_body(id, player.iteration, SceneState::Started, None);
        events.push(self.event(ts, body));
        self.scene = Some(player);
        events.extend(self.run_scene(ts));
        Ok(events)
    }

    fn stop_scene(&mut self, ts: u64) -> Vec<SimEvent> {
        let Some(player) = self.scene.take() else {
            return Vec::new();
        };
        let id = self.scenario.scenes[player.scene].id.clone();
        let body = scene_body(&id, player.iteration, SceneState::Stopped, None);
        vec![self.event(ts, body)]
    }

    /// Runs every track of the playing scene until it reaches a wait that isn't over (or its end),
    /// then finishes or restarts the scene when all tracks are done.
    fn run_scene(&mut self, ts: u64) -> Vec<SimEvent> {
        // Taken out while running, so commands go through `apply` like any other command.
        let Some(mut player) = self.scene.take() else {
            return Vec::new();
        };
        let scenario = Arc::clone(&self.scenario);
        let def = &scenario.scenes[player.scene];
        let mut events = Vec::new();

        loop {
            for (track, cursor) in def.tracks.iter().zip(player.tracks.iter_mut()) {
                loop {
                    if let Some(wait) = cursor.waiting {
                        let over = match wait {
                            Wait::Until(until_ms) => self.sim_ms >= until_ms,
                            Wait::Idle(mm) => self.mms[mm].motion.is_none(),
                        };
                        if !over {
                            break;
                        }
                        cursor.waiting = None;
                    }
                    let Some(step) = track.steps.get(cursor.next) else {
                        break;
                    };
                    cursor.next += 1;
                    match step {
                        Step::Command(command) => {
                            let envelope = CommandEnvelope::new(Origin::Scene, command.clone());
                            match self.apply(envelope, ts) {
                                Ok(batch) => events.extend(batch),
                                Err(error) => {
                                    // The scene stops; the world keeps running.
                                    let body = scene_body(
                                        &def.id,
                                        player.iteration,
                                        SceneState::Failed,
                                        Some(format!("{error:#}")),
                                    );
                                    events.push(self.event(ts, body));
                                    return events;
                                }
                            }
                        }
                        Step::WaitMs(ms) => cursor.waiting = Some(Wait::Until(self.sim_ms + ms)),
                        Step::WaitIdle(mm) => {
                            let index = scenario.mm_index(mm).expect("validated: scene MMs exist");
                            cursor.waiting = Some(Wait::Idle(index));
                        }
                    }
                }
            }

            let iteration_done = def
                .tracks
                .iter()
                .zip(&player.tracks)
                .all(|(track, cursor)| cursor.waiting.is_none() && cursor.next >= track.steps.len());
            if !iteration_done {
                break;
            }
            let another = match def.repeat {
                Repeat::Once => false,
                Repeat::Times(times) => player.iteration < times,
                Repeat::Forever => true,
            };
            if !another {
                let body = scene_body(&def.id, player.iteration, SceneState::Finished, None);
                events.push(self.event(ts, body));
                return events;
            }
            // At most one iteration starts per tick, so a scene whose waits are already over
            // can't spin within a single tick; it restarts on the next one.
            if player.iteration_started_ms == self.sim_ms {
                break;
            }
            player.restart(self.sim_ms);
            let body = scene_body(&def.id, player.iteration, SceneState::Restarted, None);
            events.push(self.event(ts, body));
        }

        self.scene = Some(player);
        events
    }

    fn scene_status(&self) -> Option<SceneStatus> {
        let player = self.scene.as_ref()?;
        let def = &self.scenario.scenes[player.scene];
        Some(SceneStatus {
            id: def.id.clone(),
            label: def.label.clone(),
            iteration: player.iteration,
            tracks: def
                .tracks
                .iter()
                .zip(&player.tracks)
                .map(|(track, cursor)| {
                    let current = cursor.waiting.map(|wait| {
                        let text = track.steps[cursor.next - 1].describe(&self.scenario);
                        match wait {
                            Wait::Until(until_ms) => format!(
                                "{text} ({} left)",
                                format_seconds(until_ms.saturating_sub(self.sim_ms))
                            ),
                            Wait::Idle(_) => text,
                        }
                    });
                    TrackStatus {
                        name: track.name.clone(),
                        step: cursor.next.min(track.steps.len()),
                        steps: track.steps.len(),
                        done: current.is_none() && cursor.next >= track.steps.len(),
                        current,
                    }
                })
                .collect(),
        })
    }

    // -----------------------------------------------------------------------------------------

    fn reset_world(&mut self) {
        let scenario = Arc::clone(&self.scenario);
        self.sim_ms = 0;
        self.rng = StdRng::seed_from_u64(scenario.sim.seed);
        self.mms = scenario
            .mobile_manipulators
            .iter()
            .map(|def| MmState {
                position: def.start,
                heading: def.start_heading_rad,
                motion: None,
            })
            .collect();
        self.sensors = scenario
            .sensors
            .iter()
            .map(|def| SensorState {
                setpoint: def.default,
                noise_std: def.noise_std,
                paused: false,
                next_due_ms: def.period_ms,
                last_value: None,
                last_ms: None,
            })
            .collect();
    }

    fn move_to(
        &mut self,
        mm_id: &str,
        target: Target,
        origin: Origin,
    ) -> anyhow::Result<Vec<EventBody>> {
        let scenario = Arc::clone(&self.scenario);
        let index = self.mm_index(mm_id)?;
        let def = &scenario.mobile_manipulators[index];

        let (destination, destination_at) = match target {
            Target::Point { x, y } => {
                let point = Vec2::new(x, y);
                if !(point.is_finite() && scenario.reachable_area(def.radius_m).contains(point)) {
                    bail!(
                        "Point ({x:.2}, {y:.2}) is outside the area {} can reach \
                         (the floor minus its {} m radius)",
                        def.id,
                        def.radius_m
                    );
                }
                (point, self.docked_at(point))
            }
            Target::Machine(machine_id) => {
                let machine = scenario.machine(&machine_id).ok_or_else(|| {
                    anyhow!(
                        "Unknown machine '{machine_id}' (known: {})",
                        known(scenario.machines.iter().map(|m| m.id.as_str()))
                    )
                })?;
                (machine.dock, Some(machine.id.clone()))
            }
        };

        let mm = &self.mms[index];
        let from = Place {
            x: round_to(mm.position.x, POSITION_DECIMALS),
            y: round_to(mm.position.y, POSITION_DECIMALS),
            at: self.docked_at(mm.position),
        };
        let to = Place {
            x: round_to(destination.x, POSITION_DECIMALS),
            y: round_to(destination.y, POSITION_DECIMALS),
            at: destination_at,
        };
        let waypoints = self.planner.plan(mm.position, destination, &scenario);
        let distance_m = path_length(mm.position, &waypoints);
        let replaces = mm.motion.as_ref().map(|motion| motion.command_id);
        let command_id = self.next_command_id;
        self.next_command_id += 1;

        let mut bodies = vec![EventBody::MoveCommand {
            entity: def.id.clone(),
            command_id,
            origin,
            from,
            to: to.clone(),
            distance_m: round_to(distance_m, POSITION_DECIMALS),
            eta_ms: eta_ms(distance_m, def.max_speed_mps),
            replaces,
        }];

        let mm = &mut self.mms[index];
        if distance_m <= ARRIVAL_EPS_M {
            mm.motion = None;
            bodies.push(EventBody::Arrived {
                entity: def.id.clone(),
                command_id,
                x: to.x,
                y: to.y,
                at: to.at,
            });
        } else {
            mm.motion = Some(Motion {
                command_id,
                waypoints,
                next: 0,
                target: to,
                next_position_ms: self.sim_ms + scenario.output.position_interval_ms,
            });
        }
        Ok(bodies)
    }

    fn stop(&mut self, mm_id: &str, origin: Origin) -> anyhow::Result<Vec<EventBody>> {
        let index = self.mm_index(mm_id)?;
        let Some(motion) = self.mms[index].motion.take() else {
            return Ok(Vec::new());
        };
        let position = self.mms[index].position;
        Ok(vec![EventBody::Stopped {
            entity: mm_id.to_string(),
            command_id: motion.command_id,
            x: round_to(position.x, POSITION_DECIMALS),
            y: round_to(position.y, POSITION_DECIMALS),
            at: self.docked_at(position),
            origin,
        }])
    }

    fn set_sensor(
        &mut self,
        sensor_id: &str,
        setpoint: Option<f64>,
        noise_std: Option<f64>,
        paused: Option<bool>,
        origin: Origin,
    ) -> anyhow::Result<Vec<EventBody>> {
        let scenario = Arc::clone(&self.scenario);
        let index = scenario.sensor_index(sensor_id).ok_or_else(|| {
            anyhow!(
                "Unknown sensor '{sensor_id}' (known: {})",
                known(scenario.sensors.iter().map(|s| s.id.as_str()))
            )
        })?;
        let def = &scenario.sensors[index];
        if let Some(value) = setpoint
            && !(value.is_finite() && value >= def.min && value <= def.max)
        {
            bail!(
                "Value {value} for '{sensor_id}' is outside its range [{}, {}]",
                def.min,
                def.max
            );
        }
        if let Some(std) = noise_std
            && !(std.is_finite() && std >= 0.0)
        {
            bail!("Noise for '{sensor_id}' must be >= 0 (got {std})");
        }

        let sensor = &mut self.sensors[index];
        let previous_setpoint = sensor.setpoint;
        if let Some(value) = setpoint {
            sensor.setpoint = value;
        }
        if let Some(std) = noise_std {
            sensor.noise_std = std;
        }
        if let Some(paused) = paused {
            sensor.paused = paused;
        }
        Ok(vec![EventBody::SensorSet {
            sensor: def.id.clone(),
            entity: def.host.clone(),
            sensor_kind: def.kind.clone(),
            setpoint: sensor.setpoint,
            noise_std: sensor.noise_std,
            previous_setpoint,
            paused: sensor.paused,
            origin,
        }])
    }

    fn perform(
        &mut self,
        mm_id: &str,
        action: &str,
        origin: Origin,
    ) -> anyhow::Result<Vec<EventBody>> {
        let index = self.mm_index(mm_id)?;
        if let Some(reason) = self.action_blocker(index, action) {
            bail!(reason);
        }
        let command_id = self.next_command_id;
        self.next_command_id += 1;
        let position = self.mms[index].position;
        Ok(vec![EventBody::Action {
            entity: mm_id.to_string(),
            action: action.to_string(),
            command_id,
            x: round_to(position.x, POSITION_DECIMALS),
            y: round_to(position.y, POSITION_DECIMALS),
            at: self.docked_at(position),
            origin,
        }])
    }

    /// Why the MM can't perform `action` right now, or `None` if it can. The single source of the
    /// action rules: used to reject commands and to tell the UI which buttons to enable.
    fn action_blocker(&self, index: usize, action_id: &str) -> Option<String> {
        let scenario = &self.scenario;
        let def = &scenario.mobile_manipulators[index];
        let Some(action) = scenario.action(action_id) else {
            return Some(format!(
                "Unknown action '{action_id}' (known: {})",
                known(scenario.actions.iter().map(|a| a.id.as_str()))
            ));
        };
        if !def.actions.iter().any(|id| id == action_id) {
            let own = known(def.actions.iter().map(String::as_str));
            return Some(format!(
                "{} cannot perform '{action_id}' (its actions: {})",
                def.id,
                if own.is_empty() { "none" } else { own.as_str() }
            ));
        }
        let mm = &self.mms[index];
        if mm.motion.is_some() && !action.allowed_while_moving {
            return Some(format!("'{action_id}' needs {} to be stopped", def.id));
        }
        if action.requires_machine && self.docked_at(mm.position).is_none() {
            return Some(format!(
                "'{action_id}' needs {} at a machine; it is at ({:.1}, {:.1})",
                def.id, mm.position.x, mm.position.y
            ));
        }
        None
    }

    fn mm_index(&self, mm_id: &str) -> anyhow::Result<usize> {
        self.scenario.mm_index(mm_id).ok_or_else(|| {
            anyhow!(
                "Unknown mobile manipulator '{mm_id}' (known: {})",
                known(self.scenario.mobile_manipulators.iter().map(|m: &MmDef| m.id.as_str()))
            )
        })
    }

    fn docked_at(&self, point: Vec2) -> Option<String> {
        self.scenario
            .machine_docked_at(point)
            .map(|machine| machine.id.clone())
    }

    fn lifecycle(&self, state: Lifecycle) -> EventBody {
        EventBody::Sim {
            state,
            scenario: self.scenario.name.clone(),
        }
    }

    fn event(&mut self, ts: u64, body: EventBody) -> SimEvent {
        self.seq += 1;
        SimEvent {
            v: SCHEMA_VERSION,
            seq: self.seq,
            ts,
            sim_ms: self.sim_ms,
            body,
        }
    }

    fn events(&mut self, ts: u64, bodies: Vec<EventBody>) -> Vec<SimEvent> {
        bodies.into_iter().map(|body| self.event(ts, body)).collect()
    }
}

fn position_body(id: &str, mm: &MmState, speed_mps: f64) -> EventBody {
    EventBody::Position {
        entity: id.to_string(),
        x: round_to(mm.position.x, POSITION_DECIMALS),
        y: round_to(mm.position.y, POSITION_DECIMALS),
        heading_rad: round_to(mm.heading, HEADING_DECIMALS),
        speed_mps,
    }
}

fn scene_body(id: &str, iteration: u32, state: SceneState, error: Option<String>) -> EventBody {
    EventBody::Scene {
        scene: id.to_string(),
        state,
        iteration,
        error,
    }
}

fn eta_ms(distance_m: f64, speed_mps: f64) -> u64 {
    (distance_m / speed_mps * 1000.0).round() as u64
}

fn known<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    ids.collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests;
