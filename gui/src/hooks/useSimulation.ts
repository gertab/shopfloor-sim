import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import { EVENTS_EVENT, STATE_EVENT, api } from "../api";
import type { Control, EventKind, Scenario, SimCommand, SimEvent, WorldState } from "../types";

const MAX_EVENTS = 2000;

export type ActionEvent = Extract<SimEvent, { kind: "action" }>;

export interface Simulation {
  scenario: Scenario | null;
  world: WorldState | null;
  /** The most recent events (at most MAX_EVENTS). */
  events: SimEvent[];
  /** Total events per kind since the scenario was loaded or the log was cleared. */
  counts: Partial<Record<EventKind, number>>;
  /** The latest `action` event per MM id (for on-floor feedback). */
  lastActions: Record<string, ActionEvent>;
  /** Live status of sinks that report one (TCP connections), by sink name. */
  sinkStatus: Record<string, string>;
  error: string | null;
  loading: boolean;
  load: (path?: string) => Promise<void>;
  pickAndLoad: () => Promise<void>;
  send: (command: SimCommand) => Promise<void>;
  control: (control: Control) => Promise<void>;
  clearEvents: () => void;
  dismissError: () => void;
}

export function useSimulation(): Simulation {
  const [scenario, setScenario] = useState<Scenario | null>(null);
  const [world, setWorld] = useState<WorldState | null>(null);
  const [events, setEvents] = useState<SimEvent[]>([]);
  const [counts, setCounts] = useState<Partial<Record<EventKind, number>>>({});
  const [lastActions, setLastActions] = useState<Record<string, ActionEvent>>({});
  const [sinkStatus, setSinkStatus] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const loadedOnceRef = useRef(false);

  useEffect(() => {
    let disposed = false;
    const unlistenState = listen<WorldState>(STATE_EVENT, (event) => {
      if (!disposed) {
        setWorld(event.payload);
      }
    });
    const unlistenEvents = listen<SimEvent[]>(EVENTS_EVENT, (event) => {
      if (disposed) {
        return;
      }
      const batch = event.payload;
      setEvents((previous) => [...previous, ...batch].slice(-MAX_EVENTS));
      setCounts((previous) => {
        const next = { ...previous };
        for (const item of batch) {
          next[item.kind] = (next[item.kind] ?? 0) + 1;
        }
        return next;
      });
      const actions = batch.filter((item): item is ActionEvent => item.kind === "action");
      if (actions.length > 0) {
        setLastActions((previous) => {
          const next = { ...previous };
          for (const action of actions) {
            next[action.device_id] = action;
          }
          return next;
        });
      }
    });
    return () => {
      disposed = true;
      void unlistenState.then((unlisten) => unlisten());
      void unlistenEvents.then((unlisten) => unlisten());
    };
  }, []);

  const dismissError = useCallback(() => setError(null), []);

  const clearEvents = useCallback(() => {
    setEvents([]);
    setCounts({});
    setLastActions({});
  }, []);

  const load = useCallback(
    async (path?: string) => {
      setLoading(true);
      // Clear first: the new run's opening events arrive while loadScenario is still resolving.
      clearEvents();
      try {
        const loaded = await api.loadScenario(path);
        setScenario(loaded);
        setWorld(await api.getState());
        setError(null);
      } catch (loadError) {
        setError(String(loadError));
      } finally {
        setLoading(false);
      }
    },
    [clearEvents],
  );

  const pickAndLoad = useCallback(async () => {
    try {
      const path = await api.pickScenarioFile();
      if (path) {
        await load(path);
      }
    } catch (pickError) {
      setError(String(pickError));
    }
  }, [load]);

  const send = useCallback(async (command: SimCommand) => {
    try {
      await api.sendCommand(command);
    } catch (commandError) {
      setError(String(commandError));
    }
  }, []);

  const control = useCallback(async (value: Control) => {
    try {
      await api.control(value);
    } catch (controlError) {
      setError(String(controlError));
    }
  }, []);

  // Poll sink status (e.g. whether a consumer is connected over TCP).
  useEffect(() => {
    const poll = () => {
      api.getSinkStatus().then(setSinkStatus, () => setSinkStatus({}));
    };
    poll();
    const timer = window.setInterval(poll, 1000);
    return () => window.clearInterval(timer);
  }, []);

  // Load the default scenario once (the ref survives React StrictMode's double mount).
  useEffect(() => {
    if (!loadedOnceRef.current) {
      loadedOnceRef.current = true;
      void load();
    }
  }, [load]);

  return {
    scenario,
    world,
    events,
    counts,
    lastActions,
    sinkStatus,
    error,
    loading,
    load,
    pickAndLoad,
    send,
    control,
    clearEvents,
    dismissError,
  };
}
