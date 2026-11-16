//! Runs a [`Driver`] in real time on tokio: the [`Engine`] ([`SimRuntime`]) or a trace [`Replay`]
//! ([`ReplayRuntime`]).
//!
//! One task owns the driver and `select!`s over incoming requests and a fixed-rate tick. Its output
//! goes out in batches on a broadcast channel (the UI bridge, the CLI), and to the scenario's sinks
//! through an [`OutputWriter`]. The latest [`WorldState`] is published on a watch channel.
//!
//! The sinks belong to an [`Outputs`]. A runtime started with `spawn` opens its own; the app opens
//! one per loaded scenario and hands it to each runtime (`spawn_with`), so switching between
//! simulating and replaying keeps them open and their consumers connected.

use crate::command::{CommandEnvelope, Control};
use crate::engine::{Engine, WorldState};
use crate::event::SimEvent;
use crate::replay::{READ_ONLY, Replay};
use crate::scenario::Scenario;
use crate::sinks::{Message, ToMessage};
use anyhow::{anyhow, bail};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

mod outputs;
pub use outputs::{OutputWriter, Outputs};

/// A group of events produced by one command or one tick.
pub type EventBatch = Arc<[SimEvent]>;
/// A group of messages for the sinks (or replayed messages) produced by one tick.
pub type MessageBatch = Arc<[Message]>;

const EVENT_CHANNEL_CAPACITY: usize = 1024;
const REQUEST_CHANNEL_CAPACITY: usize = 64;
/// How long a runtime's task gets to stop before it is aborted.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// What a [`Runtime`] drives, one tick at a time: the [`Engine`] (simulating) or a [`Replay`]
/// (playing traces back). The fast runs in [`crate::headless`] drive the same way, without the clock.
pub trait Driver: Send + 'static {
    /// What a tick, command or control produces: simulator events, or replayed messages.
    type Item: ToMessage + Send + Sync + 'static;
    /// What it is called in errors and logs, e.g. "simulation".
    const NAME: &'static str;

    fn scenario(&self) -> &Arc<Scenario>;
    /// Simulation (or replay) time.
    fn sim_ms(&self) -> u64;
    /// The opening batch (e.g. `sim started`). `now_ms` is the wall-clock time events are stamped with.
    fn start(&mut self, now_ms: u64) -> Vec<Self::Item>;
    /// One fixed tick.
    fn step(&mut self, now_ms: u64) -> Vec<Self::Item>;
    fn control(&mut self, control: Control, now_ms: u64) -> anyhow::Result<Vec<Self::Item>>;
    fn apply(&mut self, envelope: CommandEnvelope, now_ms: u64) -> anyhow::Result<Vec<Self::Item>>;
    fn state(&self) -> WorldState;
    /// Whether a real-time run opens paused (a scenario's `start_paused`). Fast runs ignore it.
    fn starts_paused(&self) -> bool {
        false
    }
    /// Whether it has nothing left to produce (a replay that has sent everything).
    fn is_done(&self) -> bool {
        false
    }
}

impl Driver for Engine {
    type Item = SimEvent;
    const NAME: &'static str = "simulation";

    fn scenario(&self) -> &Arc<Scenario> {
        Engine::scenario(self)
    }
    fn sim_ms(&self) -> u64 {
        Engine::sim_ms(self)
    }
    fn start(&mut self, now_ms: u64) -> Vec<SimEvent> {
        Engine::start(self, now_ms)
    }
    fn step(&mut self, now_ms: u64) -> Vec<SimEvent> {
        Engine::step(self, now_ms)
    }
    fn control(&mut self, control: Control, now_ms: u64) -> anyhow::Result<Vec<SimEvent>> {
        Engine::control(self, control, now_ms)
    }
    fn apply(&mut self, envelope: CommandEnvelope, now_ms: u64) -> anyhow::Result<Vec<SimEvent>> {
        Engine::apply(self, envelope, now_ms)
    }
    fn state(&self) -> WorldState {
        Engine::state(self)
    }
    fn starts_paused(&self) -> bool {
        Engine::scenario(self).sim.start_paused
    }
}

impl Driver for Replay {
    type Item = Message;
    const NAME: &'static str = "replay";

