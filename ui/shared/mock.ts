// A stand-in for the Rust side, used whenever the page runs outside Tauri.
//
// Every command in api.ts is answered here with plausible data, and the
// overlay events are emitted on a timer so the pill can be watched in a
// normal browser tab. `?state=listening&text=...` on the overlay page pins
// one state instead of cycling, which is how the screenshots are taken;
// `?running=0` starts with the engine off.

import type {
  Config,
  ConfigSection,
  ConfigValue,
  DoctorCheck,
  DownloadEvent,
  GpuReport,
  InputDevice,
  LearningSummary,
  Permission,
  ProviderSpec,
  Status,
  SttModel,
  UpdateCheck,
} from "./api";
import type { Handler, Unlisten } from "./bridge";

const log = (...args: unknown[]) => console.debug("[flow mock]", ...args);
const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

// -- event bus ---------------------------------------------------------------

const handlers = new Map<string, Set<Handler<unknown>>>();

function emit(event: string, payload: unknown): void {
  const set = handlers.get(event);
  if (!set) return;
  for (const h of set) h(payload);
}

export async function listen<T>(event: string, handler: Handler<T>): Promise<Unlisten> {
  let set = handlers.get(event);
  if (!set) {
    set = new Set();
    handlers.set(event, set);
  }
  set.add(handler as Handler<unknown>);
  ensureTimers();
  return () => {
    set?.delete(handler as Handler<unknown>);
  };
}

// -- state -------------------------------------------------------------------

const params = new URLSearchParams(typeof location !== "undefined" ? location.search : "");
const pinnedState = params.get("state");
const pinnedText = params.get("text") ?? "";

const config: Config = {
  audio: { device: "", sample_rate: 16000, silence_rms: 0.006, trim_silence: true, min_seconds: 0.35 },
  stt: { model: "nemo-parakeet-tdt-0.6b-v3", provider: "auto" },
  cleanup: {
    enabled: true,
    backend: "ollama",
    model: "qwen3:14b",
    endpoint: "http://localhost:11434",
    base_url: "",
    timeout: 20,
    keep_alive: "1h",
    style: "balanced",
    resolve_intent: true,
    think: "never",
    languages: ["en", "nl"],
    output_language: "same",
    dictionary: [],
    app_rules: {},
  },
  learning: { enabled: false, min_dictations: 15, refresh_every: 25, max_terms: 40 },
  desktop: { overlay: "auto", hotkey: "Ctrl+Alt+Space", push_to_talk: true, check_updates: true },
};

let running = params.get("running") !== "0";
let needsRestart = false;
let engineError: string | null = null;
let hotkey = config.desktop.hotkey;
let overlayState = "hidden";
let speaking = false;
let autostart = false;
const keys = new Map<string, string>();
const envKeys = new Set<string>(["openrouter"]); // pretend $OPENROUTER_API_KEY is exported
const downloaded = new Set<string>(["nemo-parakeet-tdt-0.6b-v3"]);
let download: DownloadEvent | null = null;
let cancelDownload = false;
// `?update=none` (up to date), `package` (a .deb: no self-update) or the
// default, a newer version this copy can install.
const updateMode = params.get("update") ?? "install";

const permissions: Permission[] = [
  {
    id: "hotkey-gnome-extension",
    label: "Flow GNOME Shell extension",
    granted: false,
    required: true,
    help: "Flow's Shell extension draws the island, hears the shortcut and pastes the text. Installing it takes effect after you log out and back in.",
  },
  {
    id: "accessibility",
    label: "Accessibility",
    granted: true,
    required: true,
    help: "Needed to type into the focused app.",
  },
  {
    id: "screen-recording",
    label: "Screen Recording",
    granted: false,
    required: false,
    help: "Only used to read the focused window's title so the cleanup model can match the app's tone. Optional.",
  },
];

