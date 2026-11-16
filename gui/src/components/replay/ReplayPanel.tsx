import { CircleCheck, ChevronDown, ChevronRight, FileJson, Play, Square, X } from "lucide-react";
import { useEffect, useState } from "react";
import type { FinishedReplay, ReplayChoice } from "../../hooks/useSimulation";
import type { ReplayFormat, Scenario, WorldState } from "../../types";

interface ReplayPanelProps {
  scenario: Scenario;
  world: WorldState | null;
  /** The replay that last ran to its end, if any: noted above the settings. */
  finished: FinishedReplay | null;
  /** Opens the file picker; resolves to the chosen trace files (null if cancelled). */
  onPickTraces: () => Promise<string[] | null>;
  onStart: (paths: string[], choice: ReplayChoice) => void;
  onStop: () => void;
}

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;
const seconds = (ms: number) => `${(ms / 1000).toFixed(1)} s`;

const FORMAT_CAPTIONS: Record<ReplayFormat, string> = {
  payload: "Each payload exactly as in the trace.",
  envelope: "Adds v, seq, sim_ms and kind in front of each payload.",
};

/**
 * Replaying Liminal Desk traces on this layout: which traces and how (starting from the scenario's
 * [replay], if any), then progress. Folded away while the scenario names no traces.
 */
export function ReplayPanel({ scenario, world, finished, onPickTraces, onStart, onStop }: ReplayPanelProps) {
  const status = world?.replay ?? null;
  const [open, setOpen] = useState(scenario.replay !== null);
  const [traces, setTraces] = useState<string[]>(scenario.replay?.traces ?? []);
  const [format, setFormat] = useState<ReplayFormat>(scenario.replay?.format ?? "payload");
  const [positions, setPositions] = useState(scenario.replay?.positions ?? false);

  // A newly loaded scenario brings its own defaults.
  useEffect(() => {
    setOpen(scenario.replay !== null);
    setTraces(scenario.replay?.traces ?? []);
    setFormat(scenario.replay?.format ?? "payload");
    setPositions(scenario.replay?.positions ?? false);
  }, [scenario]);

  const addTraces = async () => {
    const picked = await onPickTraces();
    if (picked && picked.length > 0) {
      setTraces((current) => [...current, ...picked.filter((path) => !current.includes(path))]);
    }
  };

  const removeTrace = (path: string) => setTraces((current) => current.filter((item) => item !== path));

  const state = status ? (status.done ? "done" : world?.paused ? "paused" : "running") : null;

  return (
    <section className="panel-section replay-panel">
      <button
        type="button"
        className="panel-toggle"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        title={open ? "Hide the replay settings" : "Replay Liminal Desk traces on this floor"}
      >
        {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        <h2>Replay</h2>
        {!open && !state && <span className="panel-subtitle">· Liminal Desk traces</span>}
        {state && <span className={`replay-state is-${state}`}>{state}</span>}
      </button>

      {open && status && (
        <div className="replay-body">
          <div className="replay-title">
            {status.done
              ? "Replay finished"
              : `Replaying ${status.traces.length} Liminal Desk ${status.traces.length === 1 ? "trace" : "traces"}`}
          </div>
          <div className="replay-progress-row">
            <div className="replay-progress" aria-hidden>
              <div style={{ width: `${status.total > 0 ? (100 * status.sent) / status.total : 100}%` }} />
            </div>
            <span className="replay-time">
              {seconds(world?.sim_ms ?? 0)} of {seconds(status.duration_ms)}
            </span>
          </div>
          <div className="field-caption">
            {status.sent.toLocaleString()} of {status.total.toLocaleString()} payloads sent
          </div>
          <ul className="replay-trace-list">
            {status.traces.map((name) => (
              <li key={name} className="replay-trace">
                <FileJson size={13} />
                <span className="replay-trace-name">{name}</span>
              </li>
            ))}
          </ul>
          <div className="field-caption">
            {status.format === "payload" ? "Payload" : "Envelope"} format · positions {status.positions ? "on" : "off"}
          </div>
          <button type="button" className="primary-button danger" onClick={onStop} title="Stop the replay and simulate this floor">
            <Square size={12} />
            Stop replay
          </button>
          <p className="hint">
            Manual control is off. Pause and Reset in the toolbar act on the replay, and once everything is sent the
            floor goes back to simulating.
          </p>
        </div>
      )}

      {open && !status && (
        <div className="replay-body">
          {finished && (
            <div className="replay-note" title={finished.traces.join(", ")}>
              <CircleCheck size={13} />
              <span>
                Last replay finished: {finished.total.toLocaleString()} payloads over {seconds(finished.duration_ms)}.
              </span>
            </div>
          )}
          <p className="replay-intro">
            Plays Liminal Desk traces on this floor. Their payloads go to the outputs at their recorded
            times, exactly as written.
          </p>
          {/*  Nothing else is simulated. */}

          <div className="replay-card">
            <div className="replay-card-header">
              <span>Traces</span>
              <button
                type="button"
                className="link-button"
                onClick={() => void addTraces()}
                title="Add one or more Liminal Desk trace files"
              >
                Add…
              </button>
            </div>
            {traces.length > 0 ? (
              <ul className="replay-trace-list">
                {traces.map((path) => (
                  <li key={path} className="replay-trace" title={path}>
                    <FileJson size={13} />
                    <span className="replay-trace-name">{fileName(path)}</span>
                    <button
                      type="button"
                      className="replay-trace-remove"
                      onClick={() => removeTrace(path)}
                      title={`Remove ${fileName(path)}`}
                      aria-label={`Remove ${fileName(path)}`}
                    >
                      <X size={12} />
                    </button>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="replay-empty">No traces yet. Add one or more Liminal Desk trace files (.json).</p>
            )}
          </div>

          <div className="field-grid">
            <span className="field-label">Output</span>
            <div>
              <div className="segmented" role="radiogroup" aria-label="Output format">
                {(["payload", "envelope"] as const).map((value) => (
                  <button
                    key={value}
                    type="button"
                    role="radio"
                    aria-checked={format === value}
                    className={format === value ? "is-active" : ""}
                    onClick={() => setFormat(value)}
                  >
                    {value === "payload" ? "Payload" : "Envelope"}
                  </button>
                ))}
              </div>
              <div className="field-caption">{FORMAT_CAPTIONS[format]}</div>
            </div>

            <span className="field-label">Positions</span>
            <div>
              <label className="field-check">
                <input type="checkbox" checked={positions} onChange={(event) => setPositions(event.target.checked)} />
                Report MM positions
              </label>
              <div className="field-caption">
                Every {scenario.output.position_interval_ms} ms while an MM moves.
              </div>
            </div>
          </div>

          <button
            type="button"
            className="primary-button"
            disabled={traces.length === 0}
            onClick={() => onStart(traces, { envelope: format === "envelope", positions })}
            title={traces.length === 0 ? "Add a trace first" : "Replay these traces on this floor"}
          >
            <Play size={12} />
            Start replay
          </button>
        </div>
      )}
    </section>
  );
}
