//! Runs an [`Engine`] ([`SimRuntime`]) or a trace [`Replay`] ([`ReplayRuntime`]) in real time on
//! tokio.
//!
//! One task owns the engine (or replay) and `select!`s over incoming requests and a fixed-rate
//! tick. Its output goes out in batches on a broadcast channel (the UI bridge, the CLI), and to the
//! scenario's sinks through an [`OutputWriter`]. The latest [`WorldState`] is published on a watch
//! channel.
//!
//! The sinks belong to an [`Outputs`]. A runtime started with `spawn` opens its own; the app opens
//! one per loaded scenario and hands it to each runtime (`spawn_with`), so switching between
//! simulating and replaying keeps them open and their consumers connected.

use crate::command::{CommandEnvelope, Control};
use crate::engine::{Engine, WorldState};
use crate::event::SimEvent;
use crate::replay::Replay;
use crate::scenario::Scenario;
use crate::sinks::{Message, SinkMonitor, SinkThread, build_sink, spawn_sink};
use anyhow::anyhow;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

/// A group of events produced by one command or one tick.
pub type EventBatch = Arc<[SimEvent]>;
/// A group of messages for the sinks (or replayed messages) produced by one tick.
pub type MessageBatch = Arc<[Message]>;

const EVENT_CHANNEL_CAPACITY: usize = 1024;
const REQUEST_CHANNEL_CAPACITY: usize = 64;
/// How long a runtime's task gets to stop before it is aborted.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// Opens the scenario's sinks. Must be called inside a tokio runtime.
    pub fn open(scenario: &Scenario) -> anyhow::Result<Self> {
        let (sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let monitor = SinkMonitor::default();
        let mut sinks = Vec::new();
        for def in &scenario.output.sinks {
            let sink = build_sink(def, &monitor)?;
            sinks.push(spawn_sink(def.name.clone(), sink, sender.subscribe()));
        }
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
    fn wanted(&self) -> bool {
        self.sender.receiver_count() > 0
    }

    fn write(&self, batch: MessageBatch) {
        if !batch.is_empty() {
            // An error only means no sink is listening.
            let _ = self.sender.send(batch);
        }
    }
}

/// Waits for a runtime's task to stop, aborting it if it takes too long.
async fn stop_task(mut task: JoinHandle<()>, what: &str) {
    if tokio::time::timeout(STOP_TIMEOUT, &mut task).await.is_err() {
        tracing::warn!("{what} task did not stop within {} s", STOP_TIMEOUT.as_secs());
        task.abort();
    }
}

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
    writer: OutputWriter,
    /// Outputs this runtime opened itself (`spawn`), closed when it shuts down.
    owned_outputs: Option<Outputs>,
}

impl SimRuntime {
    /// Opens the scenario's sinks and starts the engine. Must be called inside a tokio runtime.
    pub fn spawn(scenario: Arc<Scenario>) -> anyhow::Result<Self> {
        // The sinks are open before the engine starts, so they see the opening `sim started`.
        let outputs = Outputs::open(&scenario)?;
        let mut runtime = Self::spawn_with(scenario, outputs.writer());
        runtime.owned_outputs = Some(outputs);
        Ok(runtime)
    }

    /// Starts the engine, writing to sinks that are already open (and stay open after it).
    pub fn spawn_with(scenario: Arc<Scenario>, writer: OutputWriter) -> Self {
        let engine = Engine::new(Arc::clone(&scenario));
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (state_tx, state) = watch::channel(engine.state());
        let (requests, request_rx) = mpsc::channel(REQUEST_CHANNEL_CAPACITY);
        let engine_task = tokio::spawn(run_engine(
            engine,
            request_rx,
            events.clone(),
            writer.clone(),
            state_tx,
        ));
        Self {
            scenario,
            requests,
            events,
            state,
            engine_task,
            writer,
            owned_outputs: None,
        }
    }