const providers: ProviderSpec[] = [
  {
    key: "ollama",
    label: "Ollama (local)",
    needs_api_key: false,
    default_model: "qwen3:14b",
    suggested_models: ["qwen3:14b", "qwen3:8b", "qwen3:4b", "llama3.1:8b", "mistral-nemo:12b", "gemma3:12b", "phi4:14b"],
    note: "Runs on this machine. Nothing leaves it.",
    has_base_url: false,
  },
  {
    key: "anthropic",
    label: "Anthropic Claude",
    needs_api_key: true,
    default_model: "claude-opus-5",
    suggested_models: ["claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"],
    note: "",
    has_base_url: false,
  },
  {
    key: "openai",
    label: "OpenAI",
    needs_api_key: true,
    default_model: "gpt-5",
    suggested_models: ["gpt-5", "gpt-5-mini", "gpt-4.1", "gpt-4.1-mini"],
    note: "",
    has_base_url: false,
  },
  {
    key: "openrouter",
    label: "OpenRouter",
    needs_api_key: true,
    default_model: "deepseek/deepseek-v4.1-flash",
    suggested_models: ["deepseek/deepseek-v4.1-flash"],
    note: "Hundreds of models from one key. The full list loads below.",
    has_base_url: false,
  },
  {
    key: "deepseek",
    label: "DeepSeek",
    needs_api_key: true,
    default_model: "deepseek-chat",
    suggested_models: ["deepseek-chat", "deepseek-reasoner"],
    note: "",
    has_base_url: false,
  },
  {
    key: "custom",
    label: "Other (OpenAI-compatible)",
    needs_api_key: true,
    default_model: "",
    suggested_models: [],
    note: "Any OpenAI-compatible endpoint: Groq, Together, Fireworks, vLLM, llama.cpp, LM Studio.",
    has_base_url: true,
  },
  {
    key: "none",
    label: "No cleanup (raw transcript)",
    needs_api_key: false,
    default_model: "",
    suggested_models: [],
    note: "Paste the transcript exactly as recognised.",
    has_base_url: false,
  },
];

const providerModels: Record<string, string[]> = {
  ollama: ["gemma3:12b", "llama3.1:8b", "qwen3:14b", "qwen3:8b"],
  anthropic: ["claude-haiku-4-5", "claude-opus-5", "claude-sonnet-5"],
  openai: ["gpt-4.1", "gpt-4.1-mini", "gpt-5", "gpt-5-mini", "o4-mini"],
  openrouter: [
    "anthropic/claude-sonnet-5",
    "deepseek/deepseek-v4.1-flash",
    "deepseek/deepseek-r2",
    "google/gemini-3-flash",
    "meta-llama/llama-4-maverick",
    "mistralai/mistral-medium-3",
    "openai/gpt-5-mini",
    "qwen/qwen3-235b-a22b",
    "x-ai/grok-4",
  ],
  deepseek: ["deepseek-chat", "deepseek-reasoner"],
  custom: ["llama-3.3-70b-versatile", "qwen-2.5-32b"],
};

const sttModels: Omit<SttModel, "downloaded" | "download_mb" | "precision">[] = [
  { id: "nemo-parakeet-tdt-0.6b-v3", label: "Parakeet TDT v3", description: "English and Dutch, detected automatically" },
  { id: "nemo-parakeet-tdt-0.6b-v2", label: "Parakeet TDT v2", description: "English only, marginally better on English" },
];

const gpu: GpuReport = {
  backend: "webgpu",
  devices: [
    { name: "NVIDIA GeForce RTX 4070", vendor: "nvidia", kind: "discrete", memory_mb: 12282, compute: "8.9", driver: "nvidia" },
    { name: "Intel(R) UHD Graphics 770", vendor: "intel", kind: "integrated", memory_mb: null, compute: null, driver: "i915" },
  ],
  gpu: "NVIDIA GeForce RTX 4070 (12 GB)",
  driver: "580.159.03, CUDA 13.0",
  usable: true,
  problem: null,
  fix: null,
  loaded_from: [],
};

const devices: InputDevice[] = [
  { name: "Built-in Audio Analog Stereo", channels: 2, default_rate: 48000, is_default: true },
  { name: "Blue Yeti Nano", channels: 2, default_rate: 48000, is_default: false },
  { name: "WH-1000XM5 (headset)", channels: 1, default_rate: 16000, is_default: false },
];

