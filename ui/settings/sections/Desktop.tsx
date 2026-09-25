import { useEffect, useState } from "react";
import { api, type Permission } from "../../shared/api";
import { describe, detach, useEvent } from "../../shared/hooks";
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
  useEvent("rustle:hotkey", ({ down }) => setDown(down));

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
    <Group title="Desktop" description="How Rustle sits in your desktop">
      <Row title="Shortcut" subtitle="Hold to talk, or tap to keep recording until the next tap">
        <HotkeyRecorder value={hotkey} onSave={saveHotkey} down={status?.running ? down : undefined} />
      </Row>

      <ChoiceRow id="overlay" title="Overlay" choices={OVERLAY} value={config.desktop.overlay} onChange={(v) => detach(save("desktop", "overlay", v))} />

      <Row title="Launch at login" htmlFor="autostart" subtitle="Start Rustle in the background when you sign in">
        <AutostartSwitch id="autostart" onSaved={(on) => toast(on ? "Rustle will start when you log in" : "Rustle will not start at login")} />
      </Row>

      <ExtensionRow />
    </Group>
  );
}

const EXTENSION = "hotkey-gnome-extension";

/**
 * On GNOME, the Shell extension when it is missing or older than the copy
 * this Rustle carries. The Shell keeps the copy it loaded at login, so after an
 * upgrade this is the only place to replace it; the wizard has run already.
 */
function ExtensionRow() {
  const toast = useToast();
  const [extension, setExtension] = useState<Permission | null>(null);
  const [installed, setInstalled] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .getPermissions()
      .then((list) => setExtension(list.find((p) => p.id === EXTENSION && !p.granted) ?? null))
      .catch((err) => console.warn("get_permissions failed", err));
  }, []);

  if (!extension) return null;

  const install = async () => {
    setBusy(true);
    try {
      await api.requestPermission(EXTENSION);
      setInstalled(true);
      toast("Installed. Log out and back in to finish.");
    } catch (err) {
      toast(`Could not install it: ${describe(err)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Row title={extension.label} subtitle={installed ? "Installed. Log out and back in to finish." : extension.help}>
      {!installed && (
        <Button busy={busy} onClick={() => void install()}>
          Install
        </Button>
      )}
    </Row>
  );
}
