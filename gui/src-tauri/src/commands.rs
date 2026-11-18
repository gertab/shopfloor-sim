//! Tauri commands. Thin: everything goes through [`SimRuntime`], or [`ReplayRuntime`] while a
//! trace is replayed.
//!
//! The loaded scenario's sinks ([`Outputs`]) outlive both: switching between simulating and
//! replaying keeps them open, so a consumer connected over TCP stays connected. Loading a scenario
//! (Open, Reload) opens fresh ones, since its outputs may differ.

use crate::bridge::Bridge;
use shopfloor_core::replay::{Format, Replay, ReplayOptions};
use shopfloor_core::runtime::{Outputs, ReplayRuntime, now_ms};
use shopfloor_core::{
    CommandEnvelope, Control, Engine, Origin, Scenario, SimCommand, SimRuntime, WorldState,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, FilePath};
use tokio::sync::Mutex;

/// The running simulation (or replay), if a scenario is loaded.
#[derive(Default)]
pub(crate) struct SimHost {
    running: Mutex<Option<Running>>,
}

struct Running {
    runtime: Runtime,
    bridge: Bridge,
    outputs: Outputs,
}

/// The loaded scenario either simulates or replays traces on its layout.
enum Runtime {
    Sim(SimRuntime),
    Replay(ReplayRuntime),
}

/// Calls the same method on whichever runtime is running.
macro_rules! on_runtime {
    ($runtime:expr, $r:ident => $call:expr) => {
        match $runtime {
            Runtime::Sim($r) => $call,
            Runtime::Replay($r) => $call,
        }
    };
}

impl Runtime {
    /// The bridge forwarding it to the webview.
    fn bridge(&self, app: AppHandle) -> Bridge {
        on_runtime!(self, r => Bridge::start(app, r.watch_state(), r.subscribe()))
    }

    fn scenario(&self) -> &Arc<Scenario> {
        on_runtime!(self, r => r.scenario())
    }

    fn state(&self) -> WorldState {
        on_runtime!(self, r => r.state())
    }

    /// Manual commands only reach a simulation: a replay refuses them as read-only.
    async fn send(&self, command: SimCommand) -> anyhow::Result<()> {
        let envelope = CommandEnvelope::new(Origin::Ui, command);
        on_runtime!(self, r => r.send(envelope).await)
    }

    async fn control(&self, control: Control) -> anyhow::Result<()> {
        on_runtime!(self, r => r.control(control).await)
    }

    async fn shutdown(self) {
        on_runtime!(self, r => r.shutdown().await)
    }
}

impl Running {
    fn new(app: AppHandle, runtime: Runtime, outputs: Outputs) -> Self {
        let bridge = runtime.bridge(app);
        Self {
            runtime,
            bridge,
            outputs,
        }
    }

    /// Stops the runtime and its bridge, keeping the outputs open for the next runtime.
    async fn stop_runtime(self) -> (Arc<Scenario>, Outputs) {
        let scenario = Arc::clone(self.runtime.scenario());
        self.bridge.stop();
        self.runtime.shutdown().await;
        (scenario, self.outputs)
    }

    /// Stops everything, outputs included. Returns the scenario it ran.
    async fn stop(self) -> Arc<Scenario> {
        let (scenario, outputs) = self.stop_runtime().await;
        outputs.close().await;
        scenario
    }
}

/// Opens a scenario's outputs and simulates it from the start.
fn start_simulation(app: &AppHandle, scenario: &Arc<Scenario>) -> anyhow::Result<Running> {
    let outputs = Outputs::open(scenario)?;
    let runtime = Runtime::Sim(SimRuntime::spawn_with(
        Engine::new(Arc::clone(scenario)),
        outputs.writer(),
    ));
    Ok(Running::new(app.clone(), runtime, outputs))
}

/// What [`start_or_restore`] ended up running.
enum Loaded<T> {
    /// The new scenario.
    Started(T),
    /// The new scenario couldn't start (the error says why), so the previous one runs again.
    Restored(T, String),
    /// Nothing could start.
    Failed(String),
}