let learning: LearningSummary = {
  enabled: false,
  count: 42,
  terms: ["Flow", "Parakeet", "Mutter", "KVK", "Tauri", "libadwaita", "onnxruntime"],
  style: "Short sentences, first person, drops greetings, mixes English and Dutch.",
};

// -- commands ----------------------------------------------------------------

type Args = Record<string, unknown>;

/** Like the engine: it reads its config once, when it starts. */
function start(): void {
  needsRestart = false;
  if (!downloaded.has(config.stt.model)) {
    running = false;
    engineError = `the recognition model ${config.stt.model} is not downloaded`;
    throw new Error(engineError);
  }
  running = true;
  engineError = null;
}

/** Pretend to record for `ms`, so the level meters show speech meanwhile. */
async function speak<T>(ms: number, result: T): Promise<T> {
  speaking = true;
  await sleep(ms);
  speaking = false;
  return result;
}

const commands: Record<string, (args: Args) => unknown> = {
  overlay_ready: () => undefined,
  overlay_resize: () => undefined,

  get_config: () => structuredClone(config),
  set_config_value: ({ section, key, value }) => {
    (config[section as ConfigSection] as unknown as Record<string, ConfigValue>)[key as string] = value as ConfigValue;
    if (section === "desktop" && key === "hotkey") hotkey = String(value);
    else if (running) needsRestart = true;
    return needsRestart;
  },
  get_config_path: () => "/home/you/.config/flow/config.toml",
  open_config_file: () => undefined,

  get_status: () =>
    ({
      running,
      state: running ? overlayState : "hidden",
      hotkey,
      version: "0.3.0",
      needs_restart: needsRestart,
      session: "mock",
      error: engineError,
    }) satisfies Status,
  set_running: async ({ on }) => {
    await sleep(400);
    if (on) start();
    else running = false;
  },
  restart_engine: async () => {
    running = false;
    await sleep(900);
    start();
  },
  run_doctor: async () => {
    await sleep(700);
    return [
      { name: "Microphone", ok: true, detail: "Built-in Audio Analog Stereo, 48 kHz" },
      { name: "Recognition model", ok: downloaded.has(config.stt.model), detail: config.stt.model },
      { name: "GPU", ok: true, detail: `${gpu.gpu}, driver ${gpu.driver}` },
      { name: "Cleanup model", ok: true, detail: `${config.cleanup.model} via ${config.cleanup.backend}` },
      { name: "Overlay", ok: true, detail: "Window overlay" },
      { name: "Hotkey", ok: false, detail: `${hotkey} is not bound - grant the global shortcut` },
    ] satisfies DoctorCheck[];
  },
  copy_diagnostics: () =>
    [
      "Flow 0.3.0 (mock)",
      "Linux 6.19 · GNOME 48 · Wayland",
      `stt: ${config.stt.model} / ${config.stt.provider}`,
      `cleanup: ${config.cleanup.backend} / ${config.cleanup.model}`,
      `hotkey: ${hotkey}`,
    ].join("\n"),
  check_for_updates: async () => {
    await sleep(800);
    return updateCheck();
  },
  install_update: () => {
    if (!updateCheck().can_install) throw new Error("Flow came from a .deb package. Install the new .deb from the releases page.");
    void fakeUpdate();
  },
  open_release_page: () => void window.open("https://github.com/ceyhuncakir/flow/releases/latest", "_blank"),

  list_input_devices: () => devices,
  list_stt_models: () => {
    const p = config.stt.provider;
    const onGpu = p === "gpu" || p === "cuda" || (p !== "cpu" && gpu.usable);
    return sttModels.map((m) => ({ ...m, downloaded: downloaded.has(m.id), download_mb: onGpu ? 2550 : 671, precision: onGpu ? "fp32" : "int8" }));
  },
  detect_gpu: async () => {
    await sleep(300);
    return gpu;
  },
  download_model: ({ id }) => {
    if (download) throw new Error(`${download.id} is already downloading`);
    void fakeDownload(String(id));
  },
  get_download: () => download,
  cancel_download: () => void (cancelDownload = true),
  get_compute_report: () => {
    const requested = config.stt.provider;
    const onGpu = requested === "gpu" || requested === "cuda" || (requested !== "cpu" && gpu.usable);
    return { requested, actual: onGpu ? "webgpu" : "cpu", reason: "", fallback: null };
  },

  list_providers: () => providers,
  list_provider_models: async ({ provider }) => {
    await sleep(600);
    const p = String(provider);
    const spec = providers.find((x) => x.key === p);
    if (spec?.needs_api_key && !keys.get(p) && !envKeys.has(p) && p !== "openrouter") return [];
    return providerModels[p] ?? [];
  },
  get_key_source: ({ provider }) => {
    const p = String(provider);
    if (envKeys.has(p)) return `$${p.toUpperCase()}_API_KEY`;
    if (providers.some((x) => x.key === p && !x.needs_api_key)) return "not needed";
    return keys.get(p) ? "keyring" : "not set";
  },
  set_api_key: ({ provider, key }) => {
    // Paste "fail" to see how a keyring refusal looks.
    if (key === "fail") throw new Error("The system keyring refused it. Is a keyring running and unlocked?");
    keys.set(String(provider), String(key));
  },
  clear_api_key: ({ provider }) => void keys.delete(String(provider)),

  get_learning_summary: () => ({ ...learning, enabled: config.learning.enabled }),
  forget_history: () => {
    const removed = learning.count;
    learning = { ...learning, count: 0, terms: [], style: "" };
    return removed;
  },

  set_hotkey: ({ combo }) => {
    hotkey = String(combo);
    config.desktop.hotkey = hotkey;
  },
  get_autostart: () => autostart,
  set_autostart: ({ on }) => void (autostart = Boolean(on)),
  get_permissions: () => permissions.map((p) => ({ ...p })),
  request_permission: async ({ id }) => {
    await sleep(600);
    // The extension only runs after the next login.
    const p = permissions.find((x) => x.id === id);
    if (p && id !== "hotkey-gnome-extension") p.granted = true;
  },

  wizard_test_mic: () => speak(2000, { ok: true, peak: 0.62, detail: "heard you: peak level 62%" }),
  wizard_test_transcribe: () => speak(2600, { text: "Testing one two three, can you hear me?", seconds: 2.4 }),
  wizard_test_paste: async () => {
    await sleep(500);
    const el = document.activeElement;
    const sample = "Flow pasted this. ";
    if (el instanceof HTMLTextAreaElement || el instanceof HTMLInputElement) {
      const start = el.selectionStart ?? el.value.length;
      const end = el.selectionEnd ?? start;
      el.value = el.value.slice(0, start) + sample + el.value.slice(end);
      el.selectionStart = el.selectionEnd = start + sample.length;
      el.dispatchEvent(new Event("input", { bubbles: true }));
      return { ok: true, restored: true, detail: "Typed through the virtual keyboard and put the clipboard back" };
    }
    return { ok: false, restored: true, detail: "No text field had focus" };
  },
  wizard_test_cleanup: async () => {
    await sleep(1200);
    return {
      ok: true,
      sample: "Um so the, the meeting is at three — no wait, at four — and bring the KVK papers.\n→ The meeting is at four; bring the KVK papers.",
    };
  },
  wizard_complete: () => undefined,
};

