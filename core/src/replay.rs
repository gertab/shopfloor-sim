//! Replaying Liminal Desk traces instead of simulating.
//!
//! A [`Replay`] sends the payloads of one or more traces at the times their delays give, and
//! nothing else: no physics, no periodic sensor readings and, unless asked for, no position
//! reports. Like [`Engine`](crate::engine::Engine) it is synchronous and deterministic: a runtime
//! (or a fast run) calls [`Replay::step`] once per tick.
//!
//! The payloads also drive the picture. On the scenario's layout, MMs glide out of a dock on
//! `undock`, drive on `move` and glide in on `dock` (all three still count as actions or moves for
//! routing), and sensor payloads update the scenario's sensors. Building that picture is also where
//! a trace is checked against the scenario (see [`Finding`]).

use crate::command::Control;
use crate::engine::{ActionAvailability, MmView, MotionView, Place, SensorView, WorldState};
use crate::event::{EventBody, SCHEMA_VERSION, SimEvent};
use crate::geometry::{Vec2, round_to};
use crate::scenario::{MachineDef, Scenario};
use crate::sinks::{EventSink, Message, to_json_line};
use crate::trace::{Payload, Trace, load_trace, merge};
use anyhow::bail;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Where an MM waits beside a machine: this far out from its dock, away from the machine, leaning to
/// one side (alternating by MM) so that two MMs waiting at the same machine stand apart.
const APPROACH_M: f64 = 1.0;
const APPROACH_LEAN_RAD: f64 = std::f64::consts::PI / 6.0;
/// How long docking and undocking take at most: the MM glides between its approach point and the
/// dock.
const GLIDE_MS: u64 = 1000;
/// Why nothing can be done by hand during a replay.
pub const READ_ONLY: &str = "Replaying a trace: manual control is off";

/// What a replayed payload looks like on the sinks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// As written in the trace (compacted to one line), exactly what Liminal Desk sends.
    #[default]
    Payload,
    /// The payload with `v`, `seq`, `sim_ms` and `kind` in front (and `timestamp_ms` if it has none).
    Envelope,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayOptions {
    pub format: Format,
    /// Also report the positions of moving MMs, as the simulator does.
    pub positions: bool,
}

/// Progress of a replay, for the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplayStatus {
    pub traces: Vec<String>,
    pub format: Format,
    pub positions: bool,
    /// Payloads sent so far, of `total`.
    pub sent: usize,
    pub total: usize,
    pub duration_ms: u64,
    pub done: bool,
}

/// Something in a trace that doesn't fit the scenario. The replay still runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub trace: String,
    pub message: String,
    /// The 1-based steps it applies to (empty for the trace as a whole).
    pub steps: Vec<usize>,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.steps.as_slice() {
            [] => write!(f, "{}: {}", self.trace, self.message),
            [step] => write!(f, "{}: step {step}: {}", self.trace, self.message),
            steps => {
                let shown: Vec<String> = steps.iter().take(3).map(usize::to_string).collect();
                let more = if steps.len() > 3 { ", …" } else { "" };
                write!(
                    f,
                    "{}: {} ({} times: steps {}{more})",
                    self.trace,
                    self.message,
                    steps.len(),
                    shown.join(", ")
                )
            }
        }
    }
}

/// Findings, with repeats of the same message in the same trace grouped together.
#[derive(Default)]
struct Findings(Vec<Finding>);

impl Findings {
    fn add(&mut self, trace: &str, step: Option<usize>, message: String) {
        match self
            .0
            .iter_mut()
            .find(|f| f.trace == trace && f.message == message)
        {
            Some(finding) => finding.steps.extend(step),
            None => self.0.push(Finding {
                trace: trace.to_string(),
                message,
                steps: step.into_iter().collect(),
            }),
        }
    }
}

/// A send step on the merged timeline.
struct Item {
    at_ms: u64,
    trace: usize,
    step: usize,
    /// The payload as written, compacted.
    json: String,
    payload: Payload,
    kind: Option<&'static str>,
    /// The scenario sensor a sensor payload reports for.
    sensor: Option<usize>,
}

