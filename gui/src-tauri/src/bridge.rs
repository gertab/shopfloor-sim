//! Forwards the simulation (or a trace replay) to the webview:
//! - `shopfloor://state`: the latest [`WorldState`], at most ~30 times a second and only when it changed;
//! - `shopfloor://events`: [`LogEntry`]s, batched every 100 ms.

use serde::Serialize;
use serde_json::value::RawValue;
use shopfloor_core::WorldState;
use shopfloor_core::sinks::{Message, ToMessage};
use std::sync::Arc;
use std::time::Duration;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

pub(crate) const STATE_EVENT: &str = "shopfloor://state";
pub(crate) const EVENTS_EVENT: &str = "shopfloor://events";

const STATE_INTERVAL: Duration = Duration::from_millis(33);
const EVENTS_INTERVAL: Duration = Duration::from_millis(100);

/// One row of the app's log: what the sinks wrote (`data`), plus what the log sorts and filters on.
#[derive(Clone, Serialize)]
struct LogEntry {
    seq: u64,
    sim_ms: u64,
    kind: Option<&'static str>,
    device_id: Option<String>,
    data: Box<RawValue>,
}

impl From<Message> for LogEntry {
    fn from(message: Message) -> Self {
        Self {
            seq: message.seq,
            sim_ms: message.sim_ms,
            kind: message.kind,
            device_id: message.device_id,
            data: RawValue::from_string(message.json).expect("sinks write JSON"),
        }
    }
}

pub(crate) struct Bridge {
    tasks: Vec<JoinHandle<()>>,
}

impl Bridge {
    /// Forwards `state` and the batches on `events` (the simulator's events or replayed messages).
    pub(crate) fn start<T: ToMessage + Send + Sync + 'static>(
        app: AppHandle,
        mut state: watch::Receiver<WorldState>,
        mut events: broadcast::Receiver<Arc<[T]>>,
    ) -> Self {
        let state_app = app.clone();
        let state_task = tauri::async_runtime::spawn(async move {
            let _ = state_app.emit(STATE_EVENT, state.borrow_and_update().clone());
            let mut interval = tokio::time::interval(STATE_INTERVAL);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                match state.has_changed() {
                    Ok(true) => {
                        let snapshot = state.borrow_and_update().clone();
                        let _ = state_app.emit(STATE_EVENT, snapshot);
                    }
                    Ok(false) => {}
                    Err(_) => break, // simulation stopped
                }
            }
        });

        let events_task = tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(EVENTS_INTERVAL);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut pending: Vec<LogEntry> = Vec::new();
            loop {
                tokio::select! {
                    received = events.recv() => match received {
                        Ok(batch) => pending.extend(batch.iter().map(|item| LogEntry::from(item.to_message()))),
                        Err(RecvError::Lagged(skipped)) => {
                            tracing::warn!("UI event bridge fell behind; skipped {skipped} batches");
                        }
                        Err(RecvError::Closed) => break,
                    },
                    _ = interval.tick() => {
                        if !pending.is_empty() {
                            let _ = app.emit(EVENTS_EVENT, std::mem::take(&mut pending));
                        }
                    }
                }
            }
            if !pending.is_empty() {
                let _ = app.emit(EVENTS_EVENT, pending);
            }
        });

        Self {
            tasks: vec![state_task, events_task],
        }
    }

    pub(crate) fn stop(self) {
        for task in self.tasks {
            task.abort();
        }
    }
}
