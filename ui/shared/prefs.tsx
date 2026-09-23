// What the settings window and the first-run wizard have in common: the
// config store, and the controls that wrap one Rust command each.

import { useEffect, useState } from "react";
import { api, type Config, type ConfigSection, type ConfigValue, type DownloadEvent, type GpuReport, type InputDevice } from "./api";
import { describe, useAsync, useEvent } from "./hooks";
import { Button, Progress, Switch, formatBytes, useToast, type Option } from "./ui";

// -- config ------------------------------------------------------------------

export interface ConfigStore {
  config: Config | null;
  error: string | null;
  /** Mirror a value locally without writing it (for values Rust wrote itself). */
  patch: (section: ConfigSection, key: string, value: ConfigValue) => void;
  /** Write one key and mirror it locally. Toasts and rethrows on failure. */
  save: (section: ConfigSection, key: string, value: ConfigValue) => Promise<void>;
}

/** Load config.toml once and keep a local copy in step with every write. */
export function useConfigStore(): ConfigStore {
  const toast = useToast();
  const [config, setConfig] = useState<Config | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.getConfig().then(setConfig, (err) => setError(describe(err)));
  }, []);

  const patch: ConfigStore["patch"] = (section, key, value) => {
    setConfig((c) => (c ? { ...c, [section]: { ...c[section], [key]: value } } : c));
  };

  const save: ConfigStore["save"] = async (section, key, value) => {
    try {
      await api.setConfigValue(section, key, value);
    } catch (err) {
      toast(`Could not save: ${describe(err)}`);
      throw err;
    }
    patch(section, key, value);
  };

  return { config, error, patch, save };
}

/** The page's "could not load" screen; the caller supplies the padding. */
export function LoadFailed({ title, error }: { title: string; error: string }) {
  return (
    <>
      <h1 className="text-[17px] font-semibold">{title}</h1>
      <p className="mt-1 text-fg-2">{error}</p>
      <Button className="mt-4" onClick={() => location.reload()}>
        Try again
      </Button>
    </>
  );
}

// -- cleanup provider keys ---------------------------------------------------

/**
 * Where one provider's API key comes from ("keyring", "not set", "$VAR" or ""
 * while unknown), and a way to replace it. Pass null to leave the keyring alone.
 */
export function useApiKey(provider: string | null) {
  const toast = useToast();
  const [source, setSource] = useState("");

  const load = (p: string) => api.getKeySource(p).then(setSource, () => setSource(""));

  useEffect(() => {
    if (provider !== null) void load(provider);
  }, [provider]);

  /** Store the key, or remove it when empty. Resolves to whether a key is now stored. */
  const apply = async (key: string): Promise<boolean> => {
    if (provider === null) return false;
    const trimmed = key.trim();
    try {
      if (trimmed) {
        // Keys go to the OS keyring, never into config.toml.
        await api.setApiKey(provider, trimmed);
        toast("Saved to the keyring");
      } else {
        await api.clearApiKey(provider);
        toast("API key removed");
      }
      await load(provider);
      return trimmed !== "";
    } catch (err) {
      toast(`Could not write to the keyring: ${describe(err)}`);
      return false;
    }
  };

  return { source, fromEnv: source.startsWith("$"), apply };
}

// -- where recognition runs ---------------------------------------------------

export interface ComputePlan {
  /** Whether the encoder will run on the graphics card. */
  onGpu: boolean;
  /** One sentence: where recognition runs and, if not on the card, why. */
  headline: string;
  /** What would let it use the card, when something would. */
  fix: string | null;
}

/** The provider setting as the Select shows it; "cuda" is gpu's old name. */
export function providerChoice(provider: string): string {
  return provider === "cuda" ? "gpu" : provider;
}

