import { useMemo, useRef, useState } from "react";
import type { CSSProperties, MouseEvent } from "react";
import { formatValue, mmColor } from "../../format";
import { axisStep, clampToReach, floorScale } from "../../geometry";
import type { ActionEvent } from "../../hooks/useSimulation";
import type { MachineDef, MmDef, MmView, Scenario, SensorDef, SensorView, Vec2, WorldState } from "../../types";

/*
 * Text and markers are sized in metres for a 20×12 m floor, and multiplied by `scale` (`floorScale`)
 * so they keep their size on screen on any floor. The stylesheet does the same with `--floor-scale`.
 * Real things (machine footprints, MM radii) are drawn at their real size.
 */
/** Empty space drawn around the floor. */
const MARGIN_M = 0.9;
const LABEL_SIZE = 0.38;
const READOUT_SIZE = 0.28;
const FLOOR_SENSOR_LABEL_SIZE = 0.28;

/*
 * SVG text is never hit-tested here (`.floor-svg text { pointer-events: none }`). WebKit, the macOS
 * webview, puts the hit box of tiny viewBox-scaled text in the wrong place: some readings were not
 * clickable at all and others stole clicks from the empty floor next to them. Clickable text gets
 * an invisible `.hit-area` rect sized from the text instead.
 */
/** Advance of one character of the monospace readout font, in ems. */
const MONO_CHAR_EM = 0.6;
/** Rough average character width of the proportional label font, in ems. */
const SANS_CHAR_EM = 0.55;
const HIT_PAD = 0.08;

export type Focus = { type: "machine" | "sensor"; id: string } | null;

interface FloorViewProps {
  scenario: Scenario;
  world: WorldState | null;
  selectedMm: string | null;
  focus: Focus;
  lastActions: Record<string, ActionEvent>;
  onSelectMm: (id: string) => void;
  onFloorClick: (point: Vec2 | null) => void;
  onMachineClick: (id: string) => void;
  onSensorClick: (id: string) => void;
}

/**
 * The shopfloor, drawn in floor coordinates (metres). The SVG's y axis points down, so every y is
 * flipped with `sy()`; the floor model has its origin bottom-left with y up.
 */