/// Where an MM is over time, built from its trace events.
#[derive(Debug, Clone, Default)]
struct Track {
    /// Positions at given times; between two keys the MM moves in a straight line.
    keys: Vec<(u64, Vec2)>,
    /// From when the MM is docked at a machine (or not).
    docked: Vec<(u64, Option<String>)>,
    moves: Vec<MoveSpan>,
}

#[derive(Debug, Clone)]
struct MoveSpan {
    start_ms: u64,
    end_ms: u64,
    command_id: u64,
    target: Place,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    position: Vec2,
    heading: Option<f64>,
    speed_mps: f64,
    moving: bool,
}

impl Track {
    fn sample(&self, t: u64) -> Sample {
        let keys = &self.keys;
        let i = keys.iter().rposition(|(at, _)| *at <= t).unwrap_or(0);
        let heading = |upto: usize| {
            (1..=upto.min(keys.len() - 1))
                .rev()
                .map(|j| keys[j].1 - keys[j - 1].1)
                .find(|delta| delta.length() > 1e-9)
                .map(Vec2::heading)
        };
        if let Some(&(b, pb)) = keys.get(i + 1) {
            let (a, pa) = keys[i];
            let delta = pb - pa;
            if b > a && t >= a && delta.length() > 1e-9 {
                let fraction = (t - a) as f64 / (b - a) as f64;
                return Sample {
                    position: pa + delta.scale(fraction),
                    heading: Some(delta.heading()),
                    speed_mps: delta.length() / ((b - a) as f64 / 1000.0),
                    moving: true,
                };
            }
        }
        Sample {
            position: keys[i].1,
            heading: heading(i),
            speed_mps: 0.0,
            moving: false,
        }
    }

    fn docked_at(&self, t: u64) -> Option<String> {
        self.docked
            .iter()
            .rev()
            .find(|(at, _)| *at <= t)
            .and_then(|(_, machine)| machine.clone())
    }

    fn docked_now(&self) -> Option<String> {
        self.docked.last().and_then(|(_, machine)| machine.clone())
    }

    /// Stops whatever motion is planned beyond `t` and returns where the MM is then.
    fn cut(&mut self, t: u64) -> Vec2 {
        let position = self.sample(t).position;
        self.keys.retain(|(at, _)| *at <= t);
        self.go(t, position);
        for span in &mut self.moves {
            span.end_ms = span.end_ms.min(t);
        }
        position
    }

    /// The MM is at `position` at time `t` (a key, unless it already is one).
    fn go(&mut self, t: u64, position: Vec2) {
        if self.keys.last() != Some(&(t, position)) {
            self.keys.push((t, position));
        }
    }
}

pub struct Replay {
    scenario: Arc<Scenario>,
    options: ReplayOptions,
    trace_names: Vec<String>,
    timeline: Vec<Item>,
    duration_ms: u64,
    /// One per scenario MM, in the same order.
    tracks: Vec<Track>,
    /// Epoch ms of offset 0: from the traces' `timestamp_ms`, else when the replay started.
    time_base: Option<u64>,
    clock_base: u64,
    // Playback.
    sim_ms: u64,
    paused: bool,
    next: usize,
    seq: u64,
    readings: Vec<(Option<f64>, Option<u64>)>,
    moving: Vec<bool>,
    next_position_ms: u64,
}

impl Replay {
    /// Loads the trace files and builds a replay of them on `scenario`.
    pub fn from_files(
        scenario: Arc<Scenario>,
        paths: &[PathBuf],
        options: ReplayOptions,
        start_ts: u64,
    ) -> anyhow::Result<(Self, Vec<Finding>)> {
        let traces = paths
            .iter()
            .map(|path| load_trace(path))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Self::new(scenario, traces, options, start_ts)
    }

