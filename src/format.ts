import type { EventKind, Repeat, Scenario, SimEvent } from "./types";

export const KIND_LABELS: Record<EventKind, string> = {
  position: "position",
  move_command: "move",
  arrived: "arrived",
  stopped: "stopped",
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

function describePlace(scenario: Scenario | null, place: { x: number; y: number; at: string | null }) {
  const label = machineLabel(scenario, place.at);
  return label ? `${label} ${formatPoint(place.x, place.y)}` : formatPoint(place.x, place.y);
}

/** The entity an event is about (for filtering). */
export function eventEntity(event: SimEvent): string | null {
  return event.kind === "sim" || event.kind === "scene" ? null : event.entity;
}

/** One-line human summary of an event. */
export function describeEvent(event: SimEvent, scenario: Scenario | null): string {
  switch (event.kind) {
    case "position":
      return `${formatPoint(event.x, event.y)}  heading ${((event.heading_rad * 180) / Math.PI).toFixed(0)}°  ${event.speed_mps.toFixed(2)} m/s`;
    case "move_command":
      return (
        `#${event.command_id} ${describePlace(scenario, event.from)} → ${describePlace(scenario, event.to)}` +
        `  ${event.distance_m.toFixed(1)} m, ETA ${(event.eta_ms / 1000).toFixed(1)} s` +
        (event.replaces !== null ? `  (replaces #${event.replaces})` : "") +
        `  [${event.origin}]`
      );
    case "arrived":
      return `#${event.command_id} at ${describePlace(scenario, event)}`;
    case "stopped":
      return `#${event.command_id} stopped at ${describePlace(scenario, event)}  [${event.origin}]`;
    case "sensor":
      return `${event.sensor} = ${formatValue(event.value)} ${event.unit}  @ ${formatPoint(event.x, event.y)}`;
    case "sensor_set":
      return `${event.sensor} setpoint ${formatValue(event.previous_setpoint)} → ${formatValue(event.setpoint)}, noise σ ${formatValue(event.noise_std)}${event.paused ? ", paused" : ""}  [${event.origin}]`;
    case "action":
      return `#${event.command_id} ${event.action} at ${describePlace(scenario, event)}  [${event.origin}]`;
    case "scene":
      return `${event.scene} ${event.state} (iteration ${event.iteration})${event.error ? `: ${event.error}` : ""}`;
    case "sim":
      return `${event.state} · ${event.scenario}`;
  }
}
