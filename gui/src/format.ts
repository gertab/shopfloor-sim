import type { EventKind, LogEntry, Repeat, Scenario } from "./types";

export const KIND_LABELS: Record<EventKind, string> = {
  position: "position",
  move_command: "move",
  arrived: "arrived",
  stopped: "stopped",
  undock: "undock",
  dock: "dock",
  sensor: "sensor",
  sensor_set: "sensor set",
  action: "action",
  scene: "scene",
  sim: "sim",
};

/** Accent colour per mobile manipulator (by index in the scenario). */
const MM_COLORS = ["#53c7b6", "#f2b84b", "#6aa9ff", "#ff8fc7", "#b48cff", "#9ad86b"];

export function mmColor(index: number): string {
  return MM_COLORS[index % MM_COLORS.length];
}

export function repeatLabel(repeat: Repeat): string {
  if (repeat === "once") return "once";
  if (repeat === "forever") return "loop";
  return `${repeat.times}×`;
}

/** Compact number formatting for sensor values and coordinates. */
export function formatValue(value: number | null | undefined): string {
  if (value === null || value === undefined || Number.isNaN(value)) {
    return "–";
  }
  const magnitude = Math.abs(value);
  if (magnitude >= 100) return value.toFixed(0);
  if (magnitude >= 10) return value.toFixed(1);
  return value.toFixed(2);
}

export function formatPoint(x: number, y: number): string {
  return `(${x.toFixed(1)}, ${y.toFixed(1)})`;
}

/** mm:ss.s */
export function formatSimTime(simMs: number): string {
  const totalSeconds = simMs / 1000;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds - minutes * 60;
  return `${String(minutes).padStart(2, "0")}:${seconds.toFixed(1).padStart(4, "0")}`;
}

export function machineLabel(scenario: Scenario | null, id: string | null): string | null {
  if (!id) return null;
  return scenario?.machines.find((machine) => machine.id === id)?.label ?? id;
}

type Data = Record<string, unknown>;

const num = (data: Data, key: string): number | null => (typeof data[key] === "number" ? (data[key] as number) : null);
const str = (data: Data, key: string): string | null => (typeof data[key] === "string" ? (data[key] as string) : null);

function pointOf(value: unknown): { x: number; y: number } | null {
  const point = value as { x?: unknown; y?: unknown } | null;
  return point && typeof point.x === "number" && typeof point.y === "number" ? { x: point.x, y: point.y } : null;
}

/** "Bonder (2.5, 4.8)", or just the machine or the point when only one is known. */
function describePlace(scenario: Scenario | null, at: string | null, point: { x: number; y: number } | null): string {
  const label = machineLabel(scenario, at);
  const coords = point ? formatPoint(point.x, point.y) : null;
  return [label, coords].filter(Boolean).join(" ") || "?";
}

/**
 * One-line human summary of a log entry. It reads `data` loosely, since it may be a simulator event,
 * a replayed trace payload (fewer keys) or one in an envelope.
 */
export function describeEntry(entry: LogEntry, scenario: Scenario | null): string {
  const data = entry.data;
  const commandId = num(data, "command_id");
  const ref = commandId !== null ? `#${commandId} ` : "";
  const origin = str(data, "origin");
  const by = origin ? `  [${origin}]` : "";
  const here = pointOf(data);
  switch (entry.kind) {
    case "position": {
      const heading = num(data, "heading_rad") ?? 0;
      return `${describePlace(scenario, null, here)}  heading ${((heading * 180) / Math.PI).toFixed(0)}°  ${(num(data, "speed_mps") ?? 0).toFixed(2)} m/s`;
    }
    case "move_command": {
      const distance = num(data, "distance_m");
      const eta = num(data, "eta_ms");
      const replaces = num(data, "replaces");
      return (
        `${ref}${describePlace(scenario, str(data, "source"), pointOf(data.from))} -> ` +
        describePlace(scenario, str(data, "destination"), pointOf(data.to)) +
        (distance !== null && eta !== null ? `  ${distance.toFixed(1)} m, ETA ${(eta / 1000).toFixed(1)} s` : "") +
        (replaces !== null ? `  (replaces #${replaces})` : "") +
        by
      );
    }
    case "arrived":
      return `${ref}at ${describePlace(scenario, str(data, "at"), here)}`;
    case "stopped":
      return `${ref}stopped at ${describePlace(scenario, str(data, "at"), here)}${by}`;
    case "undock":
      return `${ref}left ${describePlace(scenario, str(data, "target"), here)}`;
    case "dock":
      return `${ref}docked at ${describePlace(scenario, str(data, "target"), here)}`;
    case "sensor": {
      const name = str(data, "sensor") ?? `${entry.device_id ?? "?"} ${str(data, "type") ?? ""}`;
      const target = str(data, "target");
      return (
        `${name} = ${formatValue(num(data, "value"))} ${str(data, "unit") ?? ""}` +
        (here ? `  @ ${formatPoint(here.x, here.y)}` : "") +
        (target ? `  -> ${machineLabel(scenario, target)}` : "")
      );
    }
    case "sensor_set":
      return `${str(data, "sensor")} setpoint ${formatValue(num(data, "previous_setpoint"))} -> ${formatValue(num(data, "setpoint"))}, noise σ ${formatValue(num(data, "noise_std"))}${data.paused ? ", paused" : ""}${by}`;
    case "action": {
      const event = str(data, "event") ?? "?";
      const object = str(data, "object");
      // Replayed dock/undock payloads name their machine as `target`.
      const at = str(data, "at") ?? str(data, "target");
      const preposition = event === "undock" ? "from" : "at";
      return `${ref}${event}${object ? ` ${object}` : ""}${at || here ? ` ${preposition} ${describePlace(scenario, at, here)}` : ""}${by}`;
    }
    case "scene": {
      const error = str(data, "error");
      return `${str(data, "scene")} ${str(data, "state")} (iteration ${num(data, "iteration")})${error ? `: ${error}` : ""}`;
    }
    case "sim":
      return `${str(data, "state")} · ${str(data, "scenario")}`;
    default:
      return JSON.stringify(data);
  }
}
