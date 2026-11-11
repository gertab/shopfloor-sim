import { invoke } from "@tauri-apps/api/core";
import type { Control, Scenario, SimCommand, WorldState } from "./types";

// Typed wrappers around the Tauri commands in src-tauri/src/commands.rs.
export const api = {
  loadScenario: (path?: string) => invoke<Scenario>("load_scenario", { path: path ?? null }),
  pickScenarioFile: () => invoke<string | null>("pick_scenario_file"),
  getState: () => invoke<WorldState | null>("get_state"),
  sendCommand: (command: SimCommand) => invoke<void>("send_command", { command }),
  control: (control: Control) => invoke<void>("control_sim", { control }),
  /** Live status of sinks that report one (TCP), by sink name. */
  getSinkStatus: () => invoke<Record<string, string>>("get_sink_status"),
};

export const STATE_EVENT = "shopfloor://state";
export const EVENTS_EVENT = "shopfloor://events";
