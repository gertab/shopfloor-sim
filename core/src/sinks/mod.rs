//! Event sinks: where the event stream goes besides the UI.
//!
//! Sinks write [`Message`]s: the JSON of a simulator event, or a replayed trace payload as it was
//! written, plus the `kind` and `device_id` that MQTT topics route on.
//!
//! Each sink runs on its own OS thread, reading batches from the runtime's broadcast channel, so a
//! slow disk or terminal never stalls the simulation. A sink that falls too far behind skips
//! batches (with a warning) rather than slowing the engine down.

mod jsonl;
mod mqtt;
mod stdout;
mod tcp;

pub use jsonl::JsonlSink;
pub use mqtt::MqttSink;
pub use stdout::StdoutSink;
pub use tcp::{TcpSink, encode_frames};

use crate::event::SimEvent;
use crate::scenario::{SinkDef, SinkKind};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

/// One item of the output stream, as the sinks write it.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Position in the stream (the app's log key; not written).
    pub seq: u64,
    /// Simulation (or replay) time it belongs to.
    pub sim_ms: u64,
    /// The event kind it counts as, e.g. `"sensor"`, or `None` when that can't be told.
    pub kind: Option<&'static str>,
    pub device_id: Option<String>,
    /// One JSON object on one line: exactly what the sinks write.
    pub json: String,
}

impl Message {
    pub fn from_event(event: &SimEvent) -> Self {
        Self {
            seq: event.seq,
            sim_ms: event.sim_ms,
            kind: Some(event.kind()),
            device_id: event.device_id().map(str::to_string),
            json: to_json_line(event),
        }
    }
}

/// What a runtime broadcasts and its sink threads turn into [`Message`]s.
pub trait ToMessage {
    fn to_message(&self) -> Message;
}

impl ToMessage for SimEvent {
    fn to_message(&self) -> Message {
        Message::from_event(self)
    }
}

impl ToMessage for Message {
    fn to_message(&self) -> Message {
        self.clone()
    }
}

pub trait EventSink: Send {
    fn write_batch(&mut self, messages: &[Message]) -> std::io::Result<()>;

    fn close(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Live status lines of sinks that have one (e.g. "listening on 127.0.0.1:9000 · 1 connected"),
/// keyed by sink name. Shared between the sink threads and whoever wants to show them.
#[derive(Debug, Clone, Default)]
pub struct SinkMonitor(Arc<Mutex<BTreeMap<String, String>>>);

impl SinkMonitor {
    pub fn set(&self, sink: &str, status: String) {
        self.0.lock().expect("sink monitor lock").insert(sink.to_string(), status);
    }

    pub fn remove(&self, sink: &str) {
        self.0.lock().expect("sink monitor lock").remove(sink);
    }

    pub fn snapshot(&self) -> BTreeMap<String, String> {
        self.0.lock().expect("sink monitor lock").clone()
    }
}

pub fn build_sink(def: &SinkDef, monitor: &SinkMonitor) -> anyhow::Result<Box<dyn EventSink>> {
    Ok(match &def.kind {
        SinkKind::Stdout => Box::new(StdoutSink),
        SinkKind::Jsonl { path, append } => Box::new(JsonlSink::open(path, *append)?),
        SinkKind::Tcp { bind } => Box::new(TcpSink::bind(&def.name, bind, monitor.clone())?),
        SinkKind::Mqtt {
            broker,
            topics,
            qos,
            client_id,
        } => Box::new(MqttSink::connect(
            &def.name,
            broker,
            topics,
            *qos,
            client_id.as_deref(),
            monitor.clone(),
        )?),
    })
}

/// One JSON object per line, as written by every sink.
pub fn to_json_line(event: &SimEvent) -> String {
    serde_json::to_string(event).expect("events always serialise")
}

pub(crate) struct SinkThread {
    pub name: String,
    pub join: std::thread::JoinHandle<()>,
}

pub(crate) fn spawn_sink<T: ToMessage + Send + Sync + 'static>(
    name: String,
    mut sink: Box<dyn EventSink>,
    mut events: broadcast::Receiver<Arc<[T]>>,
) -> SinkThread {
    let thread_name = name.clone();
    let join = std::thread::Builder::new()
        .name(format!("sink-{name}"))
        .spawn(move || {
            loop {
                match events.blocking_recv() {
                    Ok(batch) => {
                        let messages: Vec<Message> = batch.iter().map(ToMessage::to_message).collect();
                        if let Err(error) = sink.write_batch(&messages) {
                            tracing::error!("sink '{thread_name}': write failed, stopping: {error}");
                            break;
                        }
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        tracing::warn!("sink '{thread_name}' fell behind and skipped {skipped} batches");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
            if let Err(error) = sink.close() {
                tracing::error!("sink '{thread_name}': close failed: {error}");
            }
        })
        .expect("failed to spawn sink thread");
    SinkThread { name, join }
}
