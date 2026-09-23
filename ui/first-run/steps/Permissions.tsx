import { useState } from "react";
import { api } from "../../shared/api";
import { describe } from "../../shared/hooks";
import { Button, useToast } from "../../shared/ui";
import { useWizard } from "../context";

/** The permissions one step depends on, each with a Grant button. */
export function PermissionRows({ scope }: { scope: "hotkey" | "paste" }) {
  const { permissions, refreshPermissions } = useWizard();
  const toast = useToast();
  const [busy, setBusy] = useState<string | null>(null);

  const items = permissions.filter((p) => p.id.startsWith("hotkey") === (scope === "hotkey"));
  if (items.length === 0) return null;

  const grant = async (id: string) => {
    setBusy(id);
    try {
      await api.requestPermission(id);
      await refreshPermissions();
    } catch (err) {
      toast(`Could not request it: ${describe(err)}`);
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="mt-6">
      <h2 className="mb-2 px-1 text-[13px] font-medium text-fg-2">What this desktop needs</h2>
      <ul className="card divide-y divide-line">
        {items.map((p) => (
          <li key={p.id} className="flex items-start gap-4 px-4 py-3">
            <span aria-hidden="true" className={"mt-[7px] h-2 w-2 shrink-0 rounded-full " + (p.granted ? "bg-success" : p.required ? "bg-danger" : "bg-fg-3")} />
            <div className="min-w-0 flex-1">
              <div className="text-[14px] font-medium">
                {p.label}
                {!p.required && <span className="ml-2 text-[12px] font-normal text-fg-3">optional</span>}
              </div>
              <p className="mt-0.5 text-[12.5px] text-fg-2">{p.help}</p>
            </div>
            {p.granted ? (
              <span className="shrink-0 pt-1 text-[12.5px] text-success">Granted</span>
            ) : (
              <Button busy={busy === p.id} onClick={() => void grant(p.id)}>
                Grant
              </Button>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
