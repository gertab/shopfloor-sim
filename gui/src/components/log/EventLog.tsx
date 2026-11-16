import { Braces, ChevronDown, ChevronUp, Trash2 } from "lucide-react";
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { KIND_GROUP, KIND_LABELS, LOG_GROUPS, describeEntry, formatSimTime, rowLabel } from "../../format";
import type { LogGroup } from "../../format";
import type { EventKind, LogEntry, Scenario } from "../../types";

/** The kinds in each log group, in the order they're listed in a chip's tooltip. */
const GROUP_KINDS = Object.fromEntries(
  LOG_GROUPS.map((group) => [
    group,
    (Object.keys(KIND_GROUP) as EventKind[]).filter((kind) => KIND_GROUP[kind] === group),
  ]),
) as Record<LogGroup, EventKind[]>;

const GROUP_TITLES: Record<LogGroup, string> = {
  position: "positions",
  sensor: "sensor readings",
  event: "events",
  sim: "simulation events",
};

function groupContents(group: LogGroup): string {
  return GROUP_KINDS[group].map((kind) => (kind === "action" ? "actions" : KIND_LABELS[kind])).join(", ");
}

/** Rows rendered at most (the hook keeps more for filtering). */
const MAX_ROWS = 500;

interface EventLogProps {
  scenario: Scenario | null;
  events: LogEntry[];
  counts: Partial<Record<EventKind, number>>;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  onClear: () => void;
}

/** Live view of the output stream: exactly what sinks (and their consumers) receive. */
export function EventLog({ scenario, events, counts, collapsed, onToggleCollapsed, onClear }: EventLogProps) {
  const [hiddenGroups, setHiddenGroups] = useState<Set<LogGroup>>(new Set());
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
        !(event.kind && hiddenGroups.has(KIND_GROUP[event.kind])) && (entity === "all" || event.device_id === entity),
    );
    return filtered.slice(-MAX_ROWS);
  }, [events, hiddenGroups, entity]);

  useLayoutEffect(() => {
    const element = scrollRef.current;
    if (followRef.current && element) {
      element.scrollTop = element.scrollHeight;
    }
  }, [rows]);

  const toggleGroup = (group: LogGroup) =>
    setHiddenGroups((previous) => {
      const next = new Set(previous);
      if (next.has(group)) next.delete(group);
      else next.add(group);
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
              {LOG_GROUPS.map((group) => (
                <button
                  key={group}
                  type="button"
                  className={`kind-chip group-${group}${hiddenGroups.has(group) ? " is-off" : ""}`}
                  onClick={() => toggleGroup(group)}
                  title={`${hiddenGroups.has(group) ? "Show" : "Hide"} ${GROUP_TITLES[group]}: ${groupContents(group)}`}
                >
                  {group}{" "}
                  <span className="count">
                    {GROUP_KINDS[group].reduce((total, kind) => total + (counts[kind] ?? 0), 0)}
                  </span>
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
                {JSON.stringify(event.data)}
              </div>
            ) : (
              <div key={event.seq} className="event-row" title={JSON.stringify(event.data)}>
                <span className="event-time">{formatSimTime(event.sim_ms)}</span>
                <span className={`event-kind group-${event.kind ? KIND_GROUP[event.kind] : "other"}`}>
                  {rowLabel(event)}
                </span>
                <span className="event-entity">{event.device_id ?? ""}</span>
                <span className="event-text">{describeEntry(event, scenario)}</span>
              </div>
            ),
          )}
          {rows.length === 0 && <div className="event-empty">No events yet.</div>}
        </div>
      )}
    </section>
  );
}
