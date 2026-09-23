import { createContext, useContext } from "react";
import type { Config, ConfigSection, ConfigValue, Permission } from "../shared/api";

export interface WizardContextValue {
  config: Config;
  /** Write one key and mirror it locally. */
  save: (section: ConfigSection, key: string, value: ConfigValue) => Promise<void>;
  patch: (section: ConfigSection, key: string, value: ConfigValue) => void;
  /** The configured shortcut, or the one the engine reports while nothing is configured. */
  hotkey: string;
  permissions: Permission[];
  refreshPermissions: () => Promise<void>;
}

export const WizardContext = createContext<WizardContextValue | null>(null);

export function useWizard(): WizardContextValue {
  const ctx = useContext(WizardContext);
  if (!ctx) throw new Error("useWizard outside <WizardContext>");
  return ctx;
}