    /// Builds a replay of `traces` on `scenario`, checking them against it. `start_ts` (epoch ms)
    /// stamps what the replay adds if the traces carry no `timestamp_ms`.
    pub fn new(
        scenario: Arc<Scenario>,
        traces: Vec<Trace>,
        options: ReplayOptions,
        start_ts: u64,
    ) -> anyhow::Result<(Self, Vec<Finding>)> {
        if traces.is_empty() {
            bail!("No traces to replay");
        }
        let mut findings = Findings::default();
        let timeline: Vec<Item> = merge(&traces)
            .into_iter()
            .map(|timed| {
                let payload = Payload::read(&timed.json);
                let kind = payload.kind();
                let sensor = match (&payload.device_id, &payload.sensor_type, kind) {
                    (Some(device), Some(sensor_type), Some("sensor")) => scenario
                        .sensors
                        .iter()
                        .position(|s| &s.host == device && &s.kind == sensor_type),
                    _ => None,
                };
                Item {
                    at_ms: timed.at_ms,
                    trace: timed.trace,
                    step: timed.step,
                    json: timed.json,
                    payload,
                    kind,
                    sensor,
                }
            })
            .collect();
        let trace_names: Vec<String> = traces.iter().map(|trace| trace.name.clone()).collect();

        if options.format == Format::Envelope
            && let Some(item) = timeline.iter().find(|item| !item.payload.is_object)
        {
            bail!(
                "{}: step {}: the envelope format needs JSON object payloads",
                trace_names[item.trace],
                item.step
            );
        }
        check_payloads(&scenario, &timeline, &trace_names, &mut findings);
        let time_base = check_timestamps(&timeline, &trace_names, &mut findings);
        let mut command_ids = 0;
        let tracks = scenario
            .mobile_manipulators
            .iter()
            .enumerate()
            .map(|(index, def)| {
                build_track(
                    &scenario,
                    index,
                    def,
                    &timeline,
                    &trace_names,
                    &mut command_ids,
                    &mut findings,
                )
            })
            .collect::<Vec<_>>();

        let last_key = tracks
            .iter()
            .filter_map(|track| track.keys.last().map(|(at, _)| *at))
            .max()
            .unwrap_or(0);
        let duration_ms = traces
            .iter()
            .map(Trace::duration_ms)
            .max()
            .unwrap_or(0)
            .max(last_key);
        let mms = scenario.mobile_manipulators.len();
        let sensors = scenario.sensors.len();
        let next_position_ms = scenario.output.position_interval_ms;
        let replay = Self {
            options,
            trace_names,
            timeline,
            duration_ms,
            tracks,
            time_base,
            clock_base: time_base.unwrap_or(start_ts),
            sim_ms: 0,
            paused: false,
            next: 0,
            seq: 0,
            readings: vec![(None, None); sensors],
            moving: vec![false; mms],
            next_position_ms,
            scenario,
        };
        Ok((replay, findings.0))
    }

    pub fn scenario(&self) -> &Arc<Scenario> {
        &self.scenario
    }

    /// Every payload has been sent and every MM has stopped.
    pub fn is_done(&self) -> bool {
        self.next >= self.timeline.len() && self.sim_ms >= self.duration_ms
    }

    /// What is sent at offset 0, before the first tick.
    pub fn start(&mut self) -> Vec<Message> {
        self.due()
    }

    /// Advances by one tick (`sim.tick_ms`) and returns what is sent. Nothing while paused, and
    /// time stops at the end.
    pub fn step(&mut self) -> Vec<Message> {
        if self.paused || self.is_done() {
            return Vec::new();
        }
        self.sim_ms = (self.sim_ms + self.scenario.sim.tick_ms).min(self.duration_ms);
        let mut messages = Vec::new();
        if self.options.positions {
            messages.extend(self.positions());
        }
        messages.extend(self.due());
        messages
    }

    /// Pause, resume, or start again (reset). Scenes can't be played during a replay. Unlike the
    /// simulator, these add nothing to the output: it holds only what the traces say.
    pub fn control(&mut self, control: Control, now_ms: u64) -> anyhow::Result<Vec<Message>> {
        match control {
            Control::Pause => self.paused = true,
            Control::Resume => self.paused = false,
            Control::Reset => {
                self.sim_ms = 0;
                self.next = 0;
                self.readings.fill((None, None));
                self.moving.fill(false);
                self.next_position_ms = self.scenario.output.position_interval_ms;
                self.clock_base = self.time_base.unwrap_or(now_ms);
                return Ok(self.due());
            }
            Control::PlayScene(_) | Control::StopScene => bail!("{READ_ONLY} (no scenes)"),
        }
        Ok(Vec::new())
    }

