import { Hand, Navigation, Pause, Play, RotateCcw, Square } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type React from "react";
import { formatPoint, formatSimTime, formatValue, machineLabel, mmColor } from "../../format";
import type { ActionEvent } from "../../hooks/useSimulation";
import { ScenesPanel } from "../scenes/ScenesPanel";
import type { Control, MmDef, MmView, Scenario, SensorDef, SensorView, SimCommand, WorldState } from "../../types";
import type { Focus } from "../floor/FloorView";

/** Minimum time between setpoint commands while a slider is dragged. */
const SLIDER_THROTTLE_MS = 100;

interface ControlPanelProps {
  scenario: Scenario;
  world: WorldState | null;
  selectedMm: string | null;
  focus: Focus;
  lastActions: Record<string, ActionEvent>;
  onSelectMm: (id: string) => void;
  onFocusSensor: (id: string) => void;
  send: (command: SimCommand) => void;
  onControl: (control: Control) => void;
}

export function ControlPanel({
  scenario,
  world,
  selectedMm,
  focus,
  lastActions,
  onSelectMm,
  onFocusSensor,
  send,
  onControl,
}: ControlPanelProps) {
  const mmViews = new Map((world?.mobile_manipulators ?? []).map((mm) => [mm.id, mm]));
  const sensorViews = new Map((world?.sensors ?? []).map((sensor) => [sensor.id, sensor]));
  const sensorsByHost = groupSensorsByHost(scenario);

  return (
    <aside className="controls">
      <ScenesPanel scenario={scenario} world={world} onControl={onControl} />

      <section className="panel-section">
        <h2>Mobile manipulators</h2>
        {scenario.mobile_manipulators.map((def, index) => (
          <MmCard
            key={def.id}
            def={def}
            view={mmViews.get(def.id)}
            color={mmColor(index)}
            scenario={scenario}
            selected={def.id === selectedMm}
            lastAction={lastActions[def.id]}
            onSelect={() => onSelectMm(def.id)}
            send={send}
          />
        ))}
        <p className="hint">
          Select an MM, then click a machine or any point on the floor to send it there. Esc
          deselects. Actions only produce an event; greyed-out ones explain why on hover.
        </p>
      </section>

      <section className="panel-section">
        <h2>Sensors</h2>
        {sensorsByHost.map(({ host, label, sensors }) => (
          <div key={host} className="sensor-group">
            <h3>{label}</h3>
            {sensors.map((def) => (
              <SensorControl
                key={def.id}
                def={def}
                view={sensorViews.get(def.id)}
                focused={focus?.type === "sensor" && focus.id === def.id}
                highlighted={focus?.type === "machine" && focus.id === host}
                onFocus={() => onFocusSensor(def.id)}
                send={send}
              />
            ))}
          </div>
        ))}
        {scenario.sensors.length === 0 && <p className="hint">This scenario has no sensors.</p>}
      </section>
    </aside>
  );
}

/** MMs first, then machines, then free-standing sensors, in scenario order. */
function groupSensorsByHost(scenario: Scenario) {
  const hosts = [
    ...scenario.mobile_manipulators.map((mm) => ({ host: mm.id, label: mm.label, ids: mm.sensors })),
    ...scenario.machines.map((machine) => ({ host: machine.id, label: machine.label, ids: machine.sensors })),
    {
      host: "floor",
      label: "Floor",
      ids: scenario.sensors.filter((sensor) => sensor.host_kind === "floor").map((sensor) => sensor.id),
    },
  ];
  const defs = new Map(scenario.sensors.map((sensor) => [sensor.id, sensor]));
  return hosts
    .map(({ host, label, ids }) => ({
      host,
      label,
      sensors: ids.map((id) => defs.get(id)).filter((def): def is SensorDef => def !== undefined),
    }))
    .filter((group) => group.sensors.length > 0);
}

interface MmCardProps {
  def: MmDef;
  view: MmView | undefined;
  color: string;
  scenario: Scenario;
  selected: boolean;
  lastAction: ActionEvent | undefined;
  onSelect: () => void;
  send: (command: SimCommand) => void;
}

