import { useEffect, useRef, useState, type ComponentType } from "react";
import { api, type Permission, type Status } from "../shared/api";
import { describe, useInterval } from "../shared/hooks";
import { LoadFailed, useConfigStore } from "../shared/prefs";
import { Button, ToastProvider, useToast } from "../shared/ui";
import { WizardContext } from "./context";
import { CleanupStep } from "./steps/Cleanup";
import { DoneStep } from "./steps/Done";
import { HotkeyStep } from "./steps/Hotkey";
import { MicrophoneStep } from "./steps/Microphone";
import { ModelStep } from "./steps/Model";
import { PasteStep } from "./steps/Paste";
import { PreferencesStep } from "./steps/Preferences";
import { WelcomeStep } from "./steps/Welcome";

interface Step {
  id: string;
  label: string;
  Component: ComponentType;
}

const STEPS: Step[] = [
  { id: "welcome", label: "Welcome", Component: WelcomeStep },
  { id: "microphone", label: "Microphone", Component: MicrophoneStep },
  { id: "model", label: "Recognition", Component: ModelStep },
  { id: "hotkey", label: "Shortcut", Component: HotkeyStep },
  { id: "paste", label: "Paste-back", Component: PasteStep },
  { id: "cleanup", label: "Cleanup", Component: CleanupStep },
  { id: "preferences", label: "Preferences", Component: PreferencesStep },
  { id: "done", label: "Done", Component: DoneStep },
];

// Dictation runs from the Shortcut step on, so the shortcut's dot and the
// Done step's try-it box work. Reaching either starts the engine, or reloads
// it when a setting changed since; the steps between leave it running.
const ENGINE_STEPS = new Set(["hotkey", "done"]);

export function App() {
  return (
    <ToastProvider>
      <Wizard />
    </ToastProvider>
  );
}

function Wizard() {
  const toast = useToast();
  const store = useConfigStore();
  const [status, setStatus] = useState<Status | null>(null);
  const [starting, setStarting] = useState(false);
  const engineBusy = useRef(false);
  // An engine step reached while a start was still under way; that start
  // may predate settings changed since, so look again once it is done.
  const engineAgain = useRef(false);
  const onEngineStep = useRef(false);
  const [permissions, setPermissions] = useState<Permission[]>([]);
  const [index, setIndex] = useState(() => {
    const q = new URLSearchParams(location.search).get("step");
    const i = q ? STEPS.findIndex((s) => s.id === q) : -1;
    return i >= 0 ? i : 0;
  });
  const [finishing, setFinishing] = useState(false);

  const refreshStatus = async () => {
    try {
      setStatus(await api.getStatus());
    } catch (err) {
      console.warn("get_status failed", err);
    }
  };

  const refreshPermissions = async () => {
    try {
      setPermissions(await api.getPermissions());
    } catch (err) {
      console.warn("get_permissions failed", err);
    }
  };

  useEffect(() => {
    void refreshStatus();
    void refreshPermissions();
  }, []);

  const step = STEPS[index]!;
  const last = index === STEPS.length - 1;
  const engineStep = ENGINE_STEPS.has(step.id);

  onEngineStep.current = engineStep;

  const syncEngine = async (): Promise<void> => {
    if (engineBusy.current) {
      engineAgain.current = true;
      return;
    }
    engineBusy.current = true;
    setStarting(true);
    try {
      const s = await api.getStatus();
      if (!s.running) await api.setRunning(true);
      else if (s.needs_restart) await api.restartEngine();
    } catch (err) {
      toast(`Could not start Flow: ${describe(err)}`);
    } finally {
      engineBusy.current = false;
      setStarting(false);
      await refreshStatus();
    }
    if (engineAgain.current) {
      engineAgain.current = false;
      if (onEngineStep.current) await syncEngine();
    }
  };

  useEffect(() => {
    if (engineStep) void syncEngine();
  }, [step.id]);

  // A start that looked fine can still fail a moment later (the model).
  useInterval(() => void refreshStatus(), engineStep ? 3000 : null);

  // Stays busy on success: the native side closes the window.
  const finish = async () => {
    setFinishing(true);
    try {
      await api.wizardComplete();
    } catch (err) {
      toast(`Could not finish: ${describe(err)}`);
      setFinishing(false);
    }
  };

  if (store.error) {
    return (
      <div className="p-10">
        <LoadFailed title="Setup could not start" error={store.error} />
      </div>
    );
  }
  if (!store.config) return <div className="p-10 text-fg-2">Loading…</div>;

  return (
    <WizardContext.Provider
      value={{
        config: store.config,
        save: store.save,
        patch: store.patch,
        hotkey: status?.hotkey || store.config.desktop.hotkey,
        status,
        starting,
        refreshStatus,
        permissions,
        refreshPermissions,
      }}
    >
      <div className="flex h-screen min-h-[520px]">
        <Rail steps={STEPS} index={index} onJump={setIndex} />
        <div className="flex min-w-0 flex-1 flex-col">
          <main className="min-h-0 flex-1 overflow-y-auto px-10 pt-10 pb-6">
            <div key={step.id} className="mx-auto w-full max-w-[560px]">
              <step.Component />
            </div>
          </main>
          <footer className="flex items-center justify-between gap-3 border-t border-line px-10 py-4">
            <span className="text-[12.5px] text-fg-3">
              Step {index + 1} of {STEPS.length}
            </span>
            <div className="flex gap-2">
              <Button variant="flat" disabled={index === 0} onClick={() => setIndex((i) => Math.max(0, i - 1))}>
                Back
              </Button>
              {last ? (
                <Button variant="suggested" busy={finishing} onClick={() => void finish()}>
                  Start using Flow
                </Button>
              ) : (
                <Button variant="suggested" onClick={() => setIndex((i) => Math.min(STEPS.length - 1, i + 1))}>
                  Continue
                </Button>
              )}
            </div>
          </footer>
        </div>
      </div>
    </WizardContext.Provider>
  );
}

