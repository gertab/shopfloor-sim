import { X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { ControlPanel } from "./components/controls/ControlPanel";
import { FloorView } from "./components/floor/FloorView";
import type { Focus } from "./components/floor/FloorView";
import { EventLog } from "./components/log/EventLog";
import { Toolbar } from "./components/toolbar/Toolbar";
import { clampToReach } from "./geometry";
import { useSimulation } from "./hooks/useSimulation";
import type { Vec2 } from "./types";

export default function App() {
  const sim = useSimulation();
  const { scenario, world, send, control } = sim;
  const [selectedMm, setSelectedMm] = useState<string | null>(null);
  const [focus, setFocus] = useState<Focus>(null);
  const [logCollapsed, setLogCollapsed] = useState(false);

  // A new scenario may not have the selected entities.
  useEffect(() => {
    setSelectedMm(null);
    setFocus(null);
  }, [scenario]);

  // Short command errors disappear by themselves; multi-line ones (scenario validation) stay.
  const { error, dismissError } = sim;
  useEffect(() => {
    if (!error || error.includes("\n")) return;
    const timer = window.setTimeout(dismissError, 6000);
    return () => window.clearTimeout(timer);
  }, [error, dismissError]);

  const selectedDef = scenario?.mobile_manipulators.find((mm) => mm.id === selectedMm) ?? null;

  const onFloorClick = useCallback(
    (point: Vec2 | null) => {
      if (!scenario || !selectedDef || !point) {
        setSelectedMm(null);
        setFocus(null);
        return;
      }
      const target = clampToReach(point, selectedDef, scenario.floor.width_m, scenario.floor.height_m);
      void send({ type: "move_to", mm: selectedDef.id, target: { point: target } });
    },
    [scenario, selectedDef, send],
  );

  const onMachineClick = useCallback(
    (id: string) => {
      if (selectedMm) {
        void send({ type: "move_to", mm: selectedMm, target: { machine: id } });
      } else {
        setFocus({ type: "machine", id });
      }
    },
    [selectedMm, send],
  );

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing = target?.tagName === "INPUT" || target?.tagName === "SELECT" || target?.tagName === "TEXTAREA";
      if (event.key === "Escape" && !typing) {
        setSelectedMm(null);
        setFocus(null);
      } else if (event.key === " " && !typing && world) {
        event.preventDefault();
        void control(world.paused ? "resume" : "pause");
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [control, world]);

  return (
    <div className={`app${logCollapsed ? " log-collapsed" : ""}`}>
      <Toolbar
        scenario={scenario}
        world={world}
        loading={sim.loading}
        selectedMmLabel={selectedDef?.label ?? null}
        sinkStatus={sim.sinkStatus}
        onOpen={() => void sim.pickAndLoad()}
        onReload={() => void sim.load(scenario?.source_path ?? undefined)}
        onControl={(value) => void control(value)}
      />
      <main className="main">
        <div className="floor-pane">
          {scenario ? (
            <FloorView
              scenario={scenario}
              world={world}
              selectedMm={selectedMm}
              focus={focus}
              lastActions={sim.lastActions}
              onSelectMm={(id) => setSelectedMm((current) => (current === id ? null : id))}
              onFloorClick={onFloorClick}
              onMachineClick={onMachineClick}
              onSensorClick={(id) => setFocus({ type: "sensor", id })}
            />
          ) : (
            <div className="empty-state">{sim.loading ? "Loading scenario…" : "No scenario loaded."}</div>
          )}
        </div>
        {scenario && (
          <ControlPanel
            scenario={scenario}
            world={world}
            selectedMm={selectedMm}
            focus={focus}
            lastActions={sim.lastActions}
            onSelectMm={(id) => setSelectedMm((current) => (current === id ? null : id))}
            onFocusSensor={(id) => setFocus({ type: "sensor", id })}
            send={(command) => void send(command)}
            onControl={(value) => void control(value)}
          />
        )}
      </main>
      <EventLog
        scenario={scenario}
        events={sim.events}
        counts={sim.counts}
        collapsed={logCollapsed}
        onToggleCollapsed={() => setLogCollapsed((value) => !value)}
        onClear={sim.clearEvents}
      />
      {sim.error && (
        <div className="toast" role="alert">
          <pre>{sim.error}</pre>
          <button type="button" className="icon-button" onClick={sim.dismissError} title="Dismiss">
            <X size={14} />
          </button>
        </div>
      )}
    </div>
  );
}
