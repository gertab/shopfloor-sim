//! Forwards the simulation to the webview:
//! - `shopfloor://state`: the latest [`WorldState`], at most ~30 times a second and only when it changed;
//! - `shopfloor://events`: output events, batched every 100 ms.

use shopfloor_sim::{SimEvent, SimRuntime};
use std::time::Duration;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::MissedTickBehavior;

pub(crate) const STATE_EVENT: &str = "shopfloor://state";
pub(crate) const EVENTS_EVENT: &str = "shopfloor://events";

const STATE_INTERVAL: Duration = Duration::from_millis(33);
const EVENTS_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) struct Bridge {
    tasks: Vec<JoinHandle<()>>,
}

impl Bridge {
    pub(crate) fn start(app: AppHandle, runtime: &SimRuntime) -> Self {
        let mut state = runtime.watch_state();
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

        let mut events = runtime.subscribe();
        let events_task = tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(EVENTS_INTERVAL);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut pending: Vec<SimEvent> = Vec::new();
            loop {
                tokio::select! {
                    received = events.recv() => match received {
                        Ok(batch) => pending.extend(batch.iter().cloned()),
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
