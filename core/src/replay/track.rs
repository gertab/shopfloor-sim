//! Where each MM is over time during a replay, built from its trace events.

use super::Item;
use super::checks::Findings;
use crate::engine::Place;
use crate::geometry::Vec2;
use crate::scenario::{MachineDef, Scenario};

/// Where an MM waits beside a machine: this far out from its dock, away from the machine, leaning to
/// one side (alternating by MM) so that two MMs waiting at the same machine stand apart.
const APPROACH_M: f64 = 1.0;
const APPROACH_LEAN_RAD: f64 = std::f64::consts::PI / 6.0;
/// How long docking and undocking take at most: the MM glides between its approach point and the
/// dock.
const GLIDE_MS: u64 = 1000;

/// Where an MM is over time, built from its trace events.
#[derive(Debug, Clone, Default)]
pub(super) struct Track {
    /// Positions at given times; between two keys the MM moves in a straight line.
    pub(super) keys: Vec<(u64, Vec2)>,
    /// From when the MM is docked at a machine (or not).
    docked: Vec<(u64, Option<String>)>,
    pub(super) moves: Vec<MoveSpan>,
}

#[derive(Debug, Clone)]
pub(super) struct MoveSpan {
    pub(super) start_ms: u64,
    pub(super) end_ms: u64,
    pub(super) command_id: u64,
    pub(super) target: Place,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Sample {
    pub(super) position: Vec2,
    pub(super) heading: Option<f64>,
    pub(super) speed_mps: f64,
    pub(super) moving: bool,
}

impl Track {
    pub(super) fn sample(&self, t: u64) -> Sample {
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

    pub(super) fn docked_at(&self, t: u64) -> Option<String> {
        self.docked
            .iter()
            .rev()
            .find(|(at, _)| *at <= t)
            .and_then(|(_, machine)| machine.clone())
    }

    pub(super) fn docked_now(&self) -> Option<String> {
        self.docked.last().and_then(|(_, machine)| machine.clone())
    }

    /// Stops whatever motion is planned beyond `t` and returns where the MM is then.
    pub(super) fn cut(&mut self, t: u64) -> Vec2 {
        let position = self.sample(t).position;
        self.keys.retain(|(at, _)| *at <= t);
        self.go(t, position);
        for span in &mut self.moves {
            span.end_ms = span.end_ms.min(t);
        }
        position
    }

    /// The MM is at `position` at time `t` (a key, unless it already is one).
    pub(super) fn go(&mut self, t: u64, position: Vec2) {
        if self.keys.last() != Some(&(t, position)) {
            self.keys.push((t, position));
        }
    }
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
pub(super) fn build_track(
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