    pub fn state(&self) -> WorldState {
        let scenario = &self.scenario;
        let t = self.sim_ms;
        WorldState {
            sim_ms: t,
            paused: self.paused,
            mobile_manipulators: scenario
                .mobile_manipulators
                .iter()
                .zip(&self.tracks)
                .map(|(def, track)| {
                    let sample = track.sample(t);
                    let motion = track
                        .moves
                        .iter()
                        .find(|span| span.start_ms <= t && t < span.end_ms)
                        .map(|span| MotionView {
                            command_id: span.command_id,
                            target: span.target.clone(),
                            remaining_m: sample.position.distance(Vec2::new(span.target.x, span.target.y)),
                            eta_ms: span.end_ms - t,
                        });
                    MmView {
                        id: def.id.clone(),
                        x: sample.position.x,
                        y: sample.position.y,
                        heading_rad: sample.heading.unwrap_or(def.start_heading_rad),
                        speed_mps: sample.speed_mps,
                        at: track.docked_at(t),
                        motion,
                        actions: def
                            .actions
                            .iter()
                            .map(|id| ActionAvailability {
                                id: id.clone(),
                                available: false,
                                reason: Some(READ_ONLY.to_string()),
                            })
                            .collect(),
                    }
                })
                .collect(),
            sensors: scenario
                .sensors
                .iter()
                .zip(&self.readings)
                .map(|(def, (last_value, last_ms))| SensorView {
                    id: def.id.clone(),
                    setpoint: def.default,
                    noise_std: def.noise_std,
                    paused: false,
                    last_value: *last_value,
                    last_ms: *last_ms,
                })
                .collect(),
            scene: None,
            replay: Some(ReplayStatus {
                traces: self.trace_names.clone(),
                format: self.options.format,
                positions: self.options.positions,
                sent: self.next,
                total: self.timeline.len(),
                duration_ms: self.duration_ms,
                done: self.is_done(),
            }),
        }
    }

    /// The payloads due by now.
    fn due(&mut self) -> Vec<Message> {
        let mut messages = Vec::new();
        while let Some(item) = self.timeline.get(self.next)
            && item.at_ms <= self.sim_ms
        {
            if let Some(sensor) = item.sensor {
                self.readings[sensor] = (item.payload.value, Some(item.at_ms));
            }
            self.seq += 1;
            let json = match self.options.format {
                Format::Payload => item.json.clone(),
                Format::Envelope => envelope(
                    self.seq,
                    item.at_ms,
                    item.kind,
                    item.payload
                        .timestamp_ms
                        .is_none()
                        .then_some(self.clock_base + item.at_ms),
                    &item.json,
                ),
            };
            messages.push(Message {
                seq: self.seq,
                sim_ms: item.at_ms,
                kind: item.kind,
                device_id: item.payload.device_id.clone(),
                json,
            });
            self.next += 1;
        }
        messages
    }

    /// Positions of moving MMs every `position_interval_ms`, and once more when each stops.
    fn positions(&mut self) -> Vec<Message> {
        let due = self.sim_ms >= self.next_position_ms;
        while self.next_position_ms <= self.sim_ms {
            self.next_position_ms += self.scenario.output.position_interval_ms;
        }
        let mut messages = Vec::new();
        for index in 0..self.tracks.len() {
            let sample = self.tracks[index].sample(self.sim_ms);
            if sample.moving {
                if due {
                    messages.push(self.position(index, sample));
                }
                self.moving[index] = true;
            } else if self.moving[index] {
                messages.push(self.position(index, sample));
                self.moving[index] = false;
            }
        }
        messages
    }

