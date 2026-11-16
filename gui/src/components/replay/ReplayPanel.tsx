import { ChevronDown, ChevronRight, FolderOpen, Play, Square } from "lucide-react";
import { useEffect, useState } from "react";
import type { ReplayChoice } from "../../hooks/useSimulation";
import type { ReplayFormat, Scenario, WorldState } from "../../types";

interface ReplayPanelProps {
  scenario: Scenario;
  world: WorldState | null;
  /** Opens the file picker; resolves to the chosen trace files (null if cancelled). */
  onPickTraces: () => Promise<string[] | null>;
  onStart: (paths: string[], choice: ReplayChoice) => void;
  onStop: () => void;
}

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;
const seconds = (ms: number) => `${(ms / 1000).toFixed(1)} s`;

/**
 * Replaying Liminal Desk traces on this layout: which traces and how (from the scenario's [replay],
 * if any), then progress. Collapsed while the scenario names no traces.
 */
export function ReplayPanel({ scenario, world, onPickTraces, onStart, onStop }: ReplayPanelProps) {
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

  const chooseTraces = async () => {
    const picked = await onPickTraces();
    if (picked && picked.length > 0) setTraces(picked);
  };

  return (
    <section className="panel-section replay-panel">
      <button
        type="button"
        className="panel-toggle"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        title={open ? "Hide the replay settings" : "Replay Liminal Desk traces on this layout"}
      >
        {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        <h2>Replay</h2>
        {status && <span className={`replay-state${status.done ? " is-done" : ""}`}>{status.done ? "done" : "running"}</span>}
      </button>

      {open &&
        (status ? (
          <div className="replay-body">
            <div className="replay-progress" aria-hidden>
              <div style={{ width: `${status.total > 0 ? (100 * status.sent) / status.total : 100}%` }} />
            </div>
            <div className="small">
              {seconds(world?.sim_ms ?? 0)} / {seconds(status.duration_ms)} · {status.sent} of {status.total} payloads
              {status.done ? " · done" : world?.paused ? " · paused" : ""}
            </div>
            <ul className="replay-traces">
              {status.traces.map((name) => (
                <li key={name}>{name}</li>
              ))}
            </ul>
            <div className="muted small">
              {status.format} format{status.positions ? " · with positions" : ""}
            </div>
            <div className="replay-actions">
              <button type="button" className="chip-button danger" onClick={onStop} title="Stop the replay and simulate this layout">
                <Square size={11} />
                Stop replay
              </button>
            </div>
            <p className="hint">Pause and Reset in the toolbar act on the replay. Manual control is off until it stops.</p>
          </div>
        ) : (
          <div className="replay-body">
            {traces.length > 0 ? (
              <ul className="replay-traces">
                {traces.map((path) => (
                  <li key={path} title={path}>
                    {fileName(path)}
                  </li>
                ))}
              </ul>
            ) : (
              <p className="hint">No traces chosen.</p>
            )}
            <div className="replay-actions">
              <button type="button" className="chip-button" onClick={() => void chooseTraces()} title="Pick one or more trace files">
                <FolderOpen size={12} />
                Choose traces…
              </button>
            </div>
            <div className="replay-options">
              <label title="How each payload is written: as in the trace, or with v, seq, sim_ms and kind in front">
                format
                <select value={format} onChange={(event) => setFormat(event.target.value as ReplayFormat)}>
                  <option value="payload">payload</option>
                  <option value="envelope">envelope</option>
                </select>
              </label>
              <label title="Also report the positions of moving MMs">
                <input type="checkbox" checked={positions} onChange={(event) => setPositions(event.target.checked)} />
                positions
              </label>
            </div>
            <div className="replay-actions">
              <button
                type="button"
                className="chip-button replay-start"
                disabled={traces.length === 0}
                onClick={() => onStart(traces, { envelope: format === "envelope", positions })}
                title={traces.length === 0 ? "Choose traces first" : "Replay these traces on this layout"}
              >
                <Play size={11} />
                Start replay
              </button>
            </div>
            <p className="hint">
              Plays the traces' payloads at their recorded times and nothing else. The MMs follow their undock, move and
              dock events.
            </p>
          </div>
        ))}
    </section>
  );
}