export function FloorView({
  scenario,
  world,
  selectedMm,
  focus,
  lastActions,
  onSelectMm,
  onFloorClick,
  onMachineClick,
  onSensorClick,
}: FloorViewProps) {
  const svgRef = useRef<SVGSVGElement>(null);
  const [hover, setHover] = useState<Vec2 | null>(null);
  const { width_m: width, height_m: height, grid_m: grid } = scenario.floor;
  const sy = (y: number) => height - y;
  const scale = floorScale(width, height);
  const margin = MARGIN_M * scale;

  const sensorDefs = useMemo(
    () => new Map(scenario.sensors.map((sensor) => [sensor.id, sensor])),
    [scenario.sensors],
  );
  const sensorViews = useMemo(
    () => new Map((world?.sensors ?? []).map((sensor) => [sensor.id, sensor])),
    [world?.sensors],
  );
  const mmViews = useMemo(
    () => new Map((world?.mobile_manipulators ?? []).map((mm) => [mm.id, mm])),
    [world?.mobile_manipulators],
  );
  const selectedDef = scenario.mobile_manipulators.find((mm) => mm.id === selectedMm) ?? null;

  /** Converts a mouse position to floor coordinates, or null when outside the floor. */
  const toFloor = (event: MouseEvent<SVGElement>): Vec2 | null => {
    const svg = svgRef.current;
    const matrix = svg?.getScreenCTM();
    if (!svg || !matrix) return null;
    const point = new DOMPoint(event.clientX, event.clientY).matrixTransform(matrix.inverse());
    const floorPoint = { x: point.x, y: height - point.y };
    const inside =
      floorPoint.x >= 0 && floorPoint.x <= width && floorPoint.y >= 0 && floorPoint.y <= height;
    return inside ? floorPoint : null;
  };

  const gridLines = useMemo(() => {
    const lines: JSX.Element[] = [];
    for (let x = grid; x < width - 1e-9; x += grid) {
      lines.push(<line key={`x${x}`} x1={x} y1={0} x2={x} y2={height} />);
    }
    for (let y = grid; y < height - 1e-9; y += grid) {
      lines.push(<line key={`y${y}`} x1={0} y1={height - y} x2={width} y2={height - y} />);
    }
    return lines;
  }, [width, height, grid]);

  const axisTicks = useMemo(() => {
    const step = axisStep(width);
    const round = (value: number) => Number(value.toFixed(3));
    const ticks: JSX.Element[] = [];
    for (let i = 0; i * step <= width + 1e-9; i++) {
      const x = round(i * step);
      ticks.push(
        <text key={`tx${x}`} x={x} y={height + 0.55 * scale} className="axis-label" textAnchor="middle">
          {x}
        </text>,
      );
    }
    for (let i = 1; i * step <= height + 1e-9; i++) {
      const y = round(i * step);
      ticks.push(
        <text key={`ty${y}`} x={-0.25 * scale} y={height - y + 0.1 * scale} className="axis-label" textAnchor="end">
          {y}
        </text>,
      );
    }
    return ticks;
  }, [width, height, scale]);

  const ghost = selectedDef && hover ? clampToReach(hover, selectedDef, width, height) : null;

  return (
    <svg
      ref={svgRef}
      className={`floor-svg${selectedMm ? " is-targeting" : ""}`}
      style={{ "--floor-scale": scale } as CSSProperties}
      viewBox={`${-margin} ${-margin} ${width + 2 * margin} ${height + 2 * margin}`}
      preserveAspectRatio="xMidYMid meet"
      onClick={(event) => onFloorClick(toFloor(event))}
      onMouseMove={(event) => setHover(selectedMm ? toFloor(event) : null)}
      onMouseLeave={() => setHover(null)}
    >
      <rect className="floor" x={0} y={0} width={width} height={height} />
      <g className="floor-grid">{gridLines}</g>
      <g>{axisTicks}</g>

      {scenario.machines.map((machine) => (
        <Machine
          key={machine.id}
          machine={machine}
          sy={sy}
          scale={scale}
          focused={focus?.type === "machine" && focus.id === machine.id}
          targetable={selectedMm !== null}
          sensors={machine.sensors}
          sensorDefs={sensorDefs}
          sensorViews={sensorViews}
          focusedSensor={focus?.type === "sensor" ? focus.id : null}
          onClick={() => onMachineClick(machine.id)}
          onSensorClick={onSensorClick}
        />
      ))}

      {scenario.sensors
        .filter((sensor) => sensor.position !== null)
        .map((sensor) => (
          <FloorSensor
            key={sensor.id}
            def={sensor}
            view={sensorViews.get(sensor.id)}
            sy={sy}
            scale={scale}
            focused={focus?.type === "sensor" && focus.id === sensor.id}
            onClick={() => onSensorClick(sensor.id)}
          />
        ))}

      {scenario.mobile_manipulators.map((def, index) => {
        const view = mmViews.get(def.id);
        return view ? (
          <MobileManipulator
            key={def.id}
            def={def}
            view={view}
            color={mmColor(index)}
            sy={sy}
            scale={scale}
            selected={def.id === selectedMm}
            sensorDefs={sensorDefs}
            sensorViews={sensorViews}
            focusedSensor={focus?.type === "sensor" ? focus.id : null}
            onClick={() => onSelectMm(def.id)}
            onSensorClick={onSensorClick}
          />
        ) : null;
      })}

      {/*
        Not inside the MM's group: in order to avoid issues in WebKit
      */}
      {scenario.mobile_manipulators.map((def) => {
        const view = mmViews.get(def.id);
        const action = lastActions[def.id];
        return view && action ? (
          // Keyed by command id so the fade-out animation restarts for every new action.
          <text
            key={action.command_id}
            x={view.x}
            y={sy(view.y) - def.radius_m - 0.25 * scale}
            className="action-flash"
            textAnchor="middle"
          >
            {action.action}
          </text>
        ) : null;
      })}

      {ghost && selectedDef && (
        <circle
          className="ghost-target"
          style={{ color: mmColor(scenario.mobile_manipulators.indexOf(selectedDef)) }}
          cx={ghost.x}
          cy={sy(ghost.y)}
          r={selectedDef.radius_m}
        />
      )}
    </svg>
  );
}

