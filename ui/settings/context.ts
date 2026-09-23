import { createContext, useContext } from "react";
import type { Config, ConfigSection, ConfigValue, Status } from "../shared/api";

export interface SettingsContextValue {
  config: Config;
  status: Status | null;
  /** Write one key and mirror it locally. Toasts "Saved" unless told otherwise; toasts and rethrows on failure. */
  save: (section: ConfigSection, key: string, value: ConfigValue, toast?: string | false) => Promise<void>;
  /** Patch the local copy without writing (for values Rust wrote itself). */
  patch: (section: ConfigSection, key: string, value: ConfigValue) => void;
  refreshStatus: () => Promise<void>;
}

export const SettingsContext = createContext<SettingsContextValue | null>(null);

export function useSettings(): SettingsContextValue {
  const ctx = useContext(SettingsContext);
  if (!ctx) throw new Error("useSettings outside <SettingsContext>");
  return ctx;
}
