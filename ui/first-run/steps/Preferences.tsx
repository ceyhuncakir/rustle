import { detach } from "../../shared/hooks";
import { AutostartSwitch } from "../../shared/prefs";
import { Group, Row, Switch } from "../../shared/ui";
import { useWizard } from "../context";
import { StepHeader } from "./StepHeader";

export function PreferencesStep() {
  const { config, save } = useWizard();

  return (
    <div>
      <StepHeader title="Preferences" lead="Two things that are off until you say otherwise." />
      <Group>
        <Row
          title="Learn my vocabulary"
          htmlFor="learning"
          subtitle="Keeps your dictations on this machine and mines them for the names and jargon the recogniser gets wrong, and for how you phrase things. While off, nothing you dictate is stored."
        >
          <Switch id="learning" checked={config.learning.enabled} onChange={(on) => detach(save("learning", "enabled", on))} />
        </Row>
        <Row title="Launch at login" htmlFor="autostart" subtitle="Start Rustle in the background when you sign in, so the shortcut always works.">
          <AutostartSwitch id="autostart" />
        </Row>
      </Group>
      <p className="mt-5 max-w-[52ch] text-[13.5px] text-fg-2">
        While it runs, Rustle keeps a small icon in the tray. Use it to pause dictation, open Settings or quit; closing this window does
        not stop Rustle.
      </p>
    </div>
  );
}
