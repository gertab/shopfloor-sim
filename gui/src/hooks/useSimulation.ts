import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import { EVENTS_EVENT, STATE_EVENT, api } from "../api";
import type { Control, EventKind, LogEntry, Scenario, SimCommand, WorldState } from "../types";

const MAX_EVENTS = 2000;

/** The latest action of an MM, from the log (replayed actions have no position). */
export interface LastAction {
  seq: number;
  event: string;
  at: string | null;
  x: number | null;
  y: number | null;
  sim_ms: number;
}

/** What the last replay that ran to its end played. */
export interface FinishedReplay {
  traces: string[];
  total: number;
  duration_ms: number;
}

export interface ReplayChoice {
  envelope: boolean;
  positions: boolean;
}

function lastActionOf(entry: LogEntry): LastAction {
  const { data } = entry;
  return {
    seq: entry.seq,
    event: typeof data.event === "string" ? data.event : "?",
    // Replayed dock/undock payloads name their machine as `target`.
    at: typeof data.at === "string" ? data.at : typeof data.target === "string" ? data.target : null,
    x: typeof data.x === "number" ? data.x : null,
    y: typeof data.y === "number" ? data.y : null,
    sim_ms: entry.sim_ms,
  };
}

/** Warnings as one toast that stays until dismissed (multi-line). */
function warningsText(warnings: string[]): string | null {
  return warnings.length > 0 ? `Replay checks:\n${warnings.map((warning) => `- ${warning}`).join("\n")}` : null;
}

export interface Simulation {
  scenario: Scenario | null;
  world: WorldState | null;
  /** The most recent log entries (at most MAX_EVENTS). */
  events: LogEntry[];
  /** Total entries per kind since the scenario was loaded or the log was cleared. */
  counts: Partial<Record<EventKind, number>>;
  /** The latest action per MM id (for on-floor feedback). */
  lastActions: Record<string, LastAction>;
  /** Live status of sinks that report one (TCP connections), by sink name. */
  sinkStatus: Record<string, string>;
  /** The replay that last ran to its end (the app then went back to simulating), if any. */
  finishedReplay: FinishedReplay | null;
  error: string | null;
  loading: boolean;
  load: (path?: string) => Promise<void>;
  pickAndLoad: () => Promise<void>;
  /** Opens the file picker for trace files (null if cancelled). */
  pickTraceFiles: () => Promise<string[] | null>;
  /** Replays these traces on the loaded layout. */
  startReplay: (paths: string[], choice: ReplayChoice) => Promise<void>;
  /** Back to simulating the loaded layout. */
  stopReplay: () => Promise<void>;
  send: (command: SimCommand) => Promise<void>;
  control: (control: Control) => Promise<void>;
  clearEvents: () => void;
  dismissError: () => void;
}

export function useSimulation(): Simulation {
  const [scenario, setScenario] = useState<Scenario | null>(null);
  const [world, setWorld] = useState<WorldState | null>(null);
  const [events, setEvents] = useState<LogEntry[]>([]);
  const [counts, setCounts] = useState<Partial<Record<EventKind, number>>>({});
  const [lastActions, setLastActions] = useState<Record<string, LastAction>>({});
  const [sinkStatus, setSinkStatus] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [finishedReplay, setFinishedReplay] = useState<FinishedReplay | null>(null);
  const loadedOnceRef = useRef(false);
  // Whether the current replay has already been left (stopped, or finished and handed back), so
  // that a late "done" from it doesn't hand back twice.
  const replayLeftRef = useRef(true);

  useEffect(() => {
    let disposed = false;
    const unlistenState = listen<WorldState>(STATE_EVENT, (event) => {
      if (!disposed) {
        setWorld(event.payload);
      }
    });
    const unlistenEvents = listen<LogEntry[]>(EVENTS_EVENT, (event) => {
      if (disposed) {
        return;
      }
      const batch = event.payload;
      setEvents((previous) => [...previous, ...batch].slice(-MAX_EVENTS));
      setCounts((previous) => {
        const next = { ...previous };
        for (const item of batch) {
          if (item.kind) next[item.kind] = (next[item.kind] ?? 0) + 1;
        }
        return next;
      });
      const actions = batch.filter((item) => item.kind === "action" && item.device_id !== null);
      if (actions.length > 0) {
        setLastActions((previous) => {
          const next = { ...previous };
          for (const action of actions) {
            next[action.device_id as string] = lastActionOf(action);
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

  /** Runs `action`; what it throws becomes the error banner (and the result `undefined`). */
  const reportErrors = useCallback(async <T>(action: () => Promise<T>): Promise<T | undefined> => {
    try {
      return await action();
    } catch (caught) {
      setError(String(caught));
      return undefined;
    }
  }, []);

  const load = useCallback(
    async (path?: string) => {
      setLoading(true);
      // Clear first: the new run's opening events arrive while loadScenario is still resolving.
      clearEvents();
      setFinishedReplay(null);
      replayLeftRef.current = true;
      const loaded = await reportErrors(async () => {
        setScenario(await api.loadScenario(path));
        setWorld(await api.getState());
        setError(null);
        return true;
      });
      if (!loaded) {
        // The run may have changed anyway: the previous scenario started again (its outputs reopened)
        // or nothing runs. Show what does, keeping the scenario object if it is the same one.
        try {
          const running = await api.getScenario();
          setScenario((current) => (JSON.stringify(current) === JSON.stringify(running) ? current : running));
          setWorld(await api.getState());
        } catch {
          // Keep what is shown; the load error is already on screen.
        }
      }
      setLoading(false);
    },
    [clearEvents, reportErrors],
  );

  const pickAndLoad = useCallback(async () => {
    const path = await reportErrors(() => api.pickScenarioFile());
    if (path) {
      await load(path);
    }
  }, [load, reportErrors]);

  const pickTraceFiles = useCallback(
    async () => (await reportErrors(() => api.pickTraceFiles())) ?? null,
    [reportErrors],
  );

  const startReplay = useCallback(
    async (paths: string[], { envelope, positions }: ReplayChoice) => {
      clearEvents();
      setFinishedReplay(null);
      replayLeftRef.current = false;
      await reportErrors(async () => {
        const warnings = await api.startReplay(paths, envelope, positions);
        setWorld(await api.getState());
        setError(warningsText(warnings));
      });
    },
    [clearEvents, reportErrors],
  );

  const stopReplay = useCallback(async () => {
    replayLeftRef.current = true;
    clearEvents();
    await reportErrors(async () => {
      await api.stopReplay();
      setWorld(await api.getState());
    });
  }, [clearEvents, reportErrors]);

  // A replay that has sent everything hands back to the simulation by itself.
  const replay = world?.replay ?? null;
  useEffect(() => {
    if (!replay?.done || replayLeftRef.current) return;
    const finished = { traces: replay.traces, total: replay.total, duration_ms: replay.duration_ms };
    void stopReplay().then(() => setFinishedReplay(finished));
  }, [replay, stopReplay]);

  const send = useCallback(
    async (command: SimCommand) => {
      await reportErrors(() => api.sendCommand(command));
    },
    [reportErrors],
  );

  const control = useCallback(
    async (value: Control) => {
      await reportErrors(() => api.control(value));
    },
    [reportErrors],
  );

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
    finishedReplay,
    error,
    loading,
    load,
    pickAndLoad,
    pickTraceFiles,
    startReplay,
    stopReplay,
    send,
    control,
    clearEvents,
    dismissError,
  };
}