interface SensorReadoutsProps {
  ids: string[];
  x: number;
  y: number;
  scale: number;
  sensorDefs: Map<string, SensorDef>;
  sensorViews: Map<string, SensorView>;
  focusedSensor: string | null;
  onSensorClick: (id: string) => void;
}

/** Lines of "name value unit", starting at (x, y) in SVG coordinates and going down. */
function SensorReadouts({ ids, x, y, scale, sensorDefs, sensorViews, focusedSensor, onSensorClick }: SensorReadoutsProps) {
  return (
    <>
      {ids.map((id, index) => {
        const def = sensorDefs.get(id);
        if (!def) return null;
        const view = sensorViews.get(id);
        const text = view?.paused
          ? `${def.name} paused`
          : `${def.name} ${formatValue(view?.last_value ?? view?.setpoint)} ${def.unit}`;
        const baseline = y + index * (READOUT_SIZE + 0.08) * scale;
        const width = (text.length * MONO_CHAR_EM * READOUT_SIZE + 2 * HIT_PAD) * scale;
        return (
          <g
            key={id}
            className={`sensor-readout-line${focusedSensor === id ? " is-focused" : ""}`}
            onClick={(event) => {
              event.stopPropagation();
              onSensorClick(id);
            }}
          >
            <rect
              className="hit-area"
              x={x - width / 2}
              y={baseline - (READOUT_SIZE * 0.9 + 0.02) * scale}
              width={width}
              height={(READOUT_SIZE * 1.15 + 0.04) * scale}
            />
            <text
              x={x}
              y={baseline}
              className={`sensor-readout${view?.paused ? " is-paused" : ""}`}
              textAnchor="middle"
            >
              {text}
            </text>
          </g>
        );
      })}
    </>
  );
}

interface MachineProps {
  machine: MachineDef;
  sy: (y: number) => number;
  scale: number;
  focused: boolean;
  targetable: boolean;
  sensors: string[];
  sensorDefs: Map<string, SensorDef>;
  sensorViews: Map<string, SensorView>;
  focusedSensor: string | null;
  onClick: () => void;
  onSensorClick: (id: string) => void;
}

function Machine({
  machine,
  sy,
  scale,
  focused,
  targetable,
  sensors,
  sensorDefs,
  sensorViews,
  focusedSensor,
  onClick,
  onSensorClick,
}: MachineProps) {
  const { position, size, dock } = machine;
  const left = position.x - size.x / 2;
  const top = sy(position.y + size.y / 2);
  return (
    <g
      className={`machine${focused ? " is-focused" : ""}${targetable ? " is-targetable" : ""}`}
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
    >
      <title>{targetable ? `Send the selected MM to ${machine.label}` : machine.label}</title>
      <line className="dock-link" x1={position.x} y1={sy(position.y)} x2={dock.x} y2={sy(dock.y)} />
      <rect className="machine-body" x={left} y={top} width={size.x} height={size.y} rx={0.12 * scale} />
      <text x={position.x} y={top + (LABEL_SIZE + 0.15) * scale} className="machine-label" textAnchor="middle">
        {machine.label}
      </text>
      <SensorReadouts
        ids={sensors}
        x={position.x}
        y={top + (LABEL_SIZE + 0.2 + READOUT_SIZE + 0.1) * scale}
        scale={scale}
        sensorDefs={sensorDefs}
        sensorViews={sensorViews}
        focusedSensor={focusedSensor}
        onSensorClick={onSensorClick}
      />
      <circle className="dock-marker" cx={dock.x} cy={sy(dock.y)} r={0.14 * scale} />
    </g>
  );
}

interface FloorSensorProps {
  def: SensorDef;
  view: SensorView | undefined;
  sy: (y: number) => number;
  scale: number;
  focused: boolean;
  onClick: () => void;
}

