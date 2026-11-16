//! Tauri commands. Thin: everything goes through [`SimRuntime`], or [`ReplayRuntime`] while a
//! trace is replayed.

use crate::bridge::Bridge;
use shopfloor_core::replay::{Format, READ_ONLY, Replay, ReplayOptions};
use shopfloor_core::runtime::{ReplayRuntime, now_ms};
use shopfloor_core::{
    CommandEnvelope, Control, Origin, Scenario, SimCommand, SimRuntime, WorldState,
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
}

/// The loaded scenario either simulates or replays traces on its layout.
enum Runtime {
    Sim(SimRuntime),
    Replay(ReplayRuntime),
}

impl Runtime {
    /// Starts it, and the bridge to the webview.
    fn start(self, app: AppHandle) -> Running {
        let bridge = match &self {
            Runtime::Sim(runtime) => Bridge::start(app, runtime.watch_state(), runtime.subscribe()),
            Runtime::Replay(runtime) => Bridge::start(app, runtime.watch_state(), runtime.subscribe()),
        };
        Running {
            runtime: self,
            bridge,
        }
    }

    fn scenario(&self) -> &Arc<Scenario> {
        match self {
            Runtime::Sim(runtime) => runtime.scenario(),
            Runtime::Replay(runtime) => runtime.scenario(),
        }
    }

    fn state(&self) -> WorldState {
        match self {
            Runtime::Sim(runtime) => runtime.state(),
            Runtime::Replay(runtime) => runtime.state(),
        }
    }

    fn sink_status(&self) -> BTreeMap<String, String> {
        match self {
            Runtime::Sim(runtime) => runtime.sink_status(),
            Runtime::Replay(runtime) => runtime.sink_status(),
        }
    }

    /// Manual commands only reach a simulation: a replay is read-only.
    async fn send(&self, command: SimCommand) -> anyhow::Result<()> {
        match self {
            Runtime::Sim(runtime) => runtime.send(CommandEnvelope::new(Origin::Ui, command)).await,
            Runtime::Replay(_) => anyhow::bail!(READ_ONLY),
        }
    }

    async fn control(&self, control: Control) -> anyhow::Result<()> {
        match self {
            Runtime::Sim(runtime) => runtime.control(control).await,
            Runtime::Replay(runtime) => runtime.control(control).await,
        }
    }

    async fn shutdown(self) {
        match self {
            Runtime::Sim(runtime) => runtime.shutdown().await,
            Runtime::Replay(runtime) => runtime.shutdown().await,
        }
    }
}

impl Running {
    async fn stop(self) {
        self.bridge.stop();
        self.runtime.shutdown().await;
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
    scenarios_dir().join(SAMPLE_SCENARIO).to_string_lossy().to_string()
}

/// Loads a scenario (default: `scenarios/sample-floor.toml`) and (re)starts the simulation with it.
/// A scenario's `[replay]` only sets what the Replay section starts with: replays start from there
/// (`start_replay`). If the file is invalid, the current run keeps going.
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
    if let Some(previous) = running.take() {
        previous.stop().await;
    }
    let runtime = SimRuntime::spawn(Arc::clone(&scenario)).map_err(|e| format!("{e:#}"))?;
    tracing::info!("loaded scenario '{}' from {}", scenario.name, path.display());
    *running = Some(Runtime::Sim(runtime).start(app));
    Ok((*scenario).clone())
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
        format: if envelope { Format::Envelope } else { Format::Payload },
        positions,
    };
    let (replay, warnings) = build_replay(&scenario, &paths, options).map_err(|e| format!("{e:#}"))?;
    // Only now: the replay's sinks need the ports the simulation held.
    if let Some(previous) = running.take() {
        previous.stop().await;
    }
    let runtime = ReplayRuntime::spawn(replay).map_err(|e| format!("{e:#}"))?;
    tracing::info!("replaying {} trace(s) on '{}'", paths.len(), scenario.name);
    *running = Some(Runtime::Replay(runtime).start(app));
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
    let scenario = Arc::clone(current.runtime.scenario());
    current.stop().await;
    let runtime = SimRuntime::spawn(scenario).map_err(|e| format!("{e:#}"))?;
    *running = Some(Runtime::Sim(runtime).start(app));
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
pub(crate) async fn get_sink_status(
    host: State<'_, SimHost>,
) -> Result<BTreeMap<String, String>, String> {
    let running = host.running.lock().await;
    Ok(running
        .as_ref()
        .map(|running| running.runtime.sink_status())
        .unwrap_or_default())
}

#[tauri::command]
pub(crate) async fn send_command(
    host: State<'_, SimHost>,
    command: SimCommand,
) -> Result<(), String> {
    let running = host.running.lock().await;
    let running = running.as_ref().ok_or("No scenario loaded")?;
    running
        .runtime
        .send(command)
        .await
        .map_err(|e| format!("{e:#}"))
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

    fn replay_runtime() -> Runtime {
        let path = scenarios_dir().join(SAMPLE_SCENARIO);
        let mut scenario = shopfloor_core::load_scenario(&path).unwrap();
        scenario.output.sinks.clear();
        let trace = shopfloor_core::trace::parse_trace(
            "t.json",
            r#"{"version": 1, "steps": [{"type": "send", "payload": {"device_id": "mm1", "event": "pick"}}]}"#,
        )
        .unwrap();
        let (replay, _) =
            Replay::new(Arc::new(scenario), vec![trace], ReplayOptions::default(), 0).unwrap();
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
        assert!(runtime.control(Control::PlayScene("production".into())).await.is_err());
        assert!(runtime.control(Control::Pause).await.is_ok());
        runtime.shutdown().await;
    }

    #[test]
    fn the_app_opens_the_sample_scenario_in_scenarios() {
        let path = PathBuf::from(default_scenario_path());
        assert!(path.ends_with("scenarios/sample-floor.toml"), "{}", path.display());
        shopfloor_core::load_scenario(&path).expect("sample scenario loads");
    }
}
