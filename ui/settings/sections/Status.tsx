import { useState } from "react";
import { api, type DoctorCheck } from "../../shared/api";
import { describe, plural } from "../../shared/hooks";
import { Button, Group, Keys, Row, Switch, useToast } from "../../shared/ui";
import { useSettings } from "../context";

export function StatusSection() {
  const { status, refreshStatus } = useSettings();
  const toast = useToast();
  const [switching, setSwitching] = useState<boolean | null>(null);
  const [checks, setChecks] = useState<DoctorCheck[] | null>(null);
  const [checking, setChecking] = useState(false);

  const running = switching ?? status?.running ?? false;
  const hotkey = status?.hotkey ?? "";

  const toggle = async (on: boolean) => {
    setSwitching(on);
    toast(on ? "Starting Flow…" : "Flow stopped");
    try {
      await api.setRunning(on);
      await refreshStatus();
    } catch (err) {
      toast(`Could not ${on ? "start" : "stop"}: ${describe(err)}`);
    } finally {
      setSwitching(null);
    }
  };

  // Hand-rolled rather than useAction: the previous results stay up while it re-runs.
  const doctor = async () => {
    setChecking(true);
    try {
      const result = await api.runDoctor();
      setChecks(result);
      const bad = result.filter((c) => !c.ok).length;
      toast(bad === 0 ? "Everything is ready" : `${plural(bad, "check")} failing`);
    } catch (err) {
      toast(`Could not run the checks: ${describe(err)}`);
    } finally {
      setChecking(false);
    }
  };

  return (
    <Group title="Status">
      <Row
        title="Dictation"
        htmlFor="dictation"
        subtitle={
          running ? (
            <span className="inline-flex flex-wrap items-center gap-1">
              Running - hold <Keys combo={hotkey} /> and talk
            </span>
          ) : (
            "Stopped"
          )
        }
      >
        <Switch id="dictation" checked={running} disabled={switching !== null || !status} onChange={(on) => void toggle(on)} />
      </Row>
      <Row
        title="Check setup"
        subtitle="Verify the microphone, GPU, model and desktop integration"
        below={checks && <DoctorResults checks={checks} />}
      >
        <Button busy={checking} onClick={() => void doctor()}>
          Run
        </Button>
      </Row>
    </Group>
  );
}

function DoctorResults({ checks }: { checks: DoctorCheck[] }) {
  return (
    <ul className="mt-3 divide-y divide-line overflow-hidden rounded-control bg-surface-2 text-[13px]">
      {checks.map((c) => (
        <li key={c.name} className="flex items-start gap-3 px-3 py-2">
          <span
            aria-label={c.ok ? "ok" : "failing"}
            className={"mt-[5px] h-2 w-2 shrink-0 rounded-full " + (c.ok ? "bg-success" : "bg-danger")}
          />
          <span className="w-[150px] shrink-0 font-medium">{c.name}</span>
          <span className="selectable min-w-0 flex-1 text-fg-2">{c.detail}</span>
        </li>
      ))}
    </ul>
  );
}
