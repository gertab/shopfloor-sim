import type { MmDef, Vec2 } from "./types";

/**
 * Keeps a target point where the MM's centre can actually go (floor minus its radius), rounded to
 * the centimetre.
 */
export function clampToReach(point: Vec2, mm: MmDef, width: number, height: number): Vec2 {
  const r = mm.radius_m;
  const clamp = (value: number, max: number) => Math.min(Math.max(Math.round(value * 100) / 100, r), max - r);
  return { x: clamp(point.x, width), y: clamp(point.y, height) };
}

/**
 * How much to scale the floor's text and markers. They are sized for a 20×12 m floor; scaling them
 * with the floor keeps them the same size on screen, whether the floor is a lab bench or a port.
 */
export function floorScale(width: number, height: number): number {
  return Math.max(width / 20, height / 12);
}

/** Spacing of the axis labels: the smallest 1, 2 or 5 × 10ⁿ that gives at most 8 intervals. */
export function axisStep(length: number): number {
  const magnitude = 10 ** Math.floor(Math.log10(length / 8));
  return [1, 2, 5].map((nice) => nice * magnitude).find((step) => length / step <= 8) ?? 10 * magnitude;
}