export async function invoke<T>(cmd: string, args: Args = {}): Promise<T> {
  log(cmd, args);
  const handler = commands[cmd];
  if (!handler) throw new Error(`mock: unknown command ${cmd}`);
  return (await handler(args)) as T;
}

// -- timers: overlay cycle, levels, hotkey ----------------------------------

let timersStarted = false;

function ensureTimers(): void {
  if (timersStarted) return;
  timersStarted = true;

  // Levels: a quiet room with bursts of speech whenever something is
  // "listening", so the meters have something to show.
  // Like the engine, levels only flow while something records.
  let t = 0;
  window.setInterval(() => {
    if (!handlers.get("flow:level")?.size) return;
    if (!speaking && overlayState !== "listening") return;
    t += 0.04;
    const envelope = 0.35 + 0.3 * Math.sin(t * 3.1) * Math.sin(t * 0.7);
    const level = Math.max(0, Math.min(1, envelope + (Math.random() - 0.5) * 0.35));
    emit("flow:level", { level });
  }, 40);

  // Hotkey: down for a second every four while the engine runs, so the
  // wizard's indicator moves.
  let down = false;
  window.setInterval(() => {
    if (!handlers.get("flow:hotkey")?.size || !running) return;
    down = !down;
    emit("flow:hotkey", { down });
    if (down) window.setTimeout(() => emit("flow:hotkey", { down: (down = false) }), 1100);
  }, 4000);

  if (handlers.has("flow:state")) startOverlayCycle();
}