/// Starts `scenario` with `start`. If it can't start (e.g. a port of its outputs is in use) and
/// `previous` ran until now, starts that again, from the beginning, so that something runs.
fn start_or_restore<T>(
    scenario: &Arc<Scenario>,
    previous: Option<&Arc<Scenario>>,
    start: impl Fn(&Arc<Scenario>) -> anyhow::Result<T>,
) -> Loaded<T> {
    let error = match start(scenario) {
        Ok(started) => return Loaded::Started(started),
        Err(error) => format!("{error:#}"),
    };
    let Some(previous) = previous else {
        return Loaded::Failed(error);
    };
    match start(previous) {
        Ok(restored) => {
            tracing::warn!(
                "'{}' could not start, so '{}' runs again: {error}",
                scenario.name,
                previous.name
            );
            let message = format!("{error}\nBack to '{}', started again.", previous.name);
            Loaded::Restored(restored, message)
        }
        Err(again) => Loaded::Failed(format!(
            "{error}\n'{}' could not start again either: {again:#}",
            previous.name
        )),
    }
}

/// Builds a replay of `paths` on `scenario`: the traces are read and checked, nothing starts yet.
fn build_replay(
    scenario: &Arc<Scenario>,
    paths: &[PathBuf],
    options: ReplayOptions,
) -> anyhow::Result<(Replay, Vec<String>)> {
    let (replay, findings) = Replay::from_files(Arc::clone(scenario), paths, options, now_ms())?;
    Ok((replay, findings.iter().map(ToString::to_string).collect()))
}

/// `scenarios/` at the project root, where the scenario files live. Fixed at compile time.
fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("gui/src-tauri is nested under the project root")
        .join("scenarios")
}

/// The scenario the app opens.
const SAMPLE_SCENARIO: &str = "sample-floor.toml";

#[tauri::command]
pub(crate) fn default_scenario_path() -> String {
    scenarios_dir()
        .join(SAMPLE_SCENARIO)
        .to_string_lossy()
        .to_string()
}

/// Loads a scenario (default: `scenarios/sample-floor.toml`) and (re)starts the simulation with it.
/// A scenario's `[replay]` only sets what the Replay section starts with: replays start from there
/// (`start_replay`). If the file is invalid, the current run keeps going. If its outputs can't open
/// (e.g. a port in use), the previous scenario starts again, so the app isn't left with nothing.
#[tauri::command]
pub(crate) async fn load_scenario(
    app: AppHandle,
    host: State<'_, SimHost>,
    path: Option<String>,
) -> Result<Scenario, String> {
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| scenarios_dir().join(SAMPLE_SCENARIO));
    let scenario = Arc::new(shopfloor_core::load_scenario(&path).map_err(|e| format!("{e:#}"))?);

    let mut running = host.running.lock().await;
    let previous = match running.take() {
        Some(previous) => Some(previous.stop().await),
        None => None,
    };
    // Only now: the new outputs may need the ports the previous ones held.
    match start_or_restore(&scenario, previous.as_ref(), |scenario| {
        start_simulation(&app, scenario)
    }) {
        Loaded::Started(started) => {
            tracing::info!("loaded scenario '{}' from {}", scenario.name, path.display());
            *running = Some(started);
            Ok((*scenario).clone())
        }
        Loaded::Restored(restored, error) => {
            *running = Some(restored);
            Err(error)
        }
        Loaded::Failed(error) => Err(error),
    }
}

/// Replays these trace files on the loaded scenario's layout, replacing what runs now. Returns
/// what the checks found. If the traces can't be read, the current run keeps going.
#[tauri::command]
pub(crate) async fn start_replay(
    app: AppHandle,
    host: State<'_, SimHost>,
    paths: Vec<String>,
    envelope: bool,
    positions: bool,
) -> Result<Vec<String>, String> {
    let mut running = host.running.lock().await;
    let scenario = Arc::clone(running.as_ref().ok_or("No scenario loaded")?.runtime.scenario());
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let options = ReplayOptions {
        format: if envelope {
            Format::Envelope
        } else {
            Format::Payload
        },
        positions,
    };
    let (replay, warnings) = build_replay(&scenario, &paths, options).map_err(|e| format!("{e:#}"))?;
    let previous = running.take().ok_or("No scenario loaded")?;
    let (_, outputs) = previous.stop_runtime().await;
    let runtime = Runtime::Replay(ReplayRuntime::spawn_with(replay, outputs.writer()));
    tracing::info!("replaying {} trace(s) on '{}'", paths.len(), scenario.name);
    *running = Some(Running::new(app, runtime, outputs));
    Ok(warnings)
}

