import { useState } from "react";
import { api } from "../../shared/api";
import { describe, useAction, useEvent } from "../../shared/hooks";
import { AutostartSwitch } from "../../shared/prefs";
import { Button, ChoiceRow, Group, HotkeyRecorder, Row, useToast, type Choice } from "../../shared/ui";
import { useSettings } from "../context";

const OVERLAY: readonly Choice[] = [
  { value: "auto", label: "Automatic", description: "Wherever the desktop allows an overlay that cannot steal focus" },
  { value: "window", label: "Always a window", description: "A plain always-on-top window" },
  { value: "off", label: "Off", description: "Never show the island" },
];

export function DesktopSection() {
  const { config, status, save, patch, refreshStatus } = useSettings();
  const toast = useToast();
  const [down, setDown] = useState(false);
  useEvent("flow:hotkey", ({ down }) => setDown(down));

  const hotkey = config.desktop.hotkey || status?.hotkey || "";

  const saveHotkey = async (combo: string) => {
    try {
      await api.setHotkey(combo);
      patch("desktop", "hotkey", combo);
      toast("Shortcut saved");
      await refreshStatus();
    } catch (err) {
      toast(`Could not set the shortcut: ${describe(err)}`);
      throw err;
    }
  };

  const updates = useAction(
    async () => {
      const result = await api.checkForUpdates();
      toast(result.available ? `Flow ${result.version ?? ""} is available`.replace(/\s+/g, " ") : "You have the latest version");
    },
    (message) => toast(`Could not check: ${message}`),
  );

  return (
    <Group title="Desktop" description="How Flow sits in your desktop">
      <Row title="Shortcut" subtitle="Hold to talk, or tap to keep recording until the next tap">
        <HotkeyRecorder value={hotkey} onSave={saveHotkey} down={status?.running ? down : undefined} />
      </Row>

      <ChoiceRow id="overlay" title="Overlay" choices={OVERLAY} value={config.desktop.overlay} onChange={(v) => void save("desktop", "overlay", v)} />

      <Row title="Launch at login" htmlFor="autostart" subtitle="Start Flow in the background when you sign in">
        <AutostartSwitch id="autostart" onSaved={(on) => toast(on ? "Flow will start when you log in" : "Flow will not start at login")} />
      </Row>

      <Row title="Updates" subtitle={status?.version ? `Flow ${status.version}` : ""}>
        <Button busy={updates.busy} onClick={() => void updates.start()}>
          Check for updates
        </Button>
      </Row>
    </Group>
  );
}
