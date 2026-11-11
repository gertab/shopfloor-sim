mod bridge;
mod commands;

use commands::{
    SimHost, control_sim, default_scenario_path, get_scenario, get_sink_status, get_state,
    load_scenario, pick_scenario_file, send_command,
};
use tracing_subscriber::EnvFilter;

fn main() {
    // App logs go to stderr: stdout is reserved for the `stdout` event sink (pure JSONL).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(SimHost::default())
        .invoke_handler(tauri::generate_handler![
            default_scenario_path,
            load_scenario,
            pick_scenario_file,
            get_scenario,
            get_state,
            get_sink_status,
            send_command,
            control_sim,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the shopfloor app");
}
