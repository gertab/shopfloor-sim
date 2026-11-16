//! Liminal Desk traces: `{"version": 1, "steps": [...]}`, where each step sends a JSON payload or
//! waits.
//!
//! The rules are Desk's (its README, "Trace validation and files"):
//! - exactly `version: 1` and a non-empty `steps` array;
//! - a send step is exactly `{"type": "send", "payload": ...}`, with any JSON payload, `null` included;
//! - a delay step is exactly `{"type": "delay", "duration_ms": N}`, with N a non-negative integer.
//!
//! Like Desk, payloads are kept as written: key order, number literals, escapes and duplicate keys.
//! Only the whitespace between tokens is dropped, so each payload fits on one line.

use anyhow::{Context, anyhow, bail};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::path::Path;

/// A parsed trace.
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    /// The file name, for messages (e.g. `machine_events.json`).
    pub name: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Send this payload: its JSON as written, without the whitespace between tokens.
    Send(String),
    /// Wait this many milliseconds.
    Delay(u64),
}

impl Trace {
    /// The time its steps take: the sum of its delays.
    pub fn duration_ms(&self) -> u64 {
        self.steps
            .iter()
            .map(|step| match step {
                Step::Delay(ms) => *ms,
                Step::Send(_) => 0,
            })
            .fold(0, u64::saturating_add)
    }

    pub fn sends(&self) -> usize {
        self.steps.iter().filter(|step| matches!(step, Step::Send(_))).count()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceFile<'a> {
    #[serde(borrow)]
    version: &'a RawValue,
    #[serde(borrow)]
    steps: Vec<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepSpec<'a> {
    #[serde(rename = "type")]
    kind: String,
    /// Present (even as `null`) or missing.
    #[serde(default, borrow, deserialize_with = "present")]
    payload: Option<&'a RawValue>,
    #[serde(default)]
    duration_ms: Option<u64>,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(deserializer).map(Some)
}

pub fn load_trace(path: &Path) -> anyhow::Result<Trace> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read trace {}", path.display()))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    parse_trace(&name, &text)
}

/// Parses and validates a trace. Errors name the step and its line.
pub fn parse_trace(name: &str, text: &str) -> anyhow::Result<Trace> {
    let file: TraceFile = serde_json::from_str(text)
        .map_err(|error| anyhow!("{name}: not a valid trace: {error}"))?;
    if file.version.get() != "1" {
        bail!("{name}: unsupported trace version {}; expected the integer 1", file.version.get());
    }
    if file.steps.is_empty() {
        bail!("{name}: steps must be a non-empty array");
    }
    let mut steps = Vec::with_capacity(file.steps.len());
    for (index, raw) in file.steps.iter().enumerate() {
        // Only worked out for an error: counting the lines up to every step would take time
        // proportional to the square of the trace's length.
        let at = || format!("{name}: step {} (line {})", index + 1, line_of(text, raw.get()));
        let spec: StepSpec = serde_json::from_str(raw.get()).map_err(|error| {
            // serde_json counts lines from the start of the step, so only its message is useful.
            let message = error.to_string();
            let message = message.split(" at line ").next().unwrap_or(&message).to_string();
            anyhow!("{}: {message}", at())
        })?;
        let step = match (spec.kind.as_str(), spec.payload, spec.duration_ms) {
            ("send", Some(payload), None) => Step::Send(compact_json(payload.get())),
            ("send", None, _) => bail!("{}: a send step needs a `payload`", at()),
            ("send", Some(_), Some(_)) => bail!("{}: a send step takes no `duration_ms`", at()),
            ("delay", None, Some(ms)) => Step::Delay(ms),
            ("delay", _, None) => bail!("{}: a delay step needs a `duration_ms`", at()),
            ("delay", Some(_), Some(_)) => bail!("{}: a delay step takes no `payload`", at()),
            (other, ..) => bail!("{}: type must be \"send\" or \"delay\" (got \"{other}\")", at()),
        };
        steps.push(step);
    }
    Ok(Trace {
        name: name.to_string(),
        steps,
    })
}

/// The 1-based line of `slice`, which must point into `text`.
fn line_of(text: &str, slice: &str) -> usize {
    let offset = (slice.as_ptr() as usize).saturating_sub(text.as_ptr() as usize);
    text[..offset.min(text.len())].matches('\n').count() + 1
}

