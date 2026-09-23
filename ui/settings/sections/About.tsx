import { api } from "../../shared/api";
import { describe, useAction, useAsync } from "../../shared/hooks";
import { Button, Group, Row, useToast } from "../../shared/ui";
import { useSettings } from "../context";

export function AboutSection() {
  const { status } = useSettings();
  const toast = useToast();
  const path = useAsync(() => api.getConfigPath());

  const copy = useAction(
    async () => {
      const text = await api.copyDiagnostics();
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        // The native side may have put it on the clipboard already.
      }
      toast("Diagnostics copied");
    },
    (message) => toast(`Could not collect diagnostics: ${message}`),
  );

  return (
    <Group title="About">
      <Row title="Configuration file" subtitle={<span className="selectable font-mono text-[12px]">{path.data ?? ""}</span>}>
        <Button onClick={() => void api.openConfigFile().catch((err) => toast(`Could not open: ${describe(err)}`))}>Open</Button>
      </Row>
      <Row title="Version" subtitle={status?.version ? `Flow ${status.version}` : "…"} />
      <Row title="Diagnostics" subtitle="Versions, devices and the last errors, for a bug report">
        <Button busy={copy.busy} onClick={() => void copy.start()}>
          Copy diagnostics
        </Button>
      </Row>
    </Group>
  );
}
