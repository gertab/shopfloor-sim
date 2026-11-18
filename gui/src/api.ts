import { invoke } from "@tauri-apps/api/core";
import type { Control, Scenario, SimCommand, WorldState } from "./types";

// Typed wrappers around the Tauri commands in src-tauri/src/commands.rs.
export const api = {
  loadScenario: (path?: string) => invoke<Scenario>("load_scenario", { path: path ?? null }),
  pickScenarioFile: () => invoke<string | null>("pick_scenario_file"),
  pickTraceFiles: () => invoke<string[] | null>("pick_trace_files"),
  /** Replays traces on the loaded layout; returns what the checks found. */
  startReplay: (paths: string[], envelope: boolean, positions: boolean) =>
    invoke<string[]>("start_replay", { paths, envelope, positions }),
  stopReplay: () => invoke<void>("stop_replay"),
  /** The scenario that runs now, if any. */
  getScenario: () => invoke<Scenario | null>("get_scenario"),
  getState: () => invoke<WorldState | null>("get_state"),
  sendCommand: (command: SimCommand) => invoke<void>("send_command", { command }),
  control: (control: Control) => invoke<void>("control_sim", { control }),
  /** Live status of sinks that report one (TCP), by sink name. */
  getSinkStatus: () => invoke<Record<string, string>>("get_sink_status"),
};

export const STATE_EVENT = "shopfloor://state";
export const EVENTS_EVENT = "shopfloor://events";