    /// Live status of sinks that report one (TCP: address and connected clients), by sink name.
    pub fn sink_status(&self) -> BTreeMap<String, String> {
        self.writer.status()
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

    /// Stops the engine. Sinks it opened itself then drain and close; shared ones stay open.
    pub async fn shutdown(self) {
        let Self {
            requests,
            engine_task,
            writer,
            owned_outputs,
            ..
        } = self;
        let _ = requests.send(Request::Shutdown).await;
        stop_task(engine_task, "engine").await;
        // Sinks run until every writer is gone, this one included.
        drop(writer);
        if let Some(outputs) = owned_outputs {
            outputs.close().await;
        }
    }
}

enum ReplayRequest {
    Control(Control, oneshot::Sender<anyhow::Result<()>>),
    Shutdown,
}

/// Plays a [`Replay`] in real time. Unlike [`SimRuntime`] it takes no commands: a replay is
/// read-only, and only pause, resume and reset (start again) are possible.
pub struct ReplayRuntime {
    scenario: Arc<Scenario>,
    requests: mpsc::Sender<ReplayRequest>,
    events: broadcast::Sender<MessageBatch>,
    state: watch::Receiver<WorldState>,
    task: JoinHandle<()>,
    writer: OutputWriter,
    /// Outputs this runtime opened itself (`spawn`), closed when it shuts down.
    owned_outputs: Option<Outputs>,
}

impl ReplayRuntime {
    /// Opens the scenario's sinks and starts playing. Must be called inside a tokio runtime.
    pub fn spawn(replay: Replay) -> anyhow::Result<Self> {
        let outputs = Outputs::open(replay.scenario())?;
        let mut runtime = Self::spawn_with(replay, outputs.writer());
        runtime.owned_outputs = Some(outputs);
        Ok(runtime)
    }

    /// Starts playing, writing to sinks that are already open (and stay open after it).
    pub fn spawn_with(replay: Replay, writer: OutputWriter) -> Self {
        let scenario = Arc::clone(replay.scenario());
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (state_tx, state) = watch::channel(replay.state());
        let (requests, request_rx) = mpsc::channel(REQUEST_CHANNEL_CAPACITY);
        let task = tokio::spawn(run_replay(
            replay,
            request_rx,
            events.clone(),
            writer.clone(),
            state_tx,
        ));
        Self {
            scenario,
            requests,
            events,
            state,
            task,
            writer,
            owned_outputs: None,
        }
    }

    pub fn sink_status(&self) -> BTreeMap<String, String> {
        self.writer.status()
    }

    pub fn scenario(&self) -> &Arc<Scenario> {
        &self.scenario
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MessageBatch> {
        self.events.subscribe()
    }

    pub fn watch_state(&self) -> watch::Receiver<WorldState> {
        self.state.clone()
    }

    pub fn state(&self) -> WorldState {
        self.state.borrow().clone()
    }

    /// Pause, resume or reset (start again). Scenes are refused.
    pub async fn control(&self, control: Control) -> anyhow::Result<()> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(ReplayRequest::Control(control, reply))
            .await
            .map_err(|_| anyhow!("The replay is not running"))?;
        response
            .await
            .map_err(|_| anyhow!("The replay stopped before answering"))?
    }