/// JSON without the whitespace between tokens. Everything else is kept: key order, number
/// literals, escapes and duplicate keys.
pub fn compact_json(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in json.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if !matches!(c, ' ' | '\t' | '\n' | '\r') {
            out.push(c);
        }
    }
    out
}

/// A send step placed on the merged timeline of several traces.
#[derive(Debug, Clone, PartialEq)]
pub struct Timed {
    /// Milliseconds from the start: the sum of the trace's delays before it.
    pub at_ms: u64,
    /// Index of its trace.
    pub trace: usize,
    /// 1-based step number in its trace.
    pub step: usize,
    pub json: String,
}

/// Every send step of `traces` in time order, as if the traces were played side by side. Ties go
/// by trace order, then step order.
pub fn merge(traces: &[Trace]) -> Vec<Timed> {
    let mut timed = Vec::new();
    for (trace_index, trace) in traces.iter().enumerate() {
        let mut at_ms = 0u64;
        for (index, step) in trace.steps.iter().enumerate() {
            match step {
                Step::Delay(ms) => at_ms = at_ms.saturating_add(*ms),
                Step::Send(json) => timed.push(Timed {
                    at_ms,
                    trace: trace_index,
                    step: index + 1,
                    json: json.clone(),
                }),
            }
        }
    }
    timed.sort_by_key(|item| (item.at_ms, item.trace, item.step));
    timed
}

/// What a payload says, read leniently: missing keys, other types and other keys are ignored.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Payload {
    pub is_object: bool,
    pub device_id: Option<String>,
    pub event: Option<String>,
    /// The payload's `type`, e.g. `"vibration"`.
    pub sensor_type: Option<String>,
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub object: Option<String>,
    pub target: Option<String>,
    pub source: Option<String>,
    pub destination: Option<String>,
    pub timestamp_ms: Option<u64>,
}

impl Payload {
    pub fn read(json: &str) -> Self {
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(json) else {
            return Self::default();
        };
        let text = |key: &str| map.get(key).and_then(|value| value.as_str()).map(str::to_string);
        Self {
            is_object: true,
            device_id: text("device_id"),
            event: text("event"),
            sensor_type: text("type"),
            value: map.get("value").and_then(|value| value.as_f64()),
            unit: text("unit"),
            object: text("object"),
            target: text("target"),
            source: text("source"),
            destination: text("destination"),
            timestamp_ms: map.get("timestamp_ms").and_then(|value| value.as_u64()),
        }
    }

