//! `[output]`: where the event stream goes besides the app, as written and as resolved.

use super::SimSettings;
use super::resolve::resolve_path;
use crate::event::EVENT_KINDS;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSpec {
    /// How often a moving MM reports its position (sim time).
    #[serde(default = "default_position_interval_ms")]
    pub position_interval_ms: u64,
    #[serde(default)]
    pub sinks: BTreeMap<String, SinkKind>,
}

impl Default for OutputSpec {
    fn default() -> Self {
        Self {
            position_interval_ms: default_position_interval_ms(),
            sinks: BTreeMap::new(),
        }
    }
}

/// Where the event stream goes, besides the UI.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SinkKind {
    /// One JSON line per event on stdout.
    Stdout,
    /// One JSON line per event in a file. Relative paths are resolved against the scenario file.
    Jsonl {
        path: PathBuf,
        #[serde(default = "default_true")]
        append: bool,
    },
    /// Listen on `bind` (e.g. `127.0.0.1:9000`) and stream every event to each connected client
    /// as a length-prefixed JSON frame.
    Tcp { bind: String },
    /// Publish each event, as JSON, to every topic it matches on the MQTT broker at `broker`
    /// (`host:port` or `mqtt://host:port`; the port defaults to 1883).
    Mqtt {
        broker: String,
        #[serde(default)]
        topics: Vec<MqttTopic>,
        /// 0, 1 or 2.
        #[serde(default)]
        qos: u8,
        /// Defaults to `shopfloor-<sink name>-<process id>`.
        #[serde(default)]
        client_id: Option<String>,
    },
}

impl SinkKind {
    /// Streams to consumers as events happen (TCP, MQTT), so it's only useful in real time.
    pub fn is_live(&self) -> bool {
        matches!(self, SinkKind::Tcp { .. } | SinkKind::Mqtt { .. })
    }
}

/// An MQTT topic and the events it carries. An empty filter matches everything.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MqttTopic {
    pub topic: String,
    /// Event kinds, e.g. `["sensor", "sensor_set"]`.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Values of the event's `device_id` field (machine, MM or free-standing sensor ids). Events
    /// without one (`sim`, `scene`) only match topics with no entity filter.
    #[serde(default)]
    pub entities: Vec<String>,
}

impl MqttTopic {
    /// Whether a message of this `kind` about `device_id` goes to this topic. A message whose kind
    /// or device can't be told only matches topics without that filter.
    pub fn matches(&self, kind: Option<&str>, device_id: Option<&str>) -> bool {
        let kind_ok =
            self.kinds.is_empty() || kind.is_some_and(|kind| self.kinds.iter().any(|known| known == kind));
        let entity_ok = self.entities.is_empty()
            || device_id.is_some_and(|device_id| self.entities.iter().any(|id| id == device_id));
        kind_ok && entity_ok
    }
}

/// Host and port of an MQTT broker address: `host:port`, `mqtt://host:port` or just `host`
/// (port 1883).
pub(crate) fn parse_broker(broker: &str) -> Result<(String, u16), String> {
    let address = broker.trim();
    let address = address.strip_prefix("mqtt://").unwrap_or(address);
    let (host, port) = match address.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or_else(|| format!("'{broker}' has an invalid port"))?;
            (host, port)
        }
        None => (address, 1883),
    };
    if host.is_empty() || host.contains('/') {
        return Err(format!("'{broker}' is not a broker address (host:port)"));
    }
    Ok((host.to_string(), port))
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputSettings {
    pub position_interval_ms: u64,
    pub sinks: Vec<SinkDef>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SinkDef {
    pub name: String,
    #[serde(flatten)]
    pub kind: SinkKind,
}

/// `[output]`: checks it, and resolves sink paths against the scenario file's folder.
pub(super) fn resolve_output(
    output: &OutputSpec,
    sim: &SimSettings,
    base_dir: Option<&Path>,
    entity_ids: &HashSet<&str>,
    errors: &mut Vec<String>,
) -> OutputSettings {
    if output.position_interval_ms < sim.tick_ms {
        errors.push(format!(
            "output.position_interval_ms ({}) must be >= sim.tick_ms ({})",
            output.position_interval_ms, sim.tick_ms
        ));
    }
    let mut sinks = Vec::new();
    for (name, kind) in &output.sinks {
        let kind = match kind {
            SinkKind::Stdout => SinkKind::Stdout,
            SinkKind::Tcp { bind } => {
                if bind.trim().is_empty() {
                    errors.push(format!("output.sinks.{name}.bind must not be empty"));
                }
                SinkKind::Tcp { bind: bind.clone() }
            }
            SinkKind::Jsonl { path, append } => {
                if path.as_os_str().is_empty() {
                    errors.push(format!("output.sinks.{name}.path must not be empty"));
                }
                SinkKind::Jsonl {
                    path: resolve_path(base_dir, path),
                    append: *append,
                }
            }
            SinkKind::Mqtt {
                broker,
                topics,
                qos,
                client_id,
            } => {
                check_mqtt_sink(name, broker, topics, *qos, entity_ids, errors);
                SinkKind::Mqtt {
                    broker: broker.clone(),
                    topics: topics.clone(),
                    qos: *qos,
                    client_id: client_id.clone(),
                }
            }
        };
        sinks.push(SinkDef {
            name: name.clone(),
            kind,
        });
    }
    OutputSettings {
        position_interval_ms: output.position_interval_ms,
        sinks,
    }
}

fn check_mqtt_sink(
    name: &str,
    broker: &str,
    topics: &[MqttTopic],
    qos: u8,
    entity_ids: &HashSet<&str>,
    errors: &mut Vec<String>,
) {
    let at = format!("output.sinks.{name}");
    if let Err(error) = parse_broker(broker) {
        errors.push(format!("{at}.broker: {error}"));
    }
    if qos > 2 {
        errors.push(format!("{at}.qos must be 0, 1 or 2 (got {qos})"));
    }
    if topics.is_empty() {
        errors.push(format!(
            "{at} must list at least one topic ([[{at}.topics]] with a `topic`)"
        ));
    }
    let mut seen = HashSet::new();
    for topic in topics {
        let name = &topic.topic;
        if name.is_empty() {
            errors.push(format!("{at}.topics: a topic name must not be empty"));
        } else if name.contains(['+', '#', '\0']) {
            errors.push(format!(
                "{at}.topics: '{name}' contains a wildcard ('+' or '#'), which only \
                 subscribers may use"
            ));
        }
        if !seen.insert(name) {
            errors.push(format!("{at}.topics: '{name}' is listed more than once"));
        }
        for kind in &topic.kinds {
            if !EVENT_KINDS.contains(&kind.as_str()) {
                errors.push(format!(
                    "{at}.topics: '{name}' names unknown event kind '{kind}' (known: {})",
                    EVENT_KINDS.join(", ")
                ));
            }
        }
        for entity in &topic.entities {
            if !entity_ids.contains(entity.as_str()) {
                errors.push(format!(
                    "{at}.topics: '{name}' names unknown entity '{entity}' (machines, mobile \
                     manipulators and free-standing sensors)"
                ));
            }
        }
    }
}

fn default_position_interval_ms() -> u64 {
    100
}
fn default_true() -> bool {
    true
}
