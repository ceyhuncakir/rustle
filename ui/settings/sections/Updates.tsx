import { useState } from "react";
import { api, type UpdateEvent } from "../../shared/api";
import { describe, detach, useAction, useEvent } from "../../shared/hooks";
import { Button, Group, Progress, Row, Switch, formatBytes } from "../../shared/ui";
import { useSettings } from "../context";

export function UpdatesSection() {
  const { config, status, save } = useSettings();
  const [progress, setProgress] = useState<UpdateEvent | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  const check = useAction(() => api.checkForUpdates());
  const install = useAction(
    async () => {
      setFailure(null);
      await api.installUpdate();
    },
    (message) => {
      setProgress(null);
      setFailure(message);
    },
  );
  useEvent("rustle:update", (event) => {
    if (event.error) {
      setProgress(null);
      setFailure(event.error);
    } else {
      setProgress(event);
    }
  });

  const result = check.result;
  const current = result?.current ?? status?.version ?? "";
  const summary = check.busy
    ? "Checking…"
    : check.error
      ? `Could not check: ${check.error}`
      : result && !result.available
        ? `Rustle ${current} is the latest version`
        : current && `Rustle ${current}`;

  const installing = install.busy || progress !== null;

  return (
    <Group title="Updates">
      <Row
        title="Version"
        subtitle={<span className={check.error ? "selectable text-danger" : undefined}>{summary}</span>}
      >
        <Button busy={check.busy} disabled={installing} onClick={() => void check.start()}>
          Check for updates
        </Button>
      </Row>

      {result?.available && result.version && (
        <Row
          title={`Rustle ${result.version} is available`}
          subtitle={<Released date={result.date} notes={result.notes} />}
          below={
            progress ? (
              <InstallProgress event={progress} />
            ) : failure ? (
              <p className="selectable mt-2 text-[12.5px] text-danger">{failure}</p>
            ) : (
              !result.can_install && result.how && <p className="mt-2 text-[12.5px] text-fg-2">{result.how}</p>
            )
          }
        >
          {result.can_install ? (
            <Button variant="suggested" busy={installing} onClick={() => void install.start()}>
              Install and restart
            </Button>
          ) : (
            <Button onClick={() => void api.openReleasePage().catch((err) => setFailure(describe(err)))}>Open releases page</Button>
          )}
        </Row>
      )}

      <Row
        title="Check automatically"
        htmlFor="check-updates"
        subtitle="Once a day, ask GitHub whether a newer Rustle is out and say so. Nothing else is sent. Builds from source never ask."
      >
        <Switch
          id="check-updates"
          checked={config.desktop.check_updates}
          onChange={(on) => detach(save("desktop", "check_updates", on, on ? "Rustle will check once a day" : "Rustle will not check on its own"))}
        />
      </Row>
    </Group>
  );
}

function Released({ date, notes }: { date: string | null; notes: string | null }) {
  const when = date ? new Date(date) : null;
  const day =
    when && !Number.isNaN(when.getTime()) ? when.toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" }) : null;
  if (!day && !notes) return null;
  return (
    <span className="selectable whitespace-pre-line">
      {day && `Released ${day}`}
      {day && notes && ". "}
      {notes}
    </span>
  );
}

function InstallProgress({ event }: { event: UpdateEvent }) {
  const { received, total, done } = event;
  const downloaded = total > 0 && received >= total;
  const label = done
    ? "Installed. Restarting Rustle…"
    : downloaded
      ? "Installing…"
      : total > 0
        ? `Downloading: ${formatBytes(received)} of ${formatBytes(total)}`
        : `Downloading: ${formatBytes(received)}`;
  return (
    <div className="mt-3 flex flex-col gap-1.5">
      <Progress value={done || downloaded ? 1 : total > 0 ? received / total : null} label="Update download" />
      <p className="text-[12.5px] text-fg-2" aria-live="polite">
        {label}
      </p>
    </div>
  );
}