    fn position(&mut self, index: usize, sample: Sample) -> Message {
        let def = &self.scenario.mobile_manipulators[index];
        self.seq += 1;
        let timestamp_ms = self.clock_base + self.sim_ms;
        let (x, y) = (round_to(sample.position.x, 3), round_to(sample.position.y, 3));
        let heading_rad = round_to(sample.heading.unwrap_or(def.start_heading_rad), 4);
        let speed_mps = round_to(sample.speed_mps, 3);
        let json = match self.options.format {
            Format::Envelope => to_json_line(&SimEvent {
                v: SCHEMA_VERSION,
                seq: self.seq,
                timestamp_ms,
                sim_ms: self.sim_ms,
                body: EventBody::Position {
                    device_id: def.id.clone(),
                    x,
                    y,
                    heading_rad,
                    speed_mps,
                },
            }),
            Format::Payload => serde_json::to_string(&PositionPayload {
                device_id: &def.id,
                x,
                y,
                heading_rad,
                speed_mps,
                timestamp_ms,
            })
            .expect("positions always serialise"),
        };
        Message {
            seq: self.seq,
            sim_ms: self.sim_ms,
            kind: Some("position"),
            device_id: Some(def.id.clone()),
            json,
        }
    }
}

/// A position report in the traces' style.
#[derive(Serialize)]
struct PositionPayload<'a> {
    device_id: &'a str,
    x: f64,
    y: f64,
    heading_rad: f64,
    speed_mps: f64,
    timestamp_ms: u64,
}

