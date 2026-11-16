// The Rust types the app receives, generated from core/src by ts-rs into ./bindings (`cargo test`
// regenerates them; don't edit those files). Below them, the few types only the app has.

import type { SimEvent } from "./bindings/SimEvent";

export type { ActionAvailability } from "./bindings/ActionAvailability";
export type { ActionDef } from "./bindings/ActionDef";
export type { Control } from "./bindings/Control";
export type { HostKind } from "./bindings/HostKind";
export type { MachineDef } from "./bindings/MachineDef";
export type { MmDef } from "./bindings/MmDef";
export type { MmView } from "./bindings/MmView";
export type { MotionView } from "./bindings/MotionView";
export type { MqttTopic } from "./bindings/MqttTopic";
export type { Origin } from "./bindings/Origin";
export type { Place } from "./bindings/Place";
export type { Repeat } from "./bindings/Repeat";
export type { ReplayFormat } from "./bindings/ReplayFormat";
export type { ReplaySettings } from "./bindings/ReplaySettings";
export type { ReplayStatus } from "./bindings/ReplayStatus";
export type { Scenario } from "./bindings/Scenario";
export type { SceneDef } from "./bindings/SceneDef";
export type { SceneStatus } from "./bindings/SceneStatus";
export type { SceneStep } from "./bindings/SceneStep";
export type { SensorDef } from "./bindings/SensorDef";
export type { SensorView } from "./bindings/SensorView";
export type { SimCommand } from "./bindings/SimCommand";
export type { SimEvent } from "./bindings/SimEvent";
export type { SinkDef } from "./bindings/SinkDef";
export type { Target } from "./bindings/Target";
export type { TrackStatus } from "./bindings/TrackStatus";
export type { Vec2 } from "./bindings/Vec2";
export type { WorldState } from "./bindings/WorldState";

export type EventKind = SimEvent["kind"];

/**
 * One row of the log (bridge.rs: LogEntry): `data` is exactly what the sinks wrote, a SimEvent or a
 * replayed trace payload; `kind` and `device_id` are what the log (and MQTT topics) go by.
 */
export interface LogEntry {
  seq: number;
  sim_ms: number;
  kind: EventKind | null;
  device_id: string | null;
  data: Record<string, unknown>;
}
