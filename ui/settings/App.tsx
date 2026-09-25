import { useEffect, useRef, useState } from "react";
import { api, type Status } from "../shared/api";
import { useAction, useInterval } from "../shared/hooks";
import { LoadFailed, useConfigStore } from "../shared/prefs";
import { Banner, ToastProvider, useToast } from "../shared/ui";
import { SettingsContext, type SettingsContextValue } from "./context";
import { AboutSection } from "./sections/About";
import { CleanupSection } from "./sections/Cleanup";
import { DesktopSection } from "./sections/Desktop";
import { LearningSection } from "./sections/Learning";
import { StatusSection } from "./sections/Status";
import { UpdatesSection } from "./sections/Updates";
import { VoiceSection } from "./sections/Voice";

export function App() {
  return (
    <ToastProvider>
      <Settings />
    </ToastProvider>
  );
}

function Settings() {
  const toast = useToast();
  const store = useConfigStore();
  const [status, setStatus] = useState<Status | null>(null);
  const [needsRestart, setNeedsRestart] = useState(false);
  const saves = useRef(0);

  const refreshStatus = async () => {
    const seen = saves.current;
    try {
      const s = await api.getStatus();
      setStatus(s);
      // Rust's flag is the truth (a tray restart clears it), unless a save
      // landed while this was in flight.
      if (seen === saves.current) setNeedsRestart(s.needs_restart);
    } catch (err) {
      console.warn("get_status failed", err);
    }
  };

  useEffect(() => {
    void refreshStatus();
  }, []);
  useInterval(() => void refreshStatus(), 3000);

  const save: SettingsContextValue["save"] = async (section, key, value, message) => {
    const restart = await store.save(section, key, value);
    saves.current += 1;
    if (restart) setNeedsRestart(true);
    if (message !== false) toast(message ?? "Saved");
  };

  const restart = useAction(
    async () => {
      await api.restartEngine();
      setNeedsRestart(false);
      toast("Restarting Rustle…");
      await refreshStatus();
    },
    (message) => toast(`Could not restart: ${message}`),
  );

  if (store.error) {
    return (
      <div className="mx-auto max-w-[720px] px-6 py-10">
        <LoadFailed title="Settings could not load" error={store.error} />
      </div>
    );
  }
  if (!store.config) {
    return <div className="mx-auto max-w-[720px] px-6 py-10 text-fg-2">Loading…</div>;
  }

  return (
    <SettingsContext.Provider value={{ config: store.config, status, save, patch: store.patch, refreshStatus }}>
      <div className="min-h-full">
        {needsRestart && status?.running && (
          <div className="sticky top-0 z-30">
            <Banner title="Restart Rustle to apply your changes" action="Restart" busy={restart.busy} onAction={() => void restart.start()} />
          </div>
        )}
        <main className="mx-auto flex w-full max-w-[720px] flex-col gap-8 px-6 pt-6 pb-16">
          <StatusSection />
          <VoiceSection />
          <CleanupSection />
          <DesktopSection />
          <LearningSection />
          <UpdatesSection />
          <AboutSection />
        </main>
      </div>
    </SettingsContext.Provider>
  );
}