function MmCard({ def, view, color, scenario, selected, lastAction, onSelect, send }: MmCardProps) {
  const motion = view?.motion ?? null;
  let status = "–";
  if (view && motion) {
    const destination =
      machineLabel(scenario, motion.target.at) ?? formatPoint(motion.target.x, motion.target.y);
    status = `→ ${destination} · ${motion.remaining_m.toFixed(1)} m · ${(motion.eta_ms / 1000).toFixed(1)} s`;
  } else if (view) {
    status = `Idle at ${machineLabel(scenario, view.at) ?? formatPoint(view.x, view.y)}`;
  }

  return (
    <div
      className={`mm-card${selected ? " is-selected" : ""}`}
      style={{ borderLeftColor: color }}
      onClick={onSelect}
    >
      <div className="mm-card-header">
        <span className="mm-dot" style={{ background: color }} />
        <strong>{def.label}</strong>
        <span className="muted">{def.id}</span>
        <span className={`mm-state${motion ? " is-moving" : ""}`}>{motion ? "moving" : "idle"}</span>
      </div>
      <div className="mm-status">{status}</div>
      {view && (
        <div className="muted small">
          {formatPoint(view.x, view.y)} · {def.max_speed_mps} m/s max
        </div>
      )}
      <div className="mm-actions" onClick={(event) => event.stopPropagation()}>
        {scenario.machines.map((machine) => (
          <button
            key={machine.id}
            type="button"
            className="chip-button"
            disabled={view?.at === machine.id && !motion}
            title={`Send ${def.label} to ${machine.label}`}
            onClick={() => send({ type: "move_to", mm: def.id, target: { machine: machine.id } })}
          >
            <Navigation size={12} />
            {machine.label}
          </button>
        ))}
        <button
          type="button"
          className="chip-button danger"
          disabled={!motion}
          title={`Stop ${def.label}`}
          onClick={() => send({ type: "stop", mm: def.id })}
        >
          <Square size={11} />
          Stop
        </button>
      </div>
      {view && view.actions.length > 0 && (
        <div className="mm-actions" onClick={(event) => event.stopPropagation()}>
          {view.actions.map((availability) => {
            const label = scenario.actions.find((action) => action.id === availability.id)?.label ?? availability.id;
            return (
              <button
                key={availability.id}
                type="button"
                className="chip-button action"
                disabled={!availability.available}
                title={availability.reason ?? `${def.label}: ${label}`}
                onClick={() => send({ type: "perform", mm: def.id, action: availability.id })}
              >
                <Hand size={12} />
                {label}
              </button>
            );
          })}
        </div>
      )}
      {lastAction && (
        <div className="muted small last-action">
          last action: {lastAction.action}
          {lastAction.at ? ` at ${machineLabel(scenario, lastAction.at)}` : ` at ${formatPoint(lastAction.x, lastAction.y)}`} ·{" "}
          {formatSimTime(lastAction.sim_ms)}
        </div>
      )}
    </div>
  );
}

interface SensorControlProps {
  def: SensorDef;
  view: SensorView | undefined;
  focused: boolean;
  highlighted: boolean;
  onFocus: () => void;
  send: (command: SimCommand) => void;
}