/** Where recognition will run for this provider setting, from the GPU check. */
export function computePlan(gpu: GpuReport, provider: string): ComputePlan {
  const card = gpu.gpu;
  const why = gpu.problem ?? "unknown reason";
  if (provider === "cpu") {
    return {
      onGpu: false,
      headline: gpu.usable && card ? `Runs on the CPU. The ${card} is ready if you switch to Automatic.` : "Runs on the CPU.",
      fix: null,
    };
  }
  if (gpu.usable && card) return { onGpu: true, headline: `Runs on the ${card}.`, fix: null };
  if (providerChoice(provider) === "gpu") {
    // Forced: a card with nothing to fix (an integrated one) is used anyway.
    if (card && gpu.backend && !gpu.fix) return { onGpu: true, headline: `Runs on the ${card}, as set, though ${why}.`, fix: null };
    return { onGpu: false, headline: `Set to GPU only, but ${card ? `the ${card} can't be used` : "there is no usable GPU"}: ${why}.`, fix: gpu.fix };
  }
  if (card) return { onGpu: false, headline: `Runs on the CPU. The ${card} is not used: ${why}.`, fix: gpu.fix };
  return { onGpu: false, headline: `Runs on the CPU, still well ahead of your speech (${gpu.problem ?? "no graphics card found"}).`, fix: gpu.fix };
}

/** "671 MB", "2.6 GB". */
export function formatMegabytes(mb: number): string {
  return mb >= 1000 ? `${(mb / 1000).toFixed(1)} GB` : `${Math.round(mb)} MB`;
}

// -- recognition model download ---------------------------------------------

interface DownloadHandlers {
  onDone: () => void;
  onError: (message: string) => void;
}

/** Start and follow the download of one recognition model over `flow:download`. */
export function useModelDownload(id: string, handlers: DownloadHandlers) {
  const [progress, setProgress] = useState<DownloadEvent | null>(null);

  useEvent("flow:download", (ev) => {
    if (ev.id !== id) return;
    if (ev.error) {
      setProgress(null);
      handlers.onError(ev.error);
    } else if (ev.done) {
      setProgress(null);
      handlers.onDone();
    } else {
      setProgress(ev);
    }
  });

  const start = async () => {
    setProgress({ id, file: "", received: 0, total: 0, done: false });
    try {
      await api.downloadModel(id);
    } catch (err) {
      setProgress(null);
      handlers.onError(describe(err));
    }
  };

  return { progress: progress?.id === id ? progress : null, start };
}

/** A progress bar with the file being fetched and the byte count under it. */
export function DownloadProgress({ event, className }: { event: DownloadEvent; className?: string }) {
  return (
    <div className={"flex flex-col gap-1.5 " + (className ?? "")}>
      <Progress value={event.total > 0 ? event.received / event.total : null} label="Download progress" />
      <div className="flex justify-between text-[12.5px] text-fg-2">
        <span className="truncate font-mono">{event.file || "Starting…"}</span>
        {event.total > 0 && (
          <span>
            {formatBytes(event.received)} of {formatBytes(event.total)}
          </span>
        )}
      </div>
    </div>
  );
}

// -- desktop -----------------------------------------------------------------

/** The "launch at login" switch, which lives outside config.toml. */
export function AutostartSwitch({ id, onSaved }: { id?: string; onSaved?: (on: boolean) => void }) {
  const toast = useToast();
  const autostart = useAsync(() => api.getAutostart());

  const change = async (on: boolean) => {
    autostart.set(on);
    try {
      await api.setAutostart(on);
      onSaved?.(on);
    } catch (err) {
      autostart.set(!on);
      toast(`Could not change autostart: ${describe(err)}`);
    }
  };

  return <Switch id={id} checked={autostart.data ?? false} disabled={autostart.loading} onChange={(on) => void change(on)} />;
}

// -- audio -------------------------------------------------------------------

/** Input devices as select options; `current` is kept in the list even when it is unplugged. */
export function deviceOptions(devices: InputDevice[] | null, current = ""): Option[] {
  const opts: Option[] = [{ value: "", label: "System default" }];
  for (const d of devices ?? []) opts.push({ value: d.name, label: d.is_default ? `${d.name} (default)` : d.name });
  if (current && !opts.some((o) => o.value === current)) opts.push({ value: current, label: `${current} (not connected)` });
  return opts;
}
