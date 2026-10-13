import { createContext, useContext } from "react";
import type { Config, ConfigSection, ConfigValue, Permission, Status } from "../shared/api";

export interface WizardContextValue {
  config: Config;
  /** Write one key and mirror it locally. Toasts and rethrows on failure. */
  save: (section: ConfigSection, key: string, value: ConfigValue) => Promise<boolean>;
  patch: (section: ConfigSection, key: string, value: ConfigValue) => void;
  /** The config with every write asked for applied, landed or not; see ConfigStore.latest. */
  latest: () => Config;
  /** The shortcut dictation listens for, as the engine reports it; the configured one until then. */
  hotkey: string;
  /** The engine, which the wizard starts at the Shortcut step; null until known. */
  status: Status | null;
  /** Whether the wizard is starting or restarting the engine right now. */
  starting: boolean;
  refreshStatus: () => Promise<void>;
  permissions: Permission[];
  /** Read the list again; resolves to it, or null when it could not be read. */
  refreshPermissions: () => Promise<Permission[] | null>;
  /** Whether the configured recognition model is on disk; null until known. */
  modelReady: boolean | null;
}

export const WizardContext = createContext<WizardContextValue | null>(null);

export function useWizard(): WizardContextValue {
  const ctx = useContext(WizardContext);
  if (!ctx) throw new Error("useWizard outside <WizardContext>");
  return ctx;
}
