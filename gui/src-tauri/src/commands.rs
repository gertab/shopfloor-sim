//! Tauri commands. Thin: everything goes through [`SimRuntime`].

use crate::bridge::Bridge;
use shopfloor_core::{
    CommandEnvelope, Control, Origin, Scenario, SimCommand, SimRuntime, WorldState,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, FilePath};
use tokio::sync::Mutex;

/// The running simulation, if a scenario is loaded.
#[derive(Default)]
pub(crate) struct SimHost {
    running: Mutex<Option<Running>>,
}

struct Running {
    runtime: SimRuntime,
    bridge: Bridge,
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
/// If the file is invalid, the current simulation keeps running.
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
        previous.bridge.stop();
        previous.runtime.shutdown().await;
    }
    let runtime = SimRuntime::spawn(Arc::clone(&scenario)).map_err(|e| format!("{e:#}"))?;
    let bridge = Bridge::start(app, &runtime);
    tracing::info!("loaded scenario '{}' from {}", scenario.name, path.display());
    *running = Some(Running { runtime, bridge });
    Ok((*scenario).clone())
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
        .send(CommandEnvelope::new(Origin::Ui, command))
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

    #[test]
    fn the_app_opens_the_sample_scenario_in_scenarios() {
        let path = PathBuf::from(default_scenario_path());
        assert!(path.ends_with("scenarios/sample-floor.toml"), "{}", path.display());
        shopfloor_core::load_scenario(&path).expect("sample scenario loads");
    }
}
