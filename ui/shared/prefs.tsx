// What the settings window and the first-run wizard have in common: the
// config store, and the controls that wrap one Rust command each.

import { useEffect, useRef, useState } from "react";
import { api, type Config, type ConfigSection, type ConfigValue, type DownloadEvent, type GpuReport, type InputDevice, type Permission } from "./api";
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
   *
   * Writes reach config.toml one at a time, in the order they were asked
   * for: Tauri runs commands side by side, and two quick changes of provider
   * would otherwise land backend and model in any order.
   */
  save: (section: ConfigSection, key: string, value: ConfigValue) => Promise<boolean>;
  /**
   * The config with every write asked for so far applied, including those
   * still on their way. A choice that depends on another value (a provider
   * and its model) reads this, not `config`, which only has the writes that
   * have landed, and asks for both writes in the same tick.
   */
  latest: () => Config | null;
}

function withValue(c: Config, section: ConfigSection, key: string, value: ConfigValue): Config {
  return { ...c, [section]: { ...c[section], [key]: value } };
}

/** Load config.toml once and keep a local copy in step with every write. */
export function useConfigStore(): ConfigStore {
  const toast = useToast();
  const [config, setConfig] = useState<Config | null>(null);
  const [error, setError] = useState<string | null>(null);
  const intended = useRef<Config | null>(null);
  const queue = useRef<Promise<unknown>>(Promise.resolve());

  const load = () =>
    api.getConfig().then(
      (c) => {
        intended.current = c;
        setConfig(c);
      },
      (err) => setError(describe(err)),
    );

  useEffect(() => {
    void load();
  }, []);

  const patch: ConfigStore["patch"] = (section, key, value) => {
    if (intended.current) intended.current = withValue(intended.current, section, key, value);
    setConfig((c) => (c ? withValue(c, section, key, value) : c));
  };

  const save: ConfigStore["save"] = async (section, key, value) => {
    if (intended.current) intended.current = withValue(intended.current, section, key, value);
    const write = queue.current.then(() => api.setConfigValue(section, key, value));
    queue.current = write.catch(() => {});
    let restart: boolean;
    try {
      restart = await write;
    } catch (err) {
      toast(`Could not save: ${describe(err)}`);
      // What was asked for no longer says what is on disk; read it again.
      void load();
      throw err;
    }
    // Writes land in order, so the last one asked for is the last applied.
    setConfig((c) => (c ? withValue(c, section, key, value) : c));
    return restart === true;
  };

  return { config, error, patch, save, latest: () => intended.current };
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
  // The keyring can be slow; an answer about a provider the user has since
  // left must not show beside the new one.
  const asked = useRef(0);

  const load = (p: string) => {
    const id = ++asked.current;
    return api.getKeySource(p).then(
      (s) => {
        if (id === asked.current) setSource(s);
      },
      () => {
        if (id === asked.current) setSource("");
      },
    );
  };

  useEffect(() => {
    setError(null);
    setSource("");
    asked.current += 1;
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
    // A fetch still out for the last provider will not clear it any more.
    if (provider === "none") {
      setFetching(false);
      return;
    }
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

// -- permissions ---------------------------------------------------------------

/**
 * The button beside each permission Rustle cannot simply ask the OS for. The
 * GNOME extension is installed by Rustle and only runs after the next login;
 * the paste helper is installed by the user, so its button just looks again.
 * Anything else asks Rust to request it.
 */
const PERMISSION_ACTIONS: Record<string, string> = {
  "hotkey-gnome-extension": "Install",
  "paste-tool": "Check again",
};

/** Granted once the user logs out and back in. */
const NEEDS_LOGIN = new Set(["hotkey-gnome-extension"]);

/** Nothing to request: the user installs it, the button only looks again. */
const CHECK_ONLY = new Set(["paste-tool"]);

/**
 * Request a permission, then look at the list again whatever came of it: a
 * request that failed can still find the thing in place (a key binding
 * pressed meanwhile, a helper installed), and "Check again" only looks.
 * `refresh` resolves to the new list, or null when it could not be read.
 */
export function usePermissionGrant(refresh: () => Promise<Permission[] | null>) {
  const toast = useToast();
  const [busy, setBusy] = useState<string | null>(null);
  const [awaitingLogin, setAwaitingLogin] = useState<ReadonlySet<string>>(new Set());

  const grant = async (id: string) => {
    setBusy(id);
    let failure: string | null = null;
    try {
      if (!CHECK_ONLY.has(id)) await api.requestPermission(id);
      if (NEEDS_LOGIN.has(id)) {
        setAwaitingLogin((s) => new Set(s).add(id));
        toast("Installed. Log out and back in to finish.");
      }
    } catch (err) {
      failure = describe(err);
    }
    const now = (await refresh())?.find((p) => p.id === id);
    setBusy(null);
    if (now?.granted) return;
    if (failure) toast(`Could not ${NEEDS_LOGIN.has(id) ? "install" : "request"} it: ${failure}`);
    else if (CHECK_ONLY.has(id)) toast(`Still not found: ${now?.help ?? "install it, then check again"}`);
  };

  return {
    busy,
    grant,
    /** Installed, and waiting for the next login to take effect. */
    pending: (p: Permission) => !p.granted && awaitingLogin.has(p.id),
    actionLabel: (id: string) => PERMISSION_ACTIONS[id] ?? "Grant",
  };
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
