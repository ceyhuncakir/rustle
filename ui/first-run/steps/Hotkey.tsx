import { useState } from "react";
import { api } from "../../shared/api";
import { describe, useEvent } from "../../shared/hooks";
import { Group, HotkeyRecorder, Row, useToast } from "../../shared/ui";
import { useWizard } from "../context";
import { PermissionRows } from "./Permissions";
import { StepHeader } from "./StepHeader";

export function HotkeyStep() {
  const { hotkey, patch } = useWizard();
  const toast = useToast();
  const [down, setDown] = useState(false);
  useEvent("flow:hotkey", ({ down }) => setDown(down));

  const saveHotkey = async (combo: string) => {
    try {
      await api.setHotkey(combo);
      patch("desktop", "hotkey", combo);
      toast("Shortcut saved");
    } catch (err) {
      toast(`Could not set the shortcut: ${describe(err)}`);
      throw err;
    }
  };

  return (
    <div>
      <StepHeader
        title="Shortcut"
        lead="One key does both jobs: hold it and talk, or tap it to keep recording until the next tap. Press it now and the dot should light up."
      />
      <Group>
        <Row title="Dictation shortcut" subtitle={down ? "Pressed" : "Released"}>
          <HotkeyRecorder value={hotkey} onSave={saveHotkey} down={down} />
        </Row>
      </Group>
      <PermissionRows scope="hotkey" />
    </div>
  );
}
