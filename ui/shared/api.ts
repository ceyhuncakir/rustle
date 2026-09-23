// The command and event contract between the pages and the Rust side.
//
// Every command goes through `invoke` in bridge.ts; every type here mirrors
// the JSON the Tauri commands return. Keep this file in sync with
// src-tauri: it is the one place the two halves agree on names.

import { invoke, listen, type Unlisten } from "./bridge";

// -- config (crates/flow-core/src/config.rs) ---------------------------------

export interface AudioConfig {
  /** Input device name substring; empty means the system default. */
  device: string;
  sample_rate: number;
  silence_rms: number;
  trim_silence: boolean;
  min_seconds: number;
}

export interface SttConfig {
  model: string;
  /** auto | gpu | cpu ("cuda" is the old name for gpu) */
  provider: string;
}

export interface CleanupConfig {
  enabled: boolean;
  /** ollama | anthropic | openai | openrouter | deepseek | custom | none */
  backend: string;
  model: string;
  endpoint: string;
  base_url: string;
  timeout: number;
  keep_alive: string;
  /** light | balanced | tidy */
  style: string;
  resolve_intent: boolean;
  /** auto | never | always */
  think: string;
  languages: string[];
  /** same | en | nl */
  output_language: string;
  dictionary: string[];
  app_rules: Record<string, string>;
}

export interface LearningConfig {
  enabled: boolean;
  min_dictations: number;
  refresh_every: number;
  max_terms: number;
}

export interface DesktopConfig {
  /** auto | window | off */
  overlay: string;
  hotkey: string;
  push_to_talk: boolean;
  /** Ask GitHub once a day whether a newer release is out (release installs only). */
  check_updates: boolean;
}

export interface Config {
  audio: AudioConfig;
  stt: SttConfig;
  cleanup: CleanupConfig;
  learning: LearningConfig;
  desktop: DesktopConfig;
}

export type ConfigSection = keyof Config;
export type ConfigValue = boolean | number | string | string[];

// -- command payloads --------------------------------------------------------

export interface Status {
  /** False when stopped, and when the engine died (see `error`). */
  running: boolean;
  state: string;
  /** The effective dictation shortcut in the plugin's spelling ("Ctrl+Alt+Space"), on every desktop. */
  hotkey: string;
  version: string;
  /** A saved setting only applies once the engine restarts. */
  needs_restart: boolean;
  session: string;
  /** Why the engine is not working, e.g. the model failed to load. */
  error: string | null;
}

export interface InputDevice {
  name: string;
  channels: number;
  default_rate: number;
  is_default: boolean;
}

export interface SttModel {
  id: string;
  label: string;
  description: string;
  /** Whether the files the configured provider will load are on disk. */
  downloaded: boolean;
  /** Their download size, in MB. */
  download_mb: number;
  /** int8 for the CPU, fp32 for the GPU */
  precision: string;
}

// -- graphics card (crates/flow-stt/src/gpu.rs) --------------------------------

export interface GpuDevice {
  name: string;
  /** nvidia | amd | intel | apple | other */
  vendor: string;
  /** discrete | integrated, when a driver says */
  kind: string | null;
  /** Video memory in MiB, when a driver says. */
  memory_mb: number | null;
  /** CUDA compute capability such as "8.9", NVIDIA cards only. */
  compute: string | null;
  /** The kernel driver bound to the card on Linux. */
  driver: string | null;
}

export interface GpuReport {
  /** This build's GPU backend: cuda | webgpu, or null for a CPU-only build. */
  backend: string | null;
  /** Every graphics card found, the one recognition would use first. */
  devices: GpuDevice[];
  /** The card CUDA would run on, e.g. "NVIDIA GeForce RTX 4090 (24 GB)". */
  gpu: string | null;
  /** NVIDIA's driver version and the newest CUDA it supports. */
  driver: string | null;
  /** Whether recognition runs on `gpu` when the provider is auto. */
  usable: boolean;
  /** Why not, e.g. "cuDNN 9 is not installed". */
  problem: string | null;
  /** What would change that, when something can. */
  fix: string | null;
  /** CUDA libraries loaded from outside the loader's search path. */
  loaded_from: string[];
}

export interface ComputeReport {
  /** auto | gpu | cpu, as configured */
  requested: string;
  /** cuda | webgpu | cpu, what actually runs */
  actual: string;
  /** Why `actual` differs from `requested`, or empty. */
  reason: string;
  /** What the CPU has had to do for a failing GPU during dictation, or null. */
  fallback: string | null;
}

export interface ProviderSpec {
  key: string;
  label: string;
  needs_api_key: boolean;
  default_model: string;
  suggested_models: string[];
  note: string;
  has_base_url: boolean;
}

export interface LearningSummary {
  enabled: boolean;
  count: number;
  terms: string[];
  style: string;
}

export interface DoctorCheck {
  name: string;
  ok: boolean;
  detail: string;
}

/** What `check_for_updates` returns (src-tauri/src/updates.rs). */
export interface UpdateCheck {
  available: boolean;
  /** The running version. */
  current: string;
  /** The newer version, when there is one. */
  version: string | null;
  notes: string | null;
  /** When it was published (RFC 3339), when the release says. */
  date: string | null;
  /** appimage | appimagereadonly | deb | rpm | nsis | msi | app | source | dev */
  install: string;
  /** Whether "Install and restart" can replace this copy. */
  can_install: boolean;
  /** When it cannot: how to update instead. */
  how: string | null;
  release_url: string;
}

