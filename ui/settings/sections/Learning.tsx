import { useState } from "react";
import { api } from "../../shared/api";
import { detach, plural, useAction, useAsync } from "../../shared/hooks";
import { Button, Group, Row, Switch, useToast } from "../../shared/ui";
import { useSettings } from "../context";

export function LearningSection() {
  const { config, save } = useSettings();
  const toast = useToast();
  const summary = useAsync(() => api.getLearningSummary(), [config.learning.enabled]);
  const [confirm, setConfirm] = useState(false);

  const s = summary.data;
  const learned =
    s && s.count > 0
      ? `${s.terms.length} terms from ${s.count} dictations` + (s.terms.length ? ` - ${s.terms.slice(0, 4).join(", ")}…` : "")
      : "Nothing stored yet";

  const forget = useAction(
    async () => {
      const removed = await api.forgetHistory();
      toast(`Deleted ${plural(removed, "stored dictation")}`);
      await summary.reload();
    },
    (message) => toast(`Could not clear: ${message}`),
  );

  return (
    <Group title="Learning" description="Off by default. While off, nothing you dictate is stored.">
      <Row title="Learn my vocabulary" htmlFor="learning" subtitle="Picks up your jargon and how you write, and uses both">
        <Switch id="learning" checked={config.learning.enabled} onChange={(on) => detach(save("learning", "enabled", on))} />
      </Row>
      <Row title="What it has learned" subtitle={learned}>
        {confirm ? (
          <>
            <span className="text-[12.5px] text-fg-2">Delete everything stored?</span>
            <Button variant="destructive" busy={forget.busy} onClick={() => void forget.start().then(() => setConfirm(false))}>
              Delete
            </Button>
            <Button variant="flat" onClick={() => setConfirm(false)}>
              Keep
            </Button>
          </>
        ) : (
          <Button variant="destructive" disabled={!s || s.count === 0} onClick={() => setConfirm(true)}>
            Forget all
          </Button>
        )}
      </Row>
    </Group>
  );
}
