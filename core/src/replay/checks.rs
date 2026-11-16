//! Checking traces against the scenario. Nothing found here stops a replay: findings are warnings.

use super::Item;
use crate::scenario::Scenario;

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
pub(super) struct Findings(Vec<Finding>);

impl Findings {
    pub(super) fn into_vec(self) -> Vec<Finding> {
        self.0
    }

    pub(super) fn add(&mut self, trace: &str, step: Option<usize>, message: String) {
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

/// Checks each payload on its own against the scenario.
pub(super) fn check_payloads(
    scenario: &Scenario,
    timeline: &[Item],
    names: &[String],
    findings: &mut Findings,
) {
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
pub(super) fn check_timestamps(timeline: &[Item], names: &[String], findings: &mut Findings) -> Option<u64> {
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