    fn scenario(&self) -> &Arc<Scenario> {
        Replay::scenario(self)
    }
    fn sim_ms(&self) -> u64 {
        Replay::sim_ms(self)
    }
    fn start(&mut self, _now_ms: u64) -> Vec<Message> {
        Replay::start(self)
    }
    fn step(&mut self, _now_ms: u64) -> Vec<Message> {
        Replay::step(self)
    }
    fn control(&mut self, control: Control, now_ms: u64) -> anyhow::Result<Vec<Message>> {
        Replay::control(self, control, now_ms)
    }
    /// A replay is read-only: it takes no commands.
    fn apply(&mut self, _envelope: CommandEnvelope, _now_ms: u64) -> anyhow::Result<Vec<Message>> {
        bail!(READ_ONLY)
    }
    fn state(&self) -> WorldState {
        Replay::state(self)
    }
    fn is_done(&self) -> bool {
        Replay::is_done(self)
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

/// Plays a [`Driver`] in real time.
pub struct Runtime<D: Driver> {
    scenario: Arc<Scenario>,
    requests: mpsc::Sender<Request>,
    events: broadcast::Sender<Arc<[D::Item]>>,
    state: watch::Receiver<WorldState>,
    task: JoinHandle<()>,
    writer: OutputWriter,
    /// Outputs this runtime opened itself (`spawn`), closed when it shuts down.
    owned_outputs: Option<Outputs>,
}

/// Simulates a scenario in real time.
pub type SimRuntime = Runtime<Engine>;
/// Replays traces on a scenario's layout in real time. It takes no commands: only pause, resume
/// and reset (start again).
pub type ReplayRuntime = Runtime<Replay>;

impl<D: Driver> Runtime<D> {
    /// Opens the scenario's sinks and starts the driver. Must be called inside a tokio runtime.
    pub fn spawn(driver: D) -> anyhow::Result<Self> {
        // The sinks are open before the driver starts, so they see its opening batch.
        let outputs = Outputs::open(driver.scenario())?;
        let mut runtime = Self::spawn_with(driver, outputs.writer());
        runtime.owned_outputs = Some(outputs);
        Ok(runtime)
    }

    /// Starts the driver, writing to sinks that are already open (and stay open after it).
    pub fn spawn_with(driver: D, writer: OutputWriter) -> Self {
        let scenario = Arc::clone(driver.scenario());
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (state_tx, state) = watch::channel(driver.state());
        let (requests, request_rx) = mpsc::channel(REQUEST_CHANNEL_CAPACITY);
        let task = tokio::spawn(run(driver, request_rx, events.clone(), writer.clone(), state_tx));
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

    /// Live status of sinks that report one (TCP: address and connected clients), by sink name.
    pub fn sink_status(&self) -> BTreeMap<String, String> {
        self.writer.status()
    }

    pub fn scenario(&self) -> &Arc<Scenario> {
        &self.scenario
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<[D::Item]>> {
        self.events.subscribe()
    }

    pub fn watch_state(&self) -> watch::Receiver<WorldState> {
        self.state.clone()
    }

    pub fn state(&self) -> WorldState {
        self.state.borrow().clone()
    }

    /// Executes a command; returns the driver's error, if any (a replay refuses every command).
    pub async fn send(&self, envelope: CommandEnvelope) -> anyhow::Result<()> {
        self.request(|reply| Request::Command(envelope, reply)).await
    }

    /// Pause / resume / reset, or play / stop a scene; returns the driver's error, if any.
    pub async fn control(&self, control: Control) -> anyhow::Result<()> {
        self.request(|reply| Request::Control(control, reply)).await
    }

    async fn request(
        &self,
        request: impl FnOnce(oneshot::Sender<anyhow::Result<()>>) -> Request,
    ) -> anyhow::Result<()> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(request(reply))
            .await
            .map_err(|_| anyhow!("The {} is not running", D::NAME))?;
        response
            .await
            .map_err(|_| anyhow!("The {} stopped before answering", D::NAME))?
    }

    /// Stops the driver. Sinks it opened itself then drain and close; shared ones stay open.
    pub async fn shutdown(self) {
        let Self {
            requests,
            task,
            writer,
            owned_outputs,
            ..
        } = self;
        let _ = requests.send(Request::Shutdown).await;
        stop_task(task, D::NAME).await;
        // Sinks run until every writer is gone, this one included.
        drop(writer);
        if let Some(outputs) = owned_outputs {
            outputs.close().await;
        }
    }
}

async fn run<D: Driver>(
    mut driver: D,
    mut requests: mpsc::Receiver<Request>,
    events: broadcast::Sender<Arc<[D::Item]>>,
    outputs: OutputWriter,
    state: watch::Sender<WorldState>,
) {
    let tick = Duration::from_millis(driver.scenario().sim.tick_ms);
    let mut interval = tokio::time::interval(tick);
    // After a stall, carry on from now instead of bursting through the missed ticks.
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval.tick().await; // the first tick completes immediately

    publish(&events, &outputs, driver.start(now_ms()));
    // A scenario can open paused: `sim started`, then `sim paused`, and the clock waits at 0.
    if driver.starts_paused() {
        let batch = driver
            .control(Control::Pause, now_ms())
            .expect("pausing never fails");
        publish(&events, &outputs, batch);
        state.send_replace(driver.state());
    }
    loop {
        tokio::select! {
            request = requests.recv() => match request {
                Some(Request::Command(envelope, reply)) => {
                    let result = driver
                        .apply(envelope, now_ms())
                        .map(|batch| publish(&events, &outputs, batch));
                    let _ = reply.send(result);
                }
                Some(Request::Control(control, reply)) => {
                    let result = driver
                        .control(control, now_ms())
                        .map(|batch| publish(&events, &outputs, batch));
                    let _ = reply.send(result);
                }
                Some(Request::Shutdown) | None => break,
            },
            _ = interval.tick() => publish(&events, &outputs, driver.step(now_ms())),
        }
        let latest = driver.state();
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

/// Sends a batch to the runtime's subscribers (UI bridge, CLI) and, as messages, to the sinks
/// (converted only if a sink is listening).
fn publish<T: ToMessage>(events: &broadcast::Sender<Arc<[T]>>, outputs: &OutputWriter, batch: Vec<T>) {
    if batch.is_empty() {
        return;
    }
    let batch: Arc<[T]> = batch.into();
    if outputs.wanted() {
        outputs.write(T::to_messages(&batch));
    }
    // An error only means nobody is subscribed right now.
    let _ = events.send(batch);
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