    /// The simulator event kind this payload counts as (for MQTT `kinds`, the app's log and the
    /// envelope): `move` → `move_command`, any other `event` (`pick`, `place`, `dock`, `undock`, …)
    /// → `action`, and a `type` without an `event` → `sensor`.
    pub fn kind(&self) -> Option<&'static str> {
        match self.event.as_deref() {
            Some("move") => Some("move_command"),
            Some(_) => Some("action"),
            None if self.sensor_type.is_some() => Some("sensor"),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error_of(text: &str) -> String {
        format!("{:#}", parse_trace("t.json", text).unwrap_err())
    }

    #[test]
    fn parses_send_and_delay_steps_keeping_payloads_as_written() {
        let text = r#"{
          "version": 1,
          "steps": [
            {"type": "send", "payload": {"b": 1.0, "a": [1, 2e3], "s": "x \"y\"  z", "b": null}},
            {"type": "delay", "duration_ms": 100},
            {"payload": null, "type": "send"}
          ]
        }"#;
        let trace = parse_trace("t.json", text).unwrap();
        assert_eq!(
            trace.steps,
            vec![
                Step::Send(r#"{"b":1.0,"a":[1,2e3],"s":"x \"y\"  z","b":null}"#.into()),
                Step::Delay(100),
                Step::Send("null".into()),
            ]
        );
        assert_eq!((trace.duration_ms(), trace.sends()), (100, 2));
    }

    #[test]
    fn rejects_what_desk_rejects() {
        let cases = [
            ("not json", "not a valid trace"),
            (r#"{"version": 2, "steps": [{"type": "delay", "duration_ms": 1}]}"#, "unsupported trace version 2"),
            (r#"{"version": 1.0, "steps": [{"type": "delay", "duration_ms": 1}]}"#, "unsupported trace version 1.0"),
            (r#"{"version": 1, "steps": []}"#, "steps must be a non-empty array"),
            (r#"{"version": 1}"#, "missing field `steps`"),
            (r#"{"version": 1, "steps": [], "x": 1}"#, "unknown field `x`"),
            (r#"{"version": 1, "steps": [{"type": "wait", "duration_ms": 1}]}"#, "type must be \"send\" or \"delay\""),
            (r#"{"version": 1, "steps": [{"type": "send"}]}"#, "a send step needs a `payload`"),
            (r#"{"version": 1, "steps": [{"type": "send", "payload": 1, "duration_ms": 1}]}"#, "takes no `duration_ms`"),
            (r#"{"version": 1, "steps": [{"type": "delay", "duration_ms": -1}]}"#, "step 1 (line 1)"),
            (r#"{"version": 1, "steps": [{"type": "delay", "duration_ms": 1.5}]}"#, "invalid type: floating point"),
            (r#"{"version": 1, "steps": [{"type": "delay", "duration_ms": true}]}"#, "invalid type: boolean"),
            (r#"{"version": 1, "steps": [{"type": "delay"}]}"#, "needs a `duration_ms`"),
            (r#"{"version": 1, "steps": [{"type": "delay", "duration_ms": 1, "x": 0}]}"#, "unknown field `x`"),
            (r#"{"version": 1, "steps": [{"type": "delay", "type": "delay", "duration_ms": 1}]}"#, "duplicate field `type`"),
            (r#"{"version": 1, "steps": [3]}"#, "step 1"),
        ];
        for (text, expected) in cases {
            let error = error_of(text);
            assert!(error.contains(expected), "{text}\n  -> {error}");
        }
    }

    #[test]
    fn errors_give_the_line_of_the_step() {
        let text = "{\n  \"version\": 1,\n  \"steps\": [\n    {\"type\": \"delay\", \"duration_ms\": 5},\n    {\"type\": \"delay\"}\n  ]\n}";
        assert!(error_of(text).contains("t.json: step 2 (line 5)"), "{}", error_of(text));
    }

    #[test]
    fn compacting_only_drops_whitespace_between_tokens() {
        assert_eq!(compact_json("{ \"a\" : [ 1 , 2 ],\n\t\"b\\\" c\": \" \\\\ \" }"), r#"{"a":[1,2],"b\" c":" \\ "}"#);
    }

    #[test]
    fn merging_interleaves_traces_by_their_delays() {
        let a = Trace {
            name: "a".into(),
            steps: vec![Step::Send("1".into()), Step::Delay(100), Step::Send("2".into())],
        };
        let b = Trace {
            name: "b".into(),
            steps: vec![Step::Delay(50), Step::Send("3".into()), Step::Delay(50), Step::Send("4".into())],
        };
        let merged = merge(&[a, b]);
        let order: Vec<(u64, &str, usize)> = merged
            .iter()
            .map(|item| (item.at_ms, item.json.as_str(), item.step))
            .collect();
        // At 100 ms, trace a comes before trace b.
        assert_eq!(order, vec![(0, "1", 1), (50, "3", 2), (100, "2", 3), (100, "4", 4)]);
    }

    #[test]
    fn payloads_count_as_simulator_event_kinds() {
        let kind = |json: &str| Payload::read(json).kind();
        assert_eq!(kind(r#"{"type":"vibration","device_id":"mm-2","value":1.3}"#), Some("sensor"));
        assert_eq!(kind(r#"{"device_id":"mm-1","event":"pick","object":"dies"}"#), Some("action"));
        assert_eq!(kind(r#"{"device_id":"mm-1","event":"move","source":"a","destination":"b"}"#), Some("move_command"));
        assert_eq!(kind(r#"{"event":"dock","target":"a"}"#), Some("action"));
        assert_eq!(kind(r#"{"event":"undock","target":"a"}"#), Some("action"));
        assert_eq!(kind(r#"{"device_id":"mm-1"}"#), None);
        assert_eq!(kind("[1, 2]"), None);

        let payload = Payload::read(r#"{"type":"lidar","device_id":"mm-1","value":0.25,"unit":"m","target":"stocker","timestamp_ms":1794236400000}"#);
        assert!(payload.is_object);
        assert_eq!(payload.device_id.as_deref(), Some("mm-1"));
        assert_eq!((payload.value, payload.timestamp_ms), (Some(0.25), Some(1_794_236_400_000)));
        assert!(!Payload::read("null").is_object);
    }
}