function startOverlayCycle(): void {
  if (pinnedState) {
    window.setTimeout(() => {
      overlayState = pinnedState;
      emit("flow:text", { text: pinnedText });
      emit("flow:state", { state: pinnedState });
    }, 50);
    return;
  }

  const script: Array<[string, string, number]> = [
    ["idle", "", 1600],
    ["listening", "", 3600],
    ["thinking", "", 1800],
    ["inserting", "The meeting moved to four, bring the KVK papers.", 1400],
    ["hidden", "", 1200],
    ["idle", "", 1200],
    ["listening", "", 2600],
    ["thinking", "", 1400],
    ["error", "Ollama is not running", 2500],
    ["hidden", "", 1600],
  ];
  let i = 0;
  const step = () => {
    const [state, text, ms] = script[i % script.length]!;
    i += 1;
    overlayState = state;
    emit("flow:text", { text });
    emit("flow:state", { state });
    window.setTimeout(step, ms);
  };
  window.setTimeout(step, 300);
}

function report(event: DownloadEvent): void {
  download = event.done ? null : event;
  emit("flow:download", event);
}

async function fakeDownload(id: string): Promise<void> {
  const files: Array<[string, number]> = [
    ["nemo128.onnx", 1_400_000],
    ["vocab.txt", 12_000],
    ["encoder-model.onnx", 2_450_000_000],
    ["encoder-model.onnx.data", 60_000_000],
    ["decoder_joint-model.onnx", 18_000_000],
  ];
  cancelDownload = false;
  download = { id, file: "", received: 0, total: 0, done: false, error: null };
  for (const [file, total] of files) {
    const steps = Math.max(2, Math.round(total / 120_000_000));
    for (let s = 1; s <= steps; s++) {
      await sleep(120);
      if (cancelDownload) {
        report({ id, file: "", received: 0, total: 0, done: true, error: "download cancelled" });
        return;
      }
      report({ id, file, received: Math.round((total * s) / steps), total, done: false, error: null });
    }
  }
  downloaded.add(id);
  report({ id, file: "", received: 0, total: 0, done: true, error: null });
}

function updateCheck(): UpdateCheck {
  const available = updateMode !== "none";
  const packaged = updateMode === "package";
  return {
    available,
    current: "0.3.0",
    version: available ? "0.4.0" : null,
    notes: available ? "See the changelog for what changed." : null,
    date: available ? "2026-09-21T10:00:00Z" : null,
    install: packaged ? "deb" : "appimage",
    can_install: !packaged,
    how: packaged ? "Flow came from a .deb package. Install the new .deb from the releases page, or update it the way you installed it." : null,
    release_url: "https://github.com/ceyhuncakir/flow/releases/latest",
  };
}

async function fakeUpdate(): Promise<void> {
  const total = 96_000_000;
  for (let received = 0; received < total; received += 8_000_000) {
    emit("flow:update", { received, total, done: false, error: null });
    await sleep(150);
  }
  emit("flow:update", { received: total, total, done: true, error: null });
  log("install_update: Flow would restart into 0.4.0 now");
}
