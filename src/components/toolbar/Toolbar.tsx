import { Cable, Clapperboard, FileText, FolderOpen, Pause, Play, RefreshCw, RotateCcw, Terminal } from "lucide-react";
import { formatSimTime } from "../../format";
import type { Control, Scenario, WorldState } from "../../types";

interface ToolbarProps {
  scenario: Scenario | null;
  world: WorldState | null;
  loading: boolean;
  selectedMmLabel: string | null;
  sinkStatus: Record<string, string>;
  onOpen: () => void;
  onReload: () => void;
  onControl: (control: Control) => void;
}

export function Toolbar({
  scenario,
  world,
  loading,
  selectedMmLabel,
  sinkStatus,
  onOpen,
  onReload,
  onControl,
}: ToolbarProps) {
  const paused = world?.paused ?? false;
  return (
    <header className="toolbar">
      <div className="brand">Shopfloor</div>
      <div className="scenario-name" title={scenario?.source_path ?? ""}>
        {scenario?.name ?? (loading ? "Loading…" : "No scenario")}
      </div>
      <button type="button" className="tool-button" onClick={onOpen} title="Open a scenario file">
        <FolderOpen size={15} />
        Open
      </button>
      <button
        type="button"
        className="tool-button"
        onClick={onReload}
        disabled={!scenario}
        title="Reload the scenario file from disk (restarts the simulation)"
      >
        <RefreshCw size={15} />
        Reload
      </button>
      <div className="divider" />
      <button
        type="button"
        className="tool-button"
        disabled={!world}
        onClick={() => onControl(paused ? "resume" : "pause")}
        title={paused ? "Resume (Space)" : "Pause (Space)"}
      >
        {paused ? <Play size={15} /> : <Pause size={15} />}
        {paused ? "Resume" : "Pause"}
      </button>
      <button
        type="button"
        className="tool-button"
        disabled={!world}
        onClick={() => onControl("reset")}
        title="Back to the initial state: start positions, default sensor values, time 0"
      >
        <RotateCcw size={15} />
        Reset
      </button>
      <div className={`sim-clock${paused ? " is-paused" : ""}`} title="Simulation time">
        {world ? formatSimTime(world.sim_ms) : "--:--.-"}
        {paused && <span className="paused-badge">paused</span>}
      </div>
      {world?.scene && (
        <div className="scene-badge" title={`Scene '${world.scene.id}' is playing`}>
          <Clapperboard size={13} />
          {world.scene.label} · #{world.scene.iteration}
        </div>
      )}
      <div className="spacer" />
      {selectedMmLabel && (
        <div className="targeting-hint">
          {selectedMmLabel} selected: click a machine or the floor · Esc to cancel
        </div>
      )}
      <div className="sinks" title="Where events are written besides this window ([output.sinks] in the scenario)">
        {scenario?.output.sinks.map((sink) => {
          if (sink.type === "tcp") {
            const status = sinkStatus[sink.name];
            const clients = Number(status?.match(/(\d+) connected/)?.[1] ?? 0);
            return (
              <span
                key={sink.name}
                className={`sink-badge${clients > 0 ? " is-connected" : ""}`}
                title={`tcp: ${status ?? `not listening (${sink.bind})`}`}
              >
                <Cable size={12} />
                {sink.name}
                <span className="sink-dot" />
                {clients}
              </span>
            );
          }
          return (
            <span key={sink.name} className="sink-badge" title={sink.type === "jsonl" ? sink.path : "stdout"}>
              {sink.type === "jsonl" ? <FileText size={12} /> : <Terminal size={12} />}
              {sink.name}
            </span>
          );
        })}
        {scenario && scenario.output.sinks.length === 0 && <span className="muted small">no sinks</span>}
      </div>
    </header>
  );
}
