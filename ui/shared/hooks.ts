import { useCallback, useEffect, useRef, useState, type DependencyList } from "react";
import { on, type EventPayloads } from "./api";

export interface AsyncState<T> {
  data: T | null;
  error: string | null;
  loading: boolean;
  reload: () => Promise<void>;
  set: (value: T | null) => void;
}

/** Run an async loader on mount (and whenever `deps` change). */
export function useAsync<T>(loader: () => Promise<T>, deps: DependencyList = []): AsyncState<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const seq = useRef(0);

  const reload = useCallback(async () => {
    const id = ++seq.current;
    setLoading(true);
    try {
      const value = await loader();
      if (id === seq.current) {
        setData(value);
        setError(null);
      }
    } catch (err) {
      if (id === seq.current) setError(describe(err));
    } finally {
      if (id === seq.current) setLoading(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);

  useEffect(() => {
    void reload();
  }, [reload]);

  return { data, error, loading, reload, set: setData };
}

export interface Action<T> {
  busy: boolean;
  result: T | null;
  error: string | null;
  start: () => Promise<void>;
  reset: () => void;
}

/** A button's worth of async work: one run at a time, its outcome kept for display. */
export function useAction<T>(run: () => Promise<T>, onError?: (message: string) => void): Action<T> {
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reset = () => {
    setResult(null);
    setError(null);
  };

  const start = async () => {
    setBusy(true);
    reset();
    try {
      setResult(await run());
    } catch (err) {
      const message = describe(err);
      setError(message);
      onError?.(message);
    } finally {
      setBusy(false);
    }
  };

  return { busy, result, error, start, reset };
}

/** Subscribe to a Rust event for the lifetime of the component. */
export function useEvent<E extends keyof EventPayloads>(event: E, handler: (payload: EventPayloads[E]) => void): void {
  const latest = useRef(handler);
  latest.current = handler;
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void on(event, (payload) => latest.current(payload)).then((off) => {
      if (cancelled) off();
      else unlisten = off;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [event]);
}

export function useInterval(fn: () => void, ms: number | null): void {
  const latest = useRef(fn);
  latest.current = fn;
  useEffect(() => {
    if (ms === null) return;
    const id = window.setInterval(() => latest.current(), ms);
    return () => window.clearInterval(id);
  }, [ms]);
}

/**
 * Fire and forget a call that tells the user itself when it fails (a toast,
 * as `save` does), so its rejection is not left unhandled.
 */
export function detach(promise: Promise<unknown>): void {
  promise.catch(() => {});
}

export function describe(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}