    /// Stops playing. Sinks it opened itself then drain and close; shared ones stay open.
    pub async fn shutdown(self) {
        let Self {
            requests,
            task,
            writer,
            owned_outputs,
            ..
        } = self;
        let _ = requests.send(ReplayRequest::Shutdown).await;
        stop_task(task, "replay").await;
        // Sinks run until every writer is gone, this one included.
        drop(writer);
        if let Some(outputs) = owned_outputs {
            outputs.close().await;
        }
    }
}

async fn run_replay(
    mut replay: Replay,
    mut requests: mpsc::Receiver<ReplayRequest>,
    events: broadcast::Sender<MessageBatch>,
    outputs: OutputWriter,
    state: watch::Sender<WorldState>,
) {
    let tick = Duration::from_millis(replay.scenario().sim.tick_ms);
    let mut interval = tokio::time::interval(tick);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval.tick().await;

    publish_messages(&events, &outputs, replay.start());
    loop {
        tokio::select! {
            request = requests.recv() => match request {
                Some(ReplayRequest::Control(control, reply)) => {
                    let result = replay
                        .control(control, now_ms())
                        .map(|batch| publish_messages(&events, &outputs, batch));
                    let _ = reply.send(result);
                }
                Some(ReplayRequest::Shutdown) | None => break,
            },
            _ = interval.tick() => publish_messages(&events, &outputs, replay.step()),
        }
        let latest = replay.state();
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

async fn run_engine(
    mut engine: Engine,
    mut requests: mpsc::Receiver<Request>,
    events: broadcast::Sender<EventBatch>,
    outputs: OutputWriter,
    state: watch::Sender<WorldState>,
) {
    let tick = Duration::from_millis(engine.scenario().sim.tick_ms);
    let mut interval = tokio::time::interval(tick);
    // After a stall, carry on from now instead of bursting through the missed ticks.
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval.tick().await; // the first tick completes immediately

    publish_events(&events, &outputs, engine.start(now_ms()));
    // A scenario can open paused: `sim started`, then `sim paused`, and the clock waits at 0.
    if engine.scenario().sim.start_paused {
        let batch = engine
            .control(Control::Pause, now_ms())
            .expect("pausing never fails");
        publish_events(&events, &outputs, batch);
        state.send_replace(engine.state());
    }
    loop {
        tokio::select! {
            request = requests.recv() => match request {
                Some(Request::Command(envelope, reply)) => {
                    let result = engine
                        .apply(envelope, now_ms())
                        .map(|batch| publish_events(&events, &outputs, batch));
                    let _ = reply.send(result);
                }
                Some(Request::Control(control, reply)) => {
                    let result = engine
                        .control(control, now_ms())
                        .map(|batch| publish_events(&events, &outputs, batch));
                    let _ = reply.send(result);
                }
                Some(Request::Shutdown) | None => break,
            },
            _ = interval.tick() => publish_events(&events, &outputs, engine.step(now_ms())),
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

/// Sends a batch of events to the runtime's subscribers (UI bridge, CLI) and, as messages, to the
/// sinks (serialised only if a sink is listening).
fn publish_events(events: &broadcast::Sender<EventBatch>, outputs: &OutputWriter, batch: Vec<SimEvent>) {
    if batch.is_empty() {
        return;
    }
    if outputs.wanted() {
        outputs.write(batch.iter().map(Message::from_event).collect());
    }
    // An error only means nobody is subscribed right now.
    let _ = events.send(batch.into());
}

/// Sends a batch of replayed messages to the runtime's subscribers and to the sinks.
fn publish_messages(events: &broadcast::Sender<MessageBatch>, outputs: &OutputWriter, batch: Vec<Message>) {
    if batch.is_empty() {
        return;
    }
    let batch: MessageBatch = batch.into();
    outputs.write(Arc::clone(&batch));
    let _ = events.send(batch);
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
        let path = std::env::temp_dir().join(format!("shopfloor-runtime-test-{}.jsonl", std::process::id()));
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
        assert!(
            state.sim_ms >= 3000,
            "sim time follows the clock: {}",
            state.sim_ms
        );

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

    #[tokio::test(start_paused = true)]
    async fn replays_a_trace_in_real_time_and_records_it_as_written() {
        use crate::replay::{Replay, ReplayOptions};
        use crate::trace::parse_trace;

        let path = std::env::temp_dir().join(format!("shopfloor-replay-test-{}.jsonl", std::process::id()));
        let trace = parse_trace(
            "t.json",
            r#"{"version": 1, "steps": [
              {"type": "send", "payload": {"device_id": "mm1", "event": "undock", "target": "m1"}},
              {"type": "delay", "duration_ms": 500},
              {"type": "send", "payload": {"type": "vibration", "device_id": "mm1", "value": 0.20}}
            ]}"#,
        )
        .unwrap();
        let scenario = scenario_recording_to(&path);
        let (replay, _) = Replay::new(scenario, vec![trace], ReplayOptions::default(), 0).unwrap();
        let runtime = ReplayRuntime::spawn(replay).unwrap();
        let error = runtime.control(Control::PlayScene("x".into())).await.unwrap_err();
        assert!(error.to_string().contains("no scenes"));

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !runtime.state().replay.unwrap().done,
            "the second payload is 500 ms in"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
        let status = runtime.state().replay.unwrap();
        assert_eq!((status.sent, status.done), (2, true));
        runtime.shutdown().await;

        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            text,
            "{\"device_id\":\"mm1\",\"event\":\"undock\",\"target\":\"m1\"}\n\
             {\"type\":\"vibration\",\"device_id\":\"mm1\",\"value\":0.20}\n"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_scenario_can_open_paused() {
        let path = std::env::temp_dir().join(format!(
            "shopfloor-start-paused-test-{}.jsonl",
            std::process::id()
        ));
        let mut scenario = (*scenario_recording_to(&path)).clone();
        scenario.sim.start_paused = true;
        let runtime = SimRuntime::spawn(Arc::new(scenario)).unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let state = runtime.state();
        assert!(state.paused);
        assert_eq!(state.sim_ms, 0, "the clock waits");

        runtime.control(Control::Resume).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(runtime.state().sim_ms > 0);
        runtime.shutdown().await;

        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let lifecycle: Vec<String> = text
            .lines()
            .map(|line| serde_json::from_str::<SimEvent>(line).unwrap())
            .filter_map(|event| match event.body {
                crate::event::EventBody::Sim { state, .. } => Some(format!("{state:?}")),
                _ => None,
            })
            .collect();
        assert_eq!(lifecycle, ["Started", "Paused", "Resumed"]);
    }

    /// One length-prefixed frame from a TCP sink, as text.
    fn read_frame(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut length = [0u8; 4];
        stream.read_exact(&mut length).unwrap();
        let mut json = vec![0u8; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut json).unwrap();
        String::from_utf8(json).unwrap()
    }

    /// Shared outputs outlive the runtimes writing to them: a TCP client stays connected while a
    /// simulation is replaced by a replay, as when the app switches between them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_outputs_stay_open_across_runtimes() {
        use crate::replay::{Replay, ReplayOptions};
        use crate::trace::parse_trace;
        use std::io::Read;

        let scenario = Arc::new(
            parse_scenario(
                r#"
                name = "Outputs test"

                [floor]
                width_m = 10.0
                height_m = 5.0

                [mobile_manipulators.mm1]
                start = [2.0, 3.0]

                [output.sinks.tcp]
                type = "tcp"
                bind = "127.0.0.1:0"
                "#,
                None,
            )
            .unwrap(),
        );
        let outputs = Outputs::open(&scenario).unwrap();
        let connected = |clients: usize| outputs.status()["tcp"].ends_with(&format!("· {clients} connected"));
        // "listening on 127.0.0.1:PORT · 0 connected"
        let address = outputs.status()["tcp"]
            .trim_start_matches("listening on ")
            .split(' ')
            .next()
            .unwrap()
            .to_string();
        let mut client = std::net::TcpStream::connect(&address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        for _ in 0..200 {
            if connected(1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(connected(1));

        // A simulation (an idle MM and no sensors: it only says it started), then shut down.
        let simulation = SimRuntime::spawn_with(Arc::clone(&scenario), outputs.writer());
        let started = read_frame(&mut client);
        assert!(
            started.contains(r#""kind":"sim""#) && started.contains(r#""state":"started""#),
            "{started}"
        );
        simulation.shutdown().await;
        assert!(connected(1), "the simulation's end leaves the client connected");

        // Then a replay, on the same connection.
        let trace = parse_trace(
            "t.json",
            r#"{"version": 1, "steps": [{"type": "send", "payload": {"device_id": "mm1", "event": "pick"}}]}"#,
        )
        .unwrap();
        let (replay, _) =
            Replay::new(Arc::clone(&scenario), vec![trace], ReplayOptions::default(), 0).unwrap();
        let replay = ReplayRuntime::spawn_with(replay, outputs.writer());
        assert_eq!(read_frame(&mut client), r#"{"device_id":"mm1","event":"pick"}"#);
        replay.shutdown().await;

        // Closing the outputs ends the connection.
        outputs.close().await;
        let mut byte = [0u8; 1];
        assert_eq!(client.read(&mut byte).unwrap(), 0, "connection closed");
    }

    #[tokio::test]
    async fn spawn_fails_when_a_sink_cannot_open() {
        let scenario = scenario_recording_to(Path::new("/dev/null/not-a-dir/x.jsonl"));
        let error = SimRuntime::spawn(scenario).err().unwrap();
        assert!(format!("{error:#}").contains("Failed to"));
    }
}
