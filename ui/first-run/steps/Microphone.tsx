import { api, type WizardMicResult } from "../../shared/api";
import { detach, useAction, useAsync } from "../../shared/hooks";
import { deviceOptions } from "../../shared/prefs";
import { Button, LevelMeter, Select } from "../../shared/ui";
import { useWizard } from "../context";
import { Outcome, StepHeader } from "./StepHeader";

/** wizard_test_mic reports a quiet take as captured; below this it was silence. */
const SILENT = 0.02;

function heard(r: WizardMicResult): boolean {
  return r.ok && (r.peak === undefined || r.peak >= SILENT);
}

export function MicrophoneStep() {
  const { config, save } = useWizard();
  const devices = useAsync(() => api.listInputDevices());
  const test = useAction(async () => (await api.wizardTestMic()) ?? { ok: true });

  return (
    <div>
      <StepHeader title="Microphone" lead="Pick the input Rustle should record from, then press Test and say something: the bars move with your voice." />
      <label className="mb-4 flex items-center justify-between gap-4">
        <span className="text-[14px] font-medium">Input</span>
        <Select label="Input device" value={config.audio.device} options={deviceOptions(devices.data)} onChange={(v) => detach(save("audio", "device", v))} />
      </label>
      <div className="flex justify-center py-2">
        <LevelMeter className="w-[268px]" />
      </div>
      <div className="mt-4 flex items-center gap-3">
        <Button busy={test.busy} onClick={() => void test.start()}>
          {test.busy ? "Listening…" : "Test"}
        </Button>
        {test.busy && <span className="text-[13px] text-fg-2">Talk for a couple of seconds.</span>}
        {test.error && <Outcome ok={false}>{test.error}</Outcome>}
        {test.result && (
          <Outcome ok={heard(test.result)}>
            {test.result.detail ?? `${test.result.ok ? "Heard you" : "Heard nothing"}${test.result.peak !== undefined ? ` (peak ${Math.round(test.result.peak * 100)}%)` : ""}`}
          </Outcome>
        )}
      </div>
      {devices.error && <p className="mt-3 text-[13px] text-danger">Could not list devices: {devices.error}</p>}
    </div>
  );
}