export interface Permission {
  id: string;
  label: string;
  granted: boolean;
  required: boolean;
  help: string;
}

export interface WizardMicResult {
  ok: boolean;
  /** Peak level heard during the test, 0..1. */
  peak?: number;
  detail?: string;
}

export interface WizardTranscribeResult {
  text: string;
  seconds: number;
}

export interface WizardPasteResult {
  ok: boolean;
  /** Whether the clipboard was put back afterwards. */
  restored: boolean;
  detail: string;
}

export interface WizardCleanupResult {
  ok: boolean;
  sample: string;
}

// -- events ------------------------------------------------------------------

export interface StateEvent {
  state: string;
}
export interface TextEvent {
  text: string;
}
/** To every window while the engine records, and during `wizard_test_mic`. */
export interface LevelEvent {
  level: number;
}
/** Also what `get_download` returns for the download in progress. */
export interface DownloadEvent {
  id: string;
  file: string;
  received: number;
  total: number;
  done: boolean;
  error?: string | null;
}
/** To every window, for each press and release of the shortcut while the engine runs. */
export interface HotkeyEvent {
  down: boolean;
}
/** While `install_update` downloads; `done` with no error means Flow restarts now. */
export interface UpdateEvent {
  received: number;
  /** 0 while unknown. */
  total: number;
  done: boolean;
  error?: string | null;
}

export interface EventPayloads {
  "flow:state": StateEvent;
  "flow:text": TextEvent;
  "flow:level": LevelEvent;
  "flow:download": DownloadEvent;
  "flow:hotkey": HotkeyEvent;
  "flow:update": UpdateEvent;
}

export function on<E extends keyof EventPayloads>(event: E, handler: (payload: EventPayloads[E]) => void): Promise<Unlisten> {
  return listen<EventPayloads[E]>(event, handler);
}

// -- commands ----------------------------------------------------------------

export const api = {
  // overlay
  overlayReady: () => invoke<void>("overlay_ready"),
  overlayResize: (width: number, height: number) => invoke<void>("overlay_resize", { width, height }),

  // config
  getConfig: () => invoke<Config>("get_config"),
  /** Resolves to whether the engine must restart for the saved settings to apply. */
  setConfigValue: (section: ConfigSection, key: string, value: ConfigValue) =>
    invoke<boolean>("set_config_value", { section, key, value }),
  getConfigPath: () => invoke<string>("get_config_path"),
  openConfigFile: () => invoke<void>("open_config_file"),

  // engine
  getStatus: () => invoke<Status>("get_status"),
  setRunning: (on: boolean) => invoke<void>("set_running", { on }),
  restartEngine: () => invoke<void>("restart_engine"),
  runDoctor: () => invoke<DoctorCheck[]>("run_doctor"),
  copyDiagnostics: () => invoke<string>("copy_diagnostics"),
  checkForUpdates: () => invoke<UpdateCheck>("check_for_updates"),
  /** Downloads over `flow:update`, installs and restarts Flow; rejects when this copy cannot update itself. */
  installUpdate: () => invoke<void>("install_update"),
  openReleasePage: () => invoke<void>("open_release_page"),

  // audio + recognition
  listInputDevices: () => invoke<InputDevice[]>("list_input_devices"),
  listSttModels: () => invoke<SttModel[]>("list_stt_models"),
  /** Starts a download reported over `flow:download`; rejects while another runs. */
  downloadModel: (id: string) => invoke<void>("download_model", { id }),
  /** The latest progress of the download in progress, or null. */
  getDownload: () => invoke<DownloadEvent | null>("get_download"),
  cancelDownload: () => invoke<void>("cancel_download"),
  getComputeReport: () => invoke<ComputeReport>("get_compute_report"),
  detectGpu: () => invoke<GpuReport>("detect_gpu"),

  // cleanup providers
  listProviders: () => invoke<ProviderSpec[]>("list_providers"),
  listProviderModels: (provider: string) => invoke<string[]>("list_provider_models", { provider }),
  getKeySource: (provider: string) => invoke<string>("get_key_source", { provider }),
  /** Both reject with the reason when the keyring fails. */
  setApiKey: (provider: string, key: string) => invoke<void>("set_api_key", { provider, key }),
  clearApiKey: (provider: string) => invoke<void>("clear_api_key", { provider }),

  // learning
  getLearningSummary: () => invoke<LearningSummary>("get_learning_summary"),
  forgetHistory: () => invoke<number>("forget_history"),

  // desktop
  /** On GNOME this writes the Shell extension's binding. */
  setHotkey: (combo: string) => invoke<void>("set_hotkey", { combo }),
  getAutostart: () => invoke<boolean>("get_autostart"),
  setAutostart: (on: boolean) => invoke<void>("set_autostart", { on }),
  getPermissions: () => invoke<Permission[]>("get_permissions"),
  /** For "hotkey-gnome-extension" this installs and enables the extension; it works after the next login. */
  requestPermission: (id: string) => invoke<void>("request_permission", { id }),

  // first-run wizard
  /** Records about two seconds, emitting `flow:level` meanwhile. */
  wizardTestMic: () => invoke<WizardMicResult | null | undefined>("wizard_test_mic"),
  wizardTestTranscribe: () => invoke<WizardTranscribeResult>("wizard_test_transcribe"),
  wizardTestPaste: () => invoke<WizardPasteResult>("wizard_test_paste"),
  wizardTestCleanup: () => invoke<WizardCleanupResult>("wizard_test_cleanup"),
  wizardComplete: () => invoke<void>("wizard_complete"),
};
