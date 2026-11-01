"use client";

import { useCallback, useEffect, useState } from "react";
import { api } from "./api";

export const POLL_MS = 5000;

export type Polled<T> = {
  data: T | null;
  error: unknown;
  /** True until the first answer (or error) arrives. */
  loading: boolean;
  /** Fetches again now, for example after the person changed something. */
  refresh: () => void;
};

/**
 * Reads a server path now and again every few seconds while the page is
 * visible. Nothing is cached: every read goes to the server with the
 * person's session, so what is shown is what they may see right now.
 * Pass null to read nothing.
 */
export function usePoll<T>(path: string | null, intervalMs: number = POLL_MS): Polled<T> {
  const [state, setState] = useState<{ path: string | null; data: T | null; error: unknown }>({
    path: null,
    data: null,
    error: null,
  });
  const [tick, setTick] = useState(0);

  const refresh = useCallback(() => setTick((value) => value + 1), []);

  useEffect(() => {
    if (!path) {
      return;
    }
    let active = true;
    let busy = false;
    async function load() {
      if (busy || document.visibilityState === "hidden") {
        return;
      }
      busy = true;
      try {
        const data = await api<T>(path as string);
        if (active) setState({ path, data, error: null });
      } catch (error) {
        if (active) setState((previous) => ({ path, data: previous.path === path ? previous.data : null, error }));
      } finally {
        busy = false;
      }
    }
    void load();
    const timer = window.setInterval(() => void load(), intervalMs);
    const onVisible = () => {
      if (document.visibilityState === "visible") void load();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      active = false;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [path, intervalMs, tick]);

  const current = state.path === path;
  return {
    data: current ? state.data : null,
    error: current ? state.error : null,
    loading: path !== null && !current,
    refresh,
  };
}
