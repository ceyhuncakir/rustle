// The transport. Inside the Tauri app every call goes to Rust; in a plain
// browser (`pnpm dev`) the same calls are answered by mock.ts, so each page
// can be developed and screenshotted without the native app.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

export type Unlisten = () => void;
export type Handler<T> = (payload: T) => void;

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

const isTauri: boolean = typeof window !== "undefined" && window.__TAURI_INTERNALS__ !== undefined;

type Mock = typeof import("./mock");
let mock: Promise<Mock> | null = null;
function getMock(): Promise<Mock> {
  mock ??= import("./mock");
  return mock;
}

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) {
    return tauriInvoke<T>(cmd, args);
  }
  const m = await getMock();
  return m.invoke<T>(cmd, args);
}

export async function listen<T>(event: string, handler: Handler<T>): Promise<Unlisten> {
  if (isTauri) {
    return tauriListen<T>(event, (e) => handler(e.payload));
  }
  const m = await getMock();
  return m.listen<T>(event, handler);
}
