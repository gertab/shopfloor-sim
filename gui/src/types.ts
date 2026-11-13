// Hand-written mirror of the Rust types in core/src (scenario.rs, engine.rs, event.rs, command.rs).

export type Vec2 = { x: number; y: number };

// --- Static layout (scenario.rs: Scenario) -----------------------------------------------------

/** An MQTT topic and the events it carries. Empty filters match everything. */
export interface MqttTopic {
  topic: string;
  kinds: EventKind[];
  entities: string[];
}

export type SinkDef =
  | { name: string; type: "stdout" }
  | { name: string; type: "jsonl"; path: string; append: boolean }
  | { name: string; type: "tcp"; bind: string }
  | { name: string; type: "mqtt"; broker: string; topics: MqttTopic[]; qos: number; client_id: string | null };

export interface MachineDef {
  id: string;
  label: string;
  position: Vec2;
  size: Vec2;
  dock: Vec2;
  sensors: string[];
}

export interface MmDef {
  id: string;
  label: string;
  start: Vec2;
  start_at: string | null;
  start_heading_rad: number;
  max_speed_mps: number;
  radius_m: number;
  /** Ids of the actions this MM can perform. */
  actions: string[];
  sensors: string[];
}

export interface ActionDef {
  id: string;
  label: string;
  requires_machine: boolean;
  allowed_while_moving: boolean;
}

/** "floor" = a free-standing sensor at a fixed position; it is its own entity. */
export type HostKind = "machine" | "mobile_manipulator" | "floor";

export interface SensorDef {
  id: string;
  name: string;
  host: string;
  host_kind: HostKind;
  kind: string;
  label: string;
  unit: string;
  default: number;
  min: number;
  max: number;
  period_ms: number;
  noise_std: number;
  /** Fixed position of a floor sensor; null for mounted sensors. */
  position: Vec2 | null;
}

export interface Scenario {
  name: string;
  source_path: string | null;
  sim: { tick_ms: number; seed: number; dock_tolerance_m: number };
  floor: { width_m: number; height_m: number; grid_m: number };
  machines: MachineDef[];
  mobile_manipulators: MmDef[];
  sensors: SensorDef[];
  actions: ActionDef[];
  scenes: SceneDef[];
  output: { position_interval_ms: number; sinks: SinkDef[] };
}

// --- Scenes (scene.rs) --------------------------------------------------------------------------

export type Repeat = "once" | "forever" | { times: number };

export type SceneStep = { command: SimCommand } | { wait_ms: number } | { wait_idle: string };

export interface SceneDef {
  id: string;
  label: string;
  repeat: Repeat;
  tracks: { name: string; steps: SceneStep[] }[];
}

export interface TrackStatus {
  name: string;
  /** 1-based index of the step being waited on (equals `steps` when done). */
  step: number;
  steps: number;
  /** What the track waits on, e.g. "wait 1.5 s (0.4 s left)"; null when done. */
  current: string | null;
  done: boolean;
}

export interface SceneStatus {
  id: string;
  label: string;
  iteration: number;
  tracks: TrackStatus[];
}

// --- Dynamic state (engine.rs: WorldState) ------------------------------------------------------

export interface Place {
  x: number;
  y: number;
  at: string | null;
}

export interface MotionView {
  command_id: number;
  target: Place;
  remaining_m: number;
  eta_ms: number;
}

export interface MmView {
  id: string;
  x: number;
  y: number;
  heading_rad: number;
  speed_mps: number;
  at: string | null;
  motion: MotionView | null;
  /** This MM's actions and whether each can be performed right now (rules live in the engine). */
  actions: ActionAvailability[];
}

export interface ActionAvailability {
  id: string;
  available: boolean;
  reason: string | null;
}

export interface SensorView {
  id: string;
  setpoint: number;
  noise_std: number;
  paused: boolean;
  last_value: number | null;
  last_ms: number | null;
}

export interface WorldState {
  sim_ms: number;
  paused: boolean;
  mobile_manipulators: MmView[];
  sensors: SensorView[];
  /** The playing scene, if any. */
  scene: SceneStatus | null;
}

// --- Output events (event.rs: SimEvent) — the consumer contract ---------------------------------

export type Origin = "ui" | "scene" | "api";

interface Envelope {
  v: number;
  seq: number;
  timestamp_ms: number;
  sim_ms: number;
}

export type SimEvent = Envelope &
  (
    | { kind: "position"; device_id: string; x: number; y: number; heading_rad: number; speed_mps: number }
    | {
        kind: "move_command";
        device_id: string;
        event: "move";
        source: string | null;
        destination: string | null;
        command_id: number;
        origin: Origin;
        from: Vec2;
        to: Vec2;
        distance_m: number;
        eta_ms: number;
        replaces: number | null;
      }
    | { kind: "arrived"; device_id: string; command_id: number; x: number; y: number; at: string | null }
    | {
        kind: "stopped";
        device_id: string;
        command_id: number;
        x: number;
        y: number;
        at: string | null;
        origin: Origin;
      }
    | {
        kind: "undock" | "dock";
        device_id: string;
        event: "undock" | "dock";
        target: string;
        command_id: number;
        x: number;
        y: number;
      }
    | {
        kind: "sensor";
        sensor: string;
        device_id: string;
        type: string;
        value: number;
        unit: string;
        x: number;
        y: number;
      }
    | {
        kind: "sensor_set";
        sensor: string;
        device_id: string;
        type: string;
        setpoint: number;
        noise_std: number;
        previous_setpoint: number;
        paused: boolean;
        origin: Origin;
      }
    | {
        kind: "action";
        device_id: string;
        /** The action's id, e.g. "pick". */
        event: string;
        object: string | null;
        command_id: number;
        x: number;
        y: number;
        at: string | null;
        origin: Origin;
      }
    | { kind: "sim"; state: "started" | "paused" | "resumed" | "reset"; scenario: string }
    | {
        kind: "scene";
        scene: string;
        state: "started" | "restarted" | "finished" | "stopped" | "failed";
        iteration: number;
        error: string | null;
      }
  );

export type EventKind = SimEvent["kind"];

export const EVENT_KINDS: EventKind[] = [
  "position",
  "move_command",
  "arrived",
  "stopped",
  "undock",
  "dock",
  "sensor",
  "sensor_set",
  "action",
  "scene",
  "sim",
];

// --- Commands (command.rs) ---------------------------------------------------------------------

export type Target = { point: Vec2 } | { machine: string };

export type SimCommand =
  | { type: "move_to"; mm: string; target: Target }
  | { type: "stop"; mm: string }
  | { type: "set_sensor"; sensor: string; value: number }
  | { type: "set_sensor_noise"; sensor: string; std: number }
  | { type: "set_sensor_paused"; sensor: string; paused: boolean }
  | { type: "perform"; mm: string; action: string; object?: string };

export type Control = "pause" | "resume" | "reset" | "stop_scene" | { play_scene: string };
