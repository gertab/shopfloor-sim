//! The scenario's sinks, shared by the runtimes that write to them.

use super::{EVENT_CHANNEL_CAPACITY, MessageBatch};
use crate::scenario::Scenario;
use crate::sinks::{SinkMonitor, SinkThread, build_sink, spawn_sink};
use std::collections::BTreeMap;
use tokio::sync::broadcast;

/// The scenario's sinks, each on its own thread, all fed through one channel.
pub struct Outputs {
    writer: OutputWriter,
    sinks: Vec<SinkThread>,
}

/// Where a runtime writes its output: the sinks of an [`Outputs`].
#[derive(Clone)]
pub struct OutputWriter {
    sender: broadcast::Sender<MessageBatch>,
    monitor: SinkMonitor,
}

impl Outputs {
    /// Opens the scenario's sinks. Must be called inside a tokio runtime. If one can't open, those
    /// already open are closed first, so what they hold (e.g. a port) is free when this returns.
    pub fn open(scenario: &Scenario) -> anyhow::Result<Self> {
        let monitor = SinkMonitor::default();
        let mut opened = Vec::new();
        for def in &scenario.output.sinks {
            match build_sink(def, &monitor) {
                Ok(sink) => opened.push((def.name.clone(), sink)),
                Err(error) => {
                    for (_, mut sink) in opened {
                        let _ = sink.close();
                    }
                    return Err(error);
                }
            }
        }
        let (sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let sinks = opened
            .into_iter()
            .map(|(name, sink)| spawn_sink(name, sink, sender.subscribe()))
            .collect();
        Ok(Self {
            writer: OutputWriter { sender, monitor },
            sinks,
        })
    }

    pub fn writer(&self) -> OutputWriter {
        self.writer.clone()
    }

    /// Live status of sinks that report one (TCP: address and connected clients), by sink name.
    pub fn status(&self) -> BTreeMap<String, String> {
        self.writer.status()
    }

    /// Lets every sink drain and close. Call it once the runtimes writing here have shut down:
    /// sinks run until every writer is gone.
    pub async fn close(self) {
        let Self { writer, sinks } = self;
        drop(writer);
        let _ = tokio::task::spawn_blocking(move || {
            for sink in sinks {
                if sink.join.join().is_err() {
                    tracing::error!("sink '{}' panicked", sink.name);
                }
            }
        })
        .await;
    }
}

impl OutputWriter {
    pub fn status(&self) -> BTreeMap<String, String> {
        self.monitor.snapshot()
    }

    /// Whether any sink is listening, i.e. whether a batch is worth preparing.
    pub(super) fn wanted(&self) -> bool {
        self.sender.receiver_count() > 0
    }

    pub(super) fn write(&self, batch: MessageBatch) {
        if !batch.is_empty() {
            // An error only means no sink is listening.
            let _ = self.sender.send(batch);
        }
    }
}