/// Stops a replay and simulates the same layout instead.
#[tauri::command]
pub(crate) async fn stop_replay(app: AppHandle, host: State<'_, SimHost>) -> Result<(), String> {
    let mut running = host.running.lock().await;
    let Some(current) = running.take() else {
        return Err("No scenario loaded".to_string());
    };
    if matches!(current.runtime, Runtime::Sim(_)) {
        *running = Some(current);
        return Ok(());
    }
    let (scenario, outputs) = current.stop_runtime().await;
    let runtime = Runtime::Sim(SimRuntime::spawn_with(Engine::new(scenario), outputs.writer()));
    *running = Some(Running::new(app, runtime, outputs));
    Ok(())
}

#[tauri::command]
pub(crate) async fn get_scenario(host: State<'_, SimHost>) -> Result<Option<Scenario>, String> {
    let running = host.running.lock().await;
    Ok(running
        .as_ref()
        .map(|running| (**running.runtime.scenario()).clone()))
}

#[tauri::command]
pub(crate) async fn get_state(host: State<'_, SimHost>) -> Result<Option<WorldState>, String> {
    let running = host.running.lock().await;
    Ok(running.as_ref().map(|running| running.runtime.state()))
}

/// Live status of sinks that report one (TCP: address and connected clients), by sink name.
#[tauri::command]
pub(crate) async fn get_sink_status(host: State<'_, SimHost>) -> Result<BTreeMap<String, String>, String> {
    let running = host.running.lock().await;
    Ok(running
        .as_ref()
        .map(|running| running.outputs.status())
        .unwrap_or_default())
}