function Rail({ steps, index, onJump }: { steps: Step[]; index: number; onJump: (i: number) => void }) {
  return (
    <nav aria-label="Setup steps" className="hidden w-[200px] shrink-0 flex-col border-r border-line bg-surface px-5 pt-10 sm:flex">
      <div className="mb-6 flex items-center gap-2 px-1">
        <Mark />
        <span className="text-[15px] font-semibold">Flow</span>
      </div>
      <ol className="relative flex flex-col">
        {steps.map((s, i) => {
          const state = i < index ? "done" : i === index ? "current" : "todo";
          return (
            <li key={s.id} className="relative">
              {i < steps.length - 1 && (
                <span aria-hidden="true" className={"absolute top-[18px] left-[11px] h-full w-px " + (i < index ? "bg-accent" : "bg-line-strong")} />
              )}
              <button
                type="button"
                aria-current={state === "current" ? "step" : undefined}
                onClick={() => onJump(i)}
                className={
                  "relative flex w-full items-center gap-3 rounded-control px-1 py-2 text-left text-[13.5px] transition-colors " +
                  (state === "current" ? "font-medium" : state === "done" ? "text-fg-2 hover:text-fg" : "text-fg-3 hover:text-fg-2")
                }
              >
                <span
                  className={
                    "flex h-[14px] w-[14px] shrink-0 items-center justify-center rounded-full transition-colors " +
                    (state === "done"
                      ? "bg-accent"
                      : state === "current"
                        ? "bg-surface [box-shadow:inset_0_0_0_2px_var(--accent)]"
                        : "bg-surface [box-shadow:inset_0_0_0_1.5px_var(--line-strong)]")
                  }
                >
                  {state === "done" && (
                    <svg width="8" height="8" viewBox="0 0 8 8" fill="none" stroke="var(--accent-fg)" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                      <path d="M1.5 4.2 3.2 5.8 6.5 2.3" />
                    </svg>
                  )}
                </span>
                {s.label}
              </button>
            </li>
          );
        })}
      </ol>
    </nav>
  );
}

function Mark() {
  // The pill in miniature: five bars, the way the island idles.
  return (
    <svg width="26" height="18" viewBox="0 0 26 18" aria-hidden="true" className="text-accent">
      <rect x="0" y="0" width="26" height="18" rx="9" fill="rgb(17,19,27)" />
      {[4, 7, 10, 6, 4].map((h, i) => (
        <rect key={i} x={5 + i * 3.6} y={9 - h / 2} width="1.8" height={h} rx="0.9" fill="currentColor" opacity={i === 0 || i === 4 ? 0.5 : 1} />
      ))}
    </svg>
  );
}
