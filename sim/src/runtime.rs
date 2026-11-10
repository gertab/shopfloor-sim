//! Runs an [`Engine`] in real time on tokio.
//!
//! One task owns the engine and `select!`s over incoming requests and a fixed-rate tick. Events go
//! out in batches on a broadcast channel (sinks, the UI bridge), and the latest [`WorldState`] is
//! published on a watch channel.

use crate::command::{CommandEnvelope, Control};
use crate::engine::{Engine, WorldState};
use crate::event::SimEvent;
use crate::scenario::Scenario;
use crate::sinks::{SinkMonitor, SinkThread, build_sink, spawn_sink};
use std::collections::BTreeMap;
use anyhow::anyhow;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

/// A group of events produced by one command or one tick.
pub type EventBatch = Arc<[SimEvent]>;

const EVENT_CHANNEL_CAPACITY: usize = 1024;
const REQUEST_CHANNEL_CAPACITY: usize = 64;

enum Request {
    Command(CommandEnvelope, oneshot::Sender<anyhow::Result<()>>),
    Control(Control, oneshot::Sender<anyhow::Result<()>>),
    Shutdown,
}

pub struct SimRuntime {
    scenario: Arc<Scenario>,
    requests: mpsc::Sender<Request>,
    events: broadcast::Sender<EventBatch>,
    state: watch::Receiver<WorldState>,
    engine_task: JoinHandle<()>,
    sinks: Vec<SinkThread>,
    monitor: SinkMonitor,
}

impl SimRuntime {
    /// Opens the scenario's sinks and starts the engine. Must be called inside a tokio runtime.
    pub fn spawn(scenario: Arc<Scenario>) -> anyhow::Result<Self> {
        let engine = Engine::new(Arc::clone(&scenario));
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (state_tx, state) = watch::channel(engine.state());
        let (requests, request_rx) = mpsc::channel(REQUEST_CHANNEL_CAPACITY);

        // Sinks subscribe before the engine starts so they see the opening `sim started` event.
        let monitor = SinkMonitor::default();
        let mut sinks = Vec::new();
        for def in &scenario.output.sinks {
            let sink = build_sink(def, &monitor)?;
            sinks.push(spawn_sink(def.name.clone(), sink, events.subscribe()));
        }

        let engine_task = tokio::spawn(run_engine(engine, request_rx, events.clone(), state_tx));
        Ok(Self {
            scenario,
            requests,
            events,
            state,
            engine_task,
            sinks,
            monitor,
        })
    }

    /// Live status of sinks that report one (TCP: address and connected clients), by sink name.
    pub fn sink_status(&self) -> BTreeMap<String, String> {
        self.monitor.snapshot()
    }

    pub fn scenario(&self) -> &Arc<Scenario> {
        &self.scenario
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventBatch> {
        self.events.subscribe()
    }

    pub fn watch_state(&self) -> watch::Receiver<WorldState> {
        self.state.clone()
    }

    pub fn state(&self) -> WorldState {
        self.state.borrow().clone()
    }

    /// Executes a command; returns the engine's validation error, if any.
    pub async fn send(&self, envelope: CommandEnvelope) -> anyhow::Result<()> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(Request::Command(envelope, reply))
            .await
            .map_err(|_| anyhow!("Simulation is not running"))?;
        response
            .await
            .map_err(|_| anyhow!("Simulation stopped before answering"))?
    }

