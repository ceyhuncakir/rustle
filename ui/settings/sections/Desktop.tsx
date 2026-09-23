import { useState } from "react";
import { api } from "../../shared/api";
import { describe, detach, useEvent } from "../../shared/hooks";
import { AutostartSwitch } from "../../shared/prefs";
import { ChoiceRow, Group, HotkeyRecorder, Row, useToast, type Choice } from "../../shared/ui";
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

  // What the engine (or on GNOME the Shell extension) actually listens for.
  const hotkey = status?.hotkey || config.desktop.hotkey;

  // Rethrows so the recorder keeps the combination for another try.
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

  return (
    <Group title="Desktop" description="How Flow sits in your desktop">
      <Row title="Shortcut" subtitle="Hold to talk, or tap to keep recording until the next tap">
        <HotkeyRecorder value={hotkey} onSave={saveHotkey} down={status?.running ? down : undefined} />
      </Row>

      <ChoiceRow id="overlay" title="Overlay" choices={OVERLAY} value={config.desktop.overlay} onChange={(v) => detach(save("desktop", "overlay", v))} />

      <Row title="Launch at login" htmlFor="autostart" subtitle="Start Flow in the background when you sign in">
        <AutostartSwitch id="autostart" onSaved={(on) => toast(on ? "Flow will start when you log in" : "Flow will not start at login")} />
      </Row>
    </Group>
  );
}
