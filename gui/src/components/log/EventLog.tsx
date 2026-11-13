import { Braces, ChevronDown, ChevronUp, Trash2 } from "lucide-react";
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { KIND_LABELS, describeEvent, eventDeviceId, formatSimTime } from "../../format";
import { EVENT_KINDS } from "../../types";
import type { EventKind, Scenario, SimEvent } from "../../types";

/** Rows rendered at most (the hook keeps more for filtering). */
const MAX_ROWS = 500;

interface EventLogProps {
  scenario: Scenario | null;
  events: SimEvent[];
  counts: Partial<Record<EventKind, number>>;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  onClear: () => void;
}

/** Live view of the output stream: exactly what sinks (and their consumers) receive. */
export function EventLog({ scenario, events, counts, collapsed, onToggleCollapsed, onClear }: EventLogProps) {
  const [hiddenKinds, setHiddenKinds] = useState<Set<EventKind>>(new Set());
  const [entity, setEntity] = useState<string>("all");
  const [raw, setRaw] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  // Like a terminal: new events keep the log scrolled to the bottom, unless you scrolled up to
  // read older ones; scrolling back to the bottom follows again.
  const followRef = useRef(true);

  const entities = useMemo(
    () => [
      ...(scenario?.mobile_manipulators.map((mm) => mm.id) ?? []),
      ...(scenario?.machines.map((machine) => machine.id) ?? []),
      ...(scenario?.sensors.filter((sensor) => sensor.host_kind === "floor").map((sensor) => sensor.id) ?? []),
    ],
    [scenario],
  );

  const rows = useMemo(() => {
    const filtered = events.filter(
      (event) =>
        !hiddenKinds.has(event.kind) && (entity === "all" || eventDeviceId(event) === entity),
    );
    return filtered.slice(-MAX_ROWS);
  }, [events, hiddenKinds, entity]);

  useLayoutEffect(() => {
    const element = scrollRef.current;
    if (followRef.current && element) {
      element.scrollTop = element.scrollHeight;
    }
  }, [rows]);

  const toggleKind = (kind: EventKind) =>
    setHiddenKinds((previous) => {
      const next = new Set(previous);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });

  return (
    <section className={`event-log${collapsed ? " is-collapsed" : ""}`}>
      <header className="event-log-toolbar">
        <button type="button" className="icon-button" onClick={onToggleCollapsed} title="Show/hide the event log">
          {collapsed ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
        </button>
        <h2>Output events</h2>
        {!collapsed && (
          <>
            <div className="kind-chips">
              {EVENT_KINDS.map((kind) => (
                <button
                  key={kind}
                  type="button"
                  className={`kind-chip kind-${kind}${hiddenKinds.has(kind) ? " is-off" : ""}`}
                  onClick={() => toggleKind(kind)}
                  title={hiddenKinds.has(kind) ? `Show ${kind} events` : `Hide ${kind} events`}
                >
                  {KIND_LABELS[kind]} <span className="count">{counts[kind] ?? 0}</span>
                </button>
              ))}
            </div>
            <select value={entity} onChange={(event) => setEntity(event.target.value)} title="Filter by entity">
              <option value="all">all entities</option>
              {entities.map((id) => (
                <option key={id} value={id}>
                  {id}
                </option>
              ))}
            </select>
            <div className="spacer" />
            <button
              type="button"
              className={`icon-button${raw ? " is-active" : ""}`}
              onClick={() => setRaw((value) => !value)}
              title="Show each event as JSON (whole numbers lose their .0 here; the sinks keep it)"
            >
              <Braces size={14} />
            </button>
            <button type="button" className="icon-button" onClick={onClear} title="Clear the log">
              <Trash2 size={14} />
            </button>
          </>
        )}
      </header>
      {!collapsed && (
        <div
          className="event-rows"
          ref={scrollRef}
          onScroll={(event) => {
            const element = event.currentTarget;
            followRef.current = element.scrollHeight - element.scrollTop - element.clientHeight < 24;
          }}
        >
          {rows.map((event) =>
            raw ? (
              <div key={event.seq} className="event-row raw">
                {JSON.stringify(event)}
              </div>
            ) : (
              <div key={event.seq} className="event-row" title={JSON.stringify(event)}>
                <span className="event-time">{formatSimTime(event.sim_ms)}</span>
                <span className={`event-kind kind-${event.kind}`}>{KIND_LABELS[event.kind]}</span>
                <span className="event-entity">{eventDeviceId(event) ?? ""}</span>
                <span className="event-text">{describeEvent(event, scenario)}</span>
              </div>
            ),
          )}
          {rows.length === 0 && <div className="event-empty">No events yet.</div>}
        </div>
      )}
    </section>
  );
}