/// `{"v":2,"seq":..,"sim_ms":..,"kind":..}` in front of the payload's own keys, kept as written.
/// `payload` must be a compact JSON object.
pub fn envelope(
    seq: u64,
    sim_ms: u64,
    kind: Option<&str>,
    timestamp_ms: Option<u64>,
    payload: &str,
) -> String {
    let kind = kind.map_or_else(|| "null".to_string(), |kind| format!("\"{kind}\""));
    let mut json = format!(r#"{{"v":{SCHEMA_VERSION},"seq":{seq},"sim_ms":{sim_ms},"kind":{kind}"#);
    if let Some(timestamp_ms) = timestamp_ms {
        json.push_str(&format!(r#","timestamp_ms":{timestamp_ms}"#));
    }
    let inner = payload
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or_default();
    if !inner.is_empty() {
        json.push(',');
        json.push_str(inner);
    }
    json.push('}');
    json
}

/// Plays a whole replay as fast as possible into `sinks` (closed at the end). Returns how many
/// messages were written.
pub fn run_fast(replay: &mut Replay, sinks: &mut [Box<dyn EventSink>]) -> anyhow::Result<u64> {
    let mut written = 0u64;
    let mut write = |batch: Vec<Message>| -> anyhow::Result<()> {
        if !batch.is_empty() {
            for sink in sinks.iter_mut() {
                sink.write_batch(&batch)?;
            }
            written += batch.len() as u64;
        }
        Ok(())
    };
    write(replay.start())?;
    while !replay.is_done() {
        write(replay.step())?;
    }
    for sink in sinks.iter_mut() {
        sink.close()?;
    }
    Ok(written)
}

/// Checks each payload on its own against the scenario.
fn check_payloads(scenario: &Scenario, timeline: &[Item], names: &[String], findings: &mut Findings) {
    let known_device = |id: &str| {
        scenario.mm_index(id).is_some()
            || scenario.machine(id).is_some()
            || scenario.sensors.iter().any(|sensor| sensor.host == id)
    };
    for item in timeline {
        let (trace, step) = (&names[item.trace], Some(item.step));
        let payload = &item.payload;
        if !payload.is_object {
            findings.add(trace, step, "payload is not a JSON object".into());
            continue;
        }
        match &payload.device_id {
            None => findings.add(trace, step, "payload has no device_id".into()),
            Some(id) if !known_device(id) => {
                findings.add(trace, step, format!("device '{id}' is not in the scenario"))
            }
            Some(_) => {}
        }
        if let Some(event) = &payload.event
            && !matches!(event.as_str(), "move" | "dock" | "undock")
            && scenario.action(event).is_none()
        {
            findings.add(
                trace,
                step,
                format!("event '{event}' is not one of the scenario's actions"),
            );
        }
        for machine in [&payload.target, &payload.source, &payload.destination]
            .into_iter()
            .flatten()
        {
            if scenario.machine(machine).is_none() {
                findings.add(trace, step, format!("unknown machine '{machine}'"));
            }
        }
        if item.kind == Some("sensor")
            && let (Some(device), Some(sensor_type)) = (&payload.device_id, &payload.sensor_type)
        {
            match item.sensor.map(|index| &scenario.sensors[index]) {
                None => findings.add(
                    trace,
                    step,
                    format!("no '{sensor_type}' sensor on '{device}' in the scenario"),
                ),
                Some(sensor) => {
                    if let Some(unit) = &payload.unit
                        && unit != &sensor.unit
                    {
                        findings.add(
                            trace,
                            step,
                            format!("unit '{unit}' differs from {}'s '{}'", sensor.id, sensor.unit),
                        );
                    }
                }
            }
        }
    }
}

/// Checks each trace's `timestamp_ms` against its delays, and the traces' starts against each
/// other. Returns the epoch ms of offset 0 (from the first trace that has timestamps).
fn check_timestamps(timeline: &[Item], names: &[String], findings: &mut Findings) -> Option<u64> {
    let mut bases: Vec<Option<u64>> = vec![None; names.len()];
    for item in timeline {
        let Some(timestamp) = item.payload.timestamp_ms else {
            continue;
        };
        match bases[item.trace] {
            None => bases[item.trace] = Some(timestamp.saturating_sub(item.at_ms)),
            Some(base) if base + item.at_ms != timestamp => findings.add(
                &names[item.trace],
                Some(item.step),
                "timestamp_ms disagrees with the trace's delays".into(),
            ),
            Some(_) => {}
        }
    }
    let first = bases
        .iter()
        .enumerate()
        .find_map(|(index, base)| base.map(|base| (index, base)));
    if let Some((first_index, first_base)) = first {
        for (index, base) in bases.iter().enumerate() {
            if let Some(base) = base
                && *base != first_base
            {
                findings.add(
                    &names[index],
                    None,
                    format!(
                        "starts at timestamp_ms {base}, {} ms {} {} ({first_base}); the replay plays both from 0",
                        base.abs_diff(first_base),
                        if *base > first_base { "after" } else { "before" },
                        names[first_index]
                    ),
                );
            }
        }
    }
    first.map(|(_, base)| base)
}

/// Where MM number `mm_index` waits beside `machine` (see [`APPROACH_M`]).
fn approach(scenario: &Scenario, machine: &MachineDef, mm_index: usize, radius_m: f64) -> Vec2 {
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
    scenario
        .reachable_area(radius_m)
        .clamp(machine.dock + direction.scale(APPROACH_M))
}

/// Where `def` is over time: it glides out of a dock on `undock`, drives between approach points
/// on `move`, and glides into a dock on `dock`. Also checks that its events follow on from each
/// other.
fn build_track(
    scenario: &Scenario,
    mm_index: usize,
    def: &crate::scenario::MmDef,
    timeline: &[Item],
    names: &[String],
    command_ids: &mut u64,
    findings: &mut Findings,
) -> Track {
    let id = def.id.as_str();
    let events: Vec<&Item> = timeline
        .iter()
        .filter(|item| item.payload.device_id.as_deref() == Some(id) && item.payload.event.is_some())
        .collect();
    let machine_of = |name: &Option<String>| name.as_deref().and_then(|name| scenario.machine(name));
    let approach_of = |machine: &MachineDef| approach(scenario, machine, mm_index, def.radius_m);

    // Before its first event, the MM is where that event implies (docked before an undock, at the
    // approach point before a move or a dock), else at its start.
    let first = events.iter().find_map(|item| {
        let payload = &item.payload;
        match payload.event.as_deref() {
            Some("undock") => machine_of(&payload.target).map(|m| (m.dock, Some(m.id.clone()))),
            Some("dock") => machine_of(&payload.target).map(|m| (approach_of(m), None)),
            Some("move") => machine_of(&payload.source).map(|m| (approach_of(m), None)),
            _ => None,
        }
    });
    let (start, docked) = first.unwrap_or_else(|| {
        let docked = scenario
            .machine_docked_at(def.start)
            .map(|machine| machine.id.clone());
        (def.start, docked)
    });
    let mut track = Track {
        keys: vec![(0, start)],
        docked: vec![(0, docked)],
        moves: Vec::new(),
    };
    let mut last_machine = track.docked_now();
    let mut heading_to: Option<String> = None;

    for (index, item) in events.iter().enumerate() {
        let (trace, step) = (&names[item.trace], Some(item.step));
        let t = item.at_ms;
        let next_t = events.get(index + 1).map(|next| next.at_ms);
        // Docking and undocking take a second, or less if the MM's next event comes sooner.
        let glide = next_t.map_or(GLIDE_MS, |next| (next - t).min(GLIDE_MS));
        let payload = &item.payload;
        let event = payload.event.as_deref().unwrap_or_default();
        match event {
            "undock" => {
                let Some(machine) = payload.target.as_deref().and_then(|m| scenario.machine(m)) else {
                    continue; // unknown machines are reported by check_payloads
                };
                match track.docked_now() {
                    Some(docked) if docked == machine.id => {}
                    Some(docked) => findings.add(
                        trace,
                        step,
                        format!("{id} undocks from '{}' but is docked at '{docked}'", machine.id),
                    ),
                    None => findings.add(
                        trace,
                        step,
                        format!("{id} undocks from '{}' but isn't docked", machine.id),
                    ),
                }
                track.cut(t);
                track.go(t + glide, approach_of(machine));
                track.docked.push((t, None));
                last_machine = Some(machine.id.clone());
            }
            "move" => {
                let from = track.cut(t);
                if let Some(source) = &payload.source
                    && scenario.machine(source).is_some()
                    && let Some(last) = &last_machine
                    && last != source
                {
                    findings.add(
                        trace,
                        step,
                        format!("{id} moves from '{source}' but was last at '{last}'"),
                    );
                }
                if let Some(docked) = track.docked_now() {
                    findings.add(
                        trace,
                        step,
                        format!("{id} moves while docked at '{docked}' (no undock)"),
                    );
                    track.docked.push((t, None));
                }
                let Some(destination) = payload.destination.as_deref().and_then(|m| scenario.machine(m))
                else {
                    continue;
                };
                // To the destination's approach point; the dock glides it the rest of the way.
                let target = approach_of(destination);
                let arrive = match next_t {
                    Some(next) => next,
                    None => t + (from.distance(target) / def.max_speed_mps * 1000.0).round() as u64,
                };
                track.go(arrive, target);
                *command_ids += 1;
                track.moves.push(MoveSpan {
                    start_ms: t,
                    end_ms: arrive,
                    command_id: *command_ids,
                    target: Place {
                        x: target.x,
                        y: target.y,
                        at: Some(destination.id.clone()),
                    },
                });
                heading_to = Some(destination.id.clone());
            }
            "dock" => {
                let Some(machine) = payload.target.as_deref().and_then(|m| scenario.machine(m)) else {
                    continue;
                };
                if let Some(docked) = track.docked_now() {
                    findings.add(
                        trace,
                        step,
                        format!(
                            "{id} docks at '{}' but is already docked at '{docked}'",
                            machine.id
                        ),
                    );
                }
                if let Some(destination) = &heading_to
                    && destination != &machine.id
                {
                    findings.add(
                        trace,
                        step,
                        format!("{id} docks at '{}' but was moving to '{destination}'", machine.id),
                    );
                }
                track.cut(t);
                track.go(t + glide, machine.dock);
                track.docked.push((t, Some(machine.id.clone())));
                last_machine = Some(machine.id.clone());
                heading_to = None;
            }
            action => {
                if let Some(action_def) = scenario.action(action)
                    && action_def.requires_machine
                    && track.docked_now().is_none()
                {
                    findings.add(trace, step, format!("{id} does '{action}' while not docked"));
                }
            }
        }
    }
    track
}

#[cfg(test)]
mod tests;