    /// Pause / resume / reset, or play / stop a scene; returns the engine's error, if any.
    pub async fn control(&self, control: Control) -> anyhow::Result<()> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(Request::Control(control, reply))
            .await
            .map_err(|_| anyhow!("Simulation is not running"))?;
        response
            .await
            .map_err(|_| anyhow!("Simulation stopped before answering"))?
    }

    /// Stops the engine, then lets every sink drain and close.
    pub async fn shutdown(self) {
        let Self {
            requests,
            events,
            engine_task,
            sinks,
            ..
        } = self;
        let _ = requests.send(Request::Shutdown).await;
        if tokio::time::timeout(Duration::from_secs(2), engine_task)
            .await
            .is_err()
        {
            tracing::warn!("engine task did not stop within 2 s");
        }
        // Dropping the last sender closes the channel; sinks finish what's queued and exit.
        drop(events);
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

async fn run_engine(
    mut engine: Engine,
    mut requests: mpsc::Receiver<Request>,
    events: broadcast::Sender<EventBatch>,
    state: watch::Sender<WorldState>,
) {
    let tick = Duration::from_millis(engine.scenario().sim.tick_ms);
    let mut interval = tokio::time::interval(tick);
    // After a stall, carry on from now instead of bursting through the missed ticks.
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval.tick().await; // the first tick completes immediately

    publish(&events, engine.start(now_ms()));
    loop {
        tokio::select! {
            request = requests.recv() => match request {
                Some(Request::Command(envelope, reply)) => {
                    let result = engine
                        .apply(envelope, now_ms())
                        .map(|batch| publish(&events, batch));
                    let _ = reply.send(result);
                }
                Some(Request::Control(control, reply)) => {
                    let result = engine
                        .control(control, now_ms())
                        .map(|batch| publish(&events, batch));
                    let _ = reply.send(result);
                }
                Some(Request::Shutdown) | None => break,
            },
            _ = interval.tick() => publish(&events, engine.step(now_ms())),
        }
        let latest = engine.state();
        state.send_if_modified(|current| {
            if *current == latest {
                false
            } else {
                *current = latest;
                true
            }
        });
    }
}

fn publish(events: &broadcast::Sender<EventBatch>, batch: Vec<SimEvent>) {
    if !batch.is_empty() {
        // An error only means nobody is subscribed right now.
        let _ = events.send(batch.into());
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Origin, SimCommand, Target};
    use crate::scenario::parse_scenario;
    use std::path::Path;

    fn scenario_recording_to(path: &Path) -> Arc<Scenario> {
        let text = format!(
            r#"
            name = "Runtime test"

            [floor]
            width_m = 10.0
            height_m = 5.0

            [machines.m1]
            position = [8.0, 4.0]
            size = [1.0, 1.0]        # dock (8.0, 3.0)

            [mobile_manipulators.mm1]
            start = [2.0, 3.0]
            max_speed_mps = 2.0

            [mobile_manipulators.mm1.sensors.vibration]
            default = 0.1
            range = [0.0, 1.0]
            period_ms = 100

            [output.sinks.rec]
            type = "jsonl"
            path = '{}'
            append = false
            "#,
            path.display()
        );
        Arc::new(parse_scenario(&text, None).unwrap())
    }

    fn api(command: SimCommand) -> CommandEnvelope {
        CommandEnvelope::new(Origin::Api, command)
    }

    #[tokio::test(start_paused = true)]
    async fn drives_engine_in_real_time_and_records_to_jsonl() {
        let path = std::env::temp_dir().join(format!(
            "shopfloor-runtime-test-{}.jsonl",
            std::process::id()
        ));
        let runtime = SimRuntime::spawn(scenario_recording_to(&path)).unwrap();
        let mut events = runtime.subscribe();

        runtime
            .send(api(SimCommand::MoveTo {
                mm: "mm1".into(),
                target: Target::Machine("m1".into()),
            }))
            .await
            .unwrap();
        let error = runtime
            .send(api(SimCommand::Stop { mm: "mm9".into() }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Unknown mobile manipulator"));

        // 6 m at 2 m/s takes 3 s of sim time.
        tokio::time::sleep(Duration::from_secs(4)).await;
        let mut kinds = Vec::new();
        while let Ok(batch) = events.try_recv() {
            kinds.extend(batch.iter().map(SimEvent::kind));
        }
        assert!(kinds.contains(&"position"));
        assert!(kinds.contains(&"sensor"));
        assert_eq!(kinds.iter().filter(|k| **k == "arrived").count(), 1);
        let state = runtime.state();
        assert_eq!(state.mobile_manipulators[0].at.as_deref(), Some("m1"));
        assert!(state.sim_ms >= 3000, "sim time follows the clock: {}", state.sim_ms);

        runtime.control(Control::Pause).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let paused_at = runtime.state().sim_ms;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(runtime.state().paused);
        assert_eq!(runtime.state().sim_ms, paused_at);

        runtime.shutdown().await;

        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let recorded: Vec<SimEvent> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(recorded.first().unwrap().kind(), "sim");
        assert_eq!(recorded.last().unwrap().kind(), "sim"); // paused
        let seqs: Vec<u64> = recorded.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, (1..=recorded.len() as u64).collect::<Vec<_>>());
        assert!(recorded.iter().any(|e| e.kind() == "arrived"));
    }

    #[tokio::test]
    async fn spawn_fails_when_a_sink_cannot_open() {
        let scenario = scenario_recording_to(Path::new("/dev/null/not-a-dir/x.jsonl"));
        let error = SimRuntime::spawn(scenario).err().unwrap();
        assert!(format!("{error:#}").contains("Failed to"));
    }
}
