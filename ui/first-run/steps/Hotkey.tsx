import { useState } from "react";
import { api } from "../../shared/api";
import { describe, useEvent } from "../../shared/hooks";
import { Group, HotkeyRecorder, Row, useToast } from "../../shared/ui";
import { useWizard } from "../context";
import { PermissionRows } from "./Permissions";
import { StepHeader } from "./StepHeader";

export function HotkeyStep() {
  const { hotkey, patch, status, starting, refreshStatus } = useWizard();
  const toast = useToast();
  const [down, setDown] = useState(false);
  useEvent("flow:hotkey", ({ down }) => setDown(down));

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

  const running = status?.running ?? false;
  const subtitle = starting ? (
    "Starting Flow…"
  ) : running ? (
    down ? "Pressed" : "Released"
  ) : (
    <span className="text-danger">{status?.error ?? "Flow is not running, so the shortcut can't be tried yet"}</span>
  );

  return (
    <div>
      <StepHeader
        title="Shortcut"
        lead="One key does both jobs: hold it and talk, or tap it to keep recording until the next tap. Press it now and the dot should light up."
      />
      <Group>
        <Row title="Dictation shortcut" subtitle={subtitle}>
          <HotkeyRecorder value={hotkey} onSave={saveHotkey} down={running && !starting ? down : undefined} />
        </Row>
      </Group>
      <PermissionRows scope="hotkey" />
    </div>
  );
}