/** A free-standing sensor: a diamond marker with its label above and live value below. */
function FloorSensor({ def, view, sy, scale, focused, onClick }: FloorSensorProps) {
  if (!def.position) return null;
  const cx = def.position.x;
  const cy = sy(def.position.y);
  const half = 0.2 * scale;
  const labelBaseline = cy - half - 0.12 * scale;
  const valueBaseline = cy + half + (READOUT_SIZE + 0.05) * scale;
  const valueText = view?.paused ? "paused" : `${formatValue(view?.last_value ?? view?.setpoint)} ${def.unit}`;
  // One click target covering the label, the diamond and the value.
  const hitWidth =
    Math.max(
      def.label.length * SANS_CHAR_EM * FLOOR_SENSOR_LABEL_SIZE * scale,
      valueText.length * MONO_CHAR_EM * READOUT_SIZE * scale,
      2 * half,
    ) +
    2 * HIT_PAD * scale;
  const hitTop = labelBaseline - (FLOOR_SENSOR_LABEL_SIZE * 0.9 + HIT_PAD / 2) * scale;
  const hitBottom = valueBaseline + (READOUT_SIZE * 0.25 + HIT_PAD / 2) * scale;
  return (
    <g
      className={`floor-sensor${focused ? " is-focused" : ""}`}
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
    >
      <title>{`${def.label} (${def.id}) at (${cx}, ${def.position.y})`}</title>
      <rect className="hit-area" x={cx - hitWidth / 2} y={hitTop} width={hitWidth} height={hitBottom - hitTop} />
      <path
        className="floor-sensor-marker"
        d={`M ${cx} ${cy - half} L ${cx + half} ${cy} L ${cx} ${cy + half} L ${cx - half} ${cy} Z`}
      />
      <text x={cx} y={labelBaseline} className="floor-sensor-label" textAnchor="middle">
        {def.label}
      </text>
      <text
        x={cx}
        y={valueBaseline}
        className={`sensor-readout${view?.paused ? " is-paused" : ""}`}
        textAnchor="middle"
      >
        {valueText}
      </text>
    </g>
  );
}

interface MobileManipulatorProps {
  def: MmDef;
  view: MmView;
  color: string;
  sy: (y: number) => number;
  scale: number;
  selected: boolean;
  sensorDefs: Map<string, SensorDef>;
  sensorViews: Map<string, SensorView>;
  focusedSensor: string | null;
  onClick: () => void;
  onSensorClick: (id: string) => void;
}

function MobileManipulator({
  def,
  view,
  color,
  sy,
  scale,
  selected,
  sensorDefs,
  sensorViews,
  focusedSensor,
  onClick,
  onSensorClick,
}: MobileManipulatorProps) {
  const r = def.radius_m;
  const cx = view.x;
  const cy = sy(view.y);
  // SVG rotation is clockwise with y down, so negate the (counter-clockwise) heading.
  const headingDeg = (-view.heading_rad * 180) / Math.PI;
  const target = view.motion?.target;
  const cross = 0.2 * scale;
  return (
    <g className={`mm${selected ? " is-selected" : ""}`} style={{ color }}>
      {target && (
        <g className="mm-route">
          <line x1={cx} y1={cy} x2={target.x} y2={sy(target.y)} />
          <path
            d={`M ${target.x - cross} ${sy(target.y) - cross} L ${target.x + cross} ${sy(target.y) + cross} M ${target.x - cross} ${sy(target.y) + cross} L ${target.x + cross} ${sy(target.y) - cross}`}
          />
        </g>
      )}
      <g
        className="mm-body"
        onClick={(event) => {
          event.stopPropagation();
          onClick();
        }}
      >
        <title>{`${def.label} (${def.id}) — click to select`}</title>
        {selected && <circle className="mm-selection" cx={cx} cy={cy} r={r + 0.18 * scale} />}
        <circle className="mm-base" cx={cx} cy={cy} r={r} />
        <g transform={`translate(${cx} ${cy}) rotate(${headingDeg})`}>
          <path className="mm-heading" d={`M ${r * 0.15} ${-r * 0.45} L ${r * 0.95} 0 L ${r * 0.15} ${r * 0.45} Z`} />
        </g>
      </g>
      <text x={cx} y={cy + r + (LABEL_SIZE + 0.05) * scale} className="mm-label" textAnchor="middle">
        {def.label}
      </text>
      <SensorReadouts
        ids={def.sensors}
        x={cx}
        y={cy + r + (LABEL_SIZE + 0.15 + READOUT_SIZE) * scale}
        scale={scale}
        sensorDefs={sensorDefs}
        sensorViews={sensorViews}
        focusedSensor={focusedSensor}
        onSensorClick={onSensorClick}
      />
    </g>
  );
}
