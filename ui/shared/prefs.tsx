// What the settings window and the first-run wizard have in common: the
// config store, and the controls that wrap one Rust command each.

import { useEffect, useRef, useState } from "react";
import { api, type Config, type ConfigSection, type ConfigValue, type DownloadEvent, type GpuReport, type InputDevice } from "./api";
import { describe, useAsync, useEvent } from "./hooks";
import { Button, Progress, Switch, formatBytes, useToast, type Choice, type Option } from "./ui";

// -- config ------------------------------------------------------------------

export interface ConfigStore {
  config: Config | null;
  error: string | null;
  /** Mirror a value locally without writing it (for values Rust wrote itself). */
  patch: (section: ConfigSection, key: string, value: ConfigValue) => void;
  /**
   * Write one key and mirror it locally. Resolves to whether the engine must
   * restart for it to apply. Toasts and rethrows on failure.
   */
  save: (section: ConfigSection, key: string, value: ConfigValue) => Promise<boolean>;
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
    let restart: boolean;
    try {
      restart = await api.setConfigValue(section, key, value);
    } catch (err) {
      toast(`Could not save: ${describe(err)}`);
      throw err;
    }
    patch(section, key, value);
    return restart === true;
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
 * `error` is why the keyring last refused, for showing beside the field: it
 * is too long for a toast.
 */
export function useApiKey(provider: string | null) {
  const toast = useToast();
  const [source, setSource] = useState("");
  const [error, setError] = useState<string | null>(null);

  const load = (p: string) => api.getKeySource(p).then(setSource, () => setSource(""));

  useEffect(() => {
    setError(null);
    if (provider !== null) void load(provider);
  }, [provider]);

  /**
   * Store the key, or remove it when empty. Resolves to whether a key is now
   * stored; rejects when the keyring refuses, with `error` set.
   */
  const apply = async (key: string): Promise<boolean> => {
    if (provider === null) return false;
    const trimmed = key.trim();
    setError(null);
    try {
      // Keys go to the OS keyring, never into config.toml.
      if (trimmed) await api.setApiKey(provider, trimmed);
      else await api.clearApiKey(provider);
    } catch (err) {
      setError(describe(err));
      throw err;
    }
    toast(trimmed ? "Saved to the keyring" : "API key removed");
    await load(provider);
    return trimmed !== "";
  };

  return { source, fromEnv: source.startsWith("$"), error, apply };
}

// -- cleanup models -------------------------------------------------------------

/**
 * The models a cleanup provider offers: its suggestions at once, then its own
 * list once it answers (OpenRouter alone offers hundreds, so no hardcoded list
 * stays right). The configured model is always among the choices, and any
 * other name can still be typed.
 */
export function useProviderModels(provider: string, suggested: readonly string[], current: string) {
  const [fetched, setFetched] = useState<string[] | null>(null);
  const [fetching, setFetching] = useState(false);
  // A slow answer for a provider the user has since left must not win.
  const asked = useRef(0);

  const refresh = async () => {
    const id = ++asked.current;
    setFetched(null);
    if (provider === "none") return;
    setFetching(true);
    try {
      const found = await api.listProviderModels(provider);
      if (id === asked.current) setFetched(found.length ? found : null);
    } catch (err) {
      console.warn("list_provider_models failed", err);
    } finally {
      if (id === asked.current) setFetching(false);
    }
  };

  useEffect(() => {
    void refresh();
  }, [provider]);

  const list = fetched ?? [...suggested];
  const choices = current && !list.includes(current) ? [current, ...list] : list;
  return { choices, fetched, fetching, refresh };
}

// -- where recognition runs ---------------------------------------------------

export const COMPUTE_CHOICES: readonly Choice[] = [
  { value: "auto", label: "Automatic", description: "Use the graphics card when it is faster than the CPU" },
  { value: "gpu", label: "GPU only", description: "Use the graphics card even when it is slower, and fail without one" },
  { value: "cpu", label: "CPU only", description: "Slower, but leaves the graphics card free and needs the smaller download" },
];

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
  onDone: (id: string) => void;
  onError: (message: string) => void;
}

/**
 * Start, follow and cancel the recognition model download over
 * `rustle:download`. Only one runs at a time, whichever window started it, and
 * one already under way when this mounts is picked up where it is.
 */
export function useModelDownload(handlers: DownloadHandlers) {
  const [progress, setProgress] = useState<DownloadEvent | null>(null);
  const [cancelling, setCancelling] = useState(false);
  // Read by the event handler, which can run before a re-render.
  const cancelled = useRef(false);

  useEffect(() => {
    let live = true;
    api.getDownload().then(
      (ev) => {
        // An event that arrived meanwhile is newer.
        if (live && ev && !ev.done && !ev.error) setProgress((p) => p ?? ev);
      },
      (err) => console.warn("get_download failed", err),
    );
    return () => {
      live = false;
    };
  }, []);

  useEvent("rustle:download", (ev) => {
    if (!ev.done && !ev.error) {
      setProgress(ev);
      return;
    }
    const stopped = cancelled.current || /cancel/i.test(ev.error ?? "");
    cancelled.current = false;
    setProgress(null);
    setCancelling(false);
    if (!ev.error) handlers.onDone(ev.id);
    else if (!stopped) handlers.onError(ev.error);
  });

  const start = async (id: string) => {
    cancelled.current = false;
    setProgress({ id, file: "", received: 0, total: 0, done: false, error: null });
    try {
      await api.downloadModel(id);
    } catch (err) {
      setProgress(null);
      handlers.onError(describe(err));
    }
  };

  /** Stop it; the progress stays until the download says it has stopped. */
  const cancel = async () => {
    cancelled.current = true;
    setCancelling(true);
    try {
      await api.cancelDownload();
    } catch (err) {
      cancelled.current = false;
      setCancelling(false);
      handlers.onError(describe(err));
    }
  };

  return { progress, cancelling, start, cancel };
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