#[tauri::command]
pub(crate) async fn send_command(host: State<'_, SimHost>, command: SimCommand) -> Result<(), String> {
    let running = host.running.lock().await;
    let running = running.as_ref().ok_or("No scenario loaded")?;
    running.runtime.send(command).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub(crate) async fn control_sim(host: State<'_, SimHost>, control: Control) -> Result<(), String> {
    let running = host.running.lock().await;
    let running = running.as_ref().ok_or("No scenario loaded")?;
    running
        .runtime
        .control(control)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// File picker for trace files (several at once), starting in the loaded scenario's folder.
#[tauri::command]
pub(crate) async fn pick_trace_files(
    window: tauri::Window,
    host: State<'_, SimHost>,
) -> Result<Option<Vec<String>>, String> {
    let directory = {
        let running = host.running.lock().await;
        running
            .as_ref()
            .and_then(|running| running.runtime.scenario().source_path.clone())
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(scenarios_dir)
    };
    let (sender, receiver) = std::sync::mpsc::channel();

    window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("Liminal Desk traces to replay")
        .set_directory(directory)
        .add_filter("Trace (JSON)", &["json"])
        .pick_files(move |selected| {
            let paths = selected.map(|paths| {
                paths
                    .into_iter()
                    .map(dialog_path_to_string)
                    .collect::<Result<Vec<_>, _>>()
            });
            let _ = sender.send(paths.transpose());
        });

    tauri::async_runtime::spawn_blocking(move || {
        receiver
            .recv()
            .unwrap_or_else(|_| Err("Dialog closed without returning a result".to_string()))
    })
    .await
    .map_err(|error| format!("Failed to receive dialog result: {}", error))?
}

/// File picker that starts in `scenarios/`.
#[tauri::command]
pub(crate) async fn pick_scenario_file(window: tauri::Window) -> Result<Option<String>, String> {
    let (sender, receiver) = std::sync::mpsc::channel();

    window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("Open Scenario")
        .set_directory(scenarios_dir())
        .add_filter("Scenario (TOML)", &["toml"])
        .pick_file(move |selected| {
            let _ = sender.send(selected.map(dialog_path_to_string).transpose());
        });

    tauri::async_runtime::spawn_blocking(move || {
        receiver
            .recv()
            .unwrap_or_else(|_| Err("Dialog closed without returning a result".to_string()))
    })
    .await
    .map_err(|error| format!("Failed to receive dialog result: {}", error))?
}

fn dialog_path_to_string(path: FilePath) -> Result<String, String> {
    path.simplified()
        .into_path()
        .map(|path| path.to_string_lossy().to_string())
        .map_err(|error| format!("Selected path is not a filesystem path: {}", error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shopfloor_core::replay::READ_ONLY;

    fn replay_runtime() -> Runtime {
        let path = scenarios_dir().join(SAMPLE_SCENARIO);
        let mut scenario = shopfloor_core::load_scenario(&path).unwrap();
        scenario.output.sinks.clear();
        let trace = shopfloor_core::trace::parse_trace(
            "t.json",
            r#"{"version": 1, "steps": [{"type": "send", "payload": {"device_id": "mm1", "event": "pick"}}]}"#,
        )
        .unwrap();
        let (replay, _) = Replay::new(Arc::new(scenario), vec![trace], ReplayOptions::default(), 0).unwrap();
        Runtime::Replay(ReplayRuntime::spawn(replay).unwrap())
    }

    #[tokio::test]
    async fn a_replay_refuses_manual_commands_and_scenes() {
        let runtime = replay_runtime();
        let error = runtime
            .send(SimCommand::Stop { mm: "mm1".into() })
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), READ_ONLY);
        assert!(
            runtime
                .control(Control::PlayScene("production".into()))
                .await
                .is_err()
        );
        assert!(runtime.control(Control::Pause).await.is_ok());
        runtime.shutdown().await;
    }

    /// A scenario named `name`, with a TCP output listening on `bind` if given.
    fn scenario_listening_on(name: &str, bind: Option<&str>) -> Arc<Scenario> {
        let output = bind.map_or_else(String::new, |bind| {
            format!("[output.sinks.tcp]\ntype = \"tcp\"\nbind = \"{bind}\"\n")
        });
        let text = format!("name = \"{name}\"\n[floor]\nwidth_m = 10.0\nheight_m = 5.0\n{output}");
        Arc::new(shopfloor_core::parse_scenario(&text, None).unwrap())
    }

    #[tokio::test]
    async fn a_scenario_whose_port_is_taken_brings_the_previous_one_back() {
        // Another process (e.g. a CLI run) listens on the new scenario's port.
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let bind = taken.local_addr().unwrap().to_string();
        let previous = scenario_listening_on("Previous", Some("127.0.0.1:0"));
        let next = scenario_listening_on("Next", Some(&bind));

        match start_or_restore(&next, Some(&previous), |scenario| Outputs::open(scenario)) {
            Loaded::Restored(outputs, error) => {
                assert!(
                    error.starts_with(&format!("Failed to listen on {bind}")),
                    "{error}"
                );
                assert!(error.ends_with("\nBack to 'Previous', started again."), "{error}");
                outputs.close().await;
            }
            _ => panic!("expected the previous scenario to run again"),
        }
        match start_or_restore(&next, None, |scenario| Outputs::open(scenario)) {
            Loaded::Failed(error) => assert!(error.starts_with("Failed to listen on"), "{error}"),
            _ => panic!("with no previous scenario, nothing runs"),
        }

        drop(taken);
        match start_or_restore(&next, Some(&previous), |scenario| Outputs::open(scenario)) {
            Loaded::Started(outputs) => outputs.close().await,
            _ => panic!("with the port free, the new scenario starts"),
        }
    }

    #[tokio::test]
    async fn when_neither_scenario_can_start_both_reasons_are_given() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let bind = taken.local_addr().unwrap().to_string();
        let previous = scenario_listening_on("Previous", Some(&bind));
        let next = scenario_listening_on("Next", Some(&bind));
        match start_or_restore(&next, Some(&previous), |scenario| Outputs::open(scenario)) {
            Loaded::Failed(error) => {
                assert!(error.starts_with("Failed to listen on"), "{error}");
                assert!(
                    error.contains("\n'Previous' could not start again either: Failed to listen on"),
                    "{error}"
                );
            }
            _ => panic!("expected nothing to run"),
        }
    }

    #[test]
    fn the_app_opens_the_sample_scenario_in_scenarios() {
        let path = PathBuf::from(default_scenario_path());
        assert!(
            path.ends_with("scenarios/sample-floor.toml"),
            "{}",
            path.display()
        );
        shopfloor_core::load_scenario(&path).expect("sample scenario loads");
    }
}
