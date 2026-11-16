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
use crate::engine::{ActionAvailability, MmView, MotionView, SensorView, WorldState};
use crate::event::{EventBody, HEADING_DECIMALS, POSITION_DECIMALS, SCHEMA_VERSION, SimEvent};
use crate::geometry::{Vec2, round_to};
use crate::scenario::Scenario;
use crate::sinks::{EventSink, Message, to_json_line};
use crate::trace::{Payload, Trace, load_trace, merge};
use anyhow::bail;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

mod checks;
mod track;

pub use checks::Finding;
use checks::{Findings, check_payloads, check_timestamps};
use track::{Sample, Track, build_track};
use ts_rs::TS;

/// Why nothing can be done by hand during a replay.
pub const READ_ONLY: &str = "Replaying a trace: manual control is off";

/// What a replayed payload looks like on the sinks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, rename = "ReplayFormat")]
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
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
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
        Ok((replay, findings.into_vec()))
    }

    pub fn scenario(&self) -> &Arc<Scenario> {
        &self.scenario
    }

    /// Replay time: milliseconds from the start of the traces.
    pub fn sim_ms(&self) -> u64 {
        self.sim_ms
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
        let (x, y) = (
            round_to(sample.position.x, POSITION_DECIMALS),
            round_to(sample.position.y, POSITION_DECIMALS),
        );
        let heading_rad = round_to(sample.heading.unwrap_or(def.start_heading_rad), HEADING_DECIMALS);
        let speed_mps = round_to(sample.speed_mps, POSITION_DECIMALS);
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
    let (written, ()) = crate::headless::drive(replay, 0, Vec::new(), sinks, |replay, _| {
        replay.is_done().then_some(())
    })?;
    Ok(written)
}

#[cfg(test)]
mod tests;