function SensorControl({ def, view, focused, highlighted, onFocus, send }: SensorControlProps) {
  const rowRef = useRef<HTMLDivElement>(null);
  const setpoint = view?.setpoint ?? def.default;
  const noise = view?.noise_std ?? def.noise_std;
  const paused = view?.paused ?? false;

  // While dragging, the slider shows a local draft that is sent at most every SLIDER_THROTTLE_MS
  // and dropped shortly after the last send (by then the simulation reports it back).
  const [draft, setDraft] = useState<number | null>(null);
  const draftRef = useRef<number | null>(null);
  const timerRef = useRef<number | null>(null);
  // The number fields are edited as text and committed on Enter / blur.
  const [valueText, setValueText] = useState<string | null>(null);
  const [noiseText, setNoiseText] = useState<string | null>(null);

  useEffect(() => {
    if (focused) {
      rowRef.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    }
  }, [focused]);

  useEffect(
    () => () => {
      if (timerRef.current !== null) window.clearTimeout(timerRef.current);
    },
    [],
  );

  const clamp = (value: number) => Math.min(Math.max(value, def.min), def.max);

  const sendSetpoint = (value: number) => {
    send({ type: "set_sensor", sensor: def.id, value: clamp(value) });
  };

  const onSlider = (value: number) => {
    setDraft(value);
    draftRef.current = value;
    if (timerRef.current === null) {
      timerRef.current = window.setTimeout(() => {
        timerRef.current = null;
        if (draftRef.current !== null) sendSetpoint(draftRef.current);
        window.setTimeout(() => {
          if (timerRef.current === null) setDraft(null);
        }, 400);
      }, SLIDER_THROTTLE_MS);
    }
  };

  const commitValue = () => {
    const value = Number(valueText);
    if (valueText !== null && valueText.trim() !== "" && Number.isFinite(value) && value !== setpoint) {
      sendSetpoint(value);
    }
    setValueText(null);
  };

  const commitNoise = () => {
    const value = Number(noiseText);
    if (noiseText !== null && noiseText.trim() !== "" && Number.isFinite(value) && value >= 0 && value !== noise) {
      send({ type: "set_sensor_noise", sensor: def.id, std: value });
    }
    setNoiseText(null);
  };

  const blurOnEnter = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter") event.currentTarget.blur();
    if (event.key === "Escape") {
      setValueText(null);
      setNoiseText(null);
      event.currentTarget.blur();
    }
  };

  const shown = draft ?? setpoint;
  const step = niceStep(def.max - def.min);

  return (
    <div
      ref={rowRef}
      className={`sensor-control${focused ? " is-focused" : ""}${highlighted ? " is-highlighted" : ""}`}
      onClick={onFocus}
    >
      <div className="sensor-header">
        <span className="sensor-name" title={def.id}>
          {def.label}
        </span>
        <span className="sensor-kind">{def.kind}</span>
        <span
          className={`sensor-reading${paused ? " is-paused" : ""}`}
          title={paused ? "Paused: this was its last reading" : "Latest reading"}
        >
          {paused && "paused · "}
          {formatValue(view?.last_value)} <span className="muted">{def.unit}</span>
        </span>
      </div>
      <div className="sensor-row">
        <input
          type="range"
          min={def.min}
          max={def.max}
          step={step}
          value={shown}
          aria-label={`${def.label} setpoint`}
          onChange={(event) => onSlider(Number(event.target.value))}
        />
        <input
          type="number"
          className="number-input"
          min={def.min}
          max={def.max}
          step={step}
          value={valueText ?? String(Number(shown.toFixed(6)))}
          aria-label={`${def.label} setpoint value`}
          onChange={(event) => setValueText(event.target.value)}
          onBlur={commitValue}
          onKeyDown={blurOnEnter}
        />
        <button
          type="button"
          className="icon-button"
          title={`Reset to default (${def.default} ${def.unit})`}
          disabled={setpoint === def.default}
          onClick={(event) => {
            event.stopPropagation();
            sendSetpoint(def.default);
          }}
        >
          <RotateCcw size={13} />
        </button>
        <button
          type="button"
          className={`icon-button${paused ? " is-active" : ""}`}
          title={paused ? "Resume this sensor's readings" : "Pause this sensor's readings, as if it had failed"}
          aria-pressed={paused}
          onClick={(event) => {
            event.stopPropagation();
            send({ type: "set_sensor_paused", sensor: def.id, paused: !paused });
          }}
        >
          {paused ? <Play size={13} /> : <Pause size={13} />}
        </button>
      </div>
      <div className="sensor-row small muted">
        <span>
          range {def.min}–{def.max} · every {def.period_ms} ms
        </span>
        <label className="noise-field">
          noise σ
          <input
            type="number"
            className="number-input small"
            min={0}
            step={step}
            value={noiseText ?? String(noise)}
            onChange={(event) => setNoiseText(event.target.value)}
            onBlur={commitNoise}
            onKeyDown={blurOnEnter}
          />
        </label>
      </div>
    </div>
  );
}

/** A slider step of roughly 1/200 of the range, rounded to 1, 2 or 5 × 10^n. */
function niceStep(span: number): number {
  const raw = span / 200;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const normalized = raw / magnitude;
  const nice = normalized < 1.5 ? 1 : normalized < 3.5 ? 2 : 5;
  return nice * magnitude;
}
