import { usePermissionGrant } from "../../shared/prefs";
import { Button } from "../../shared/ui";
import { useWizard } from "../context";

/** The permissions one step depends on, each with a Grant button. */
export function PermissionRows({ scope }: { scope: "hotkey" | "paste" }) {
  const { permissions, refreshPermissions } = useWizard();
  const { busy, grant, pending, actionLabel } = usePermissionGrant(refreshPermissions);

  const items = permissions.filter((p) => p.id.startsWith("hotkey") === (scope === "hotkey"));
  if (items.length === 0) return null;

  return (
    <section className="mt-6">
      <h2 className="mb-2 px-1 text-[13px] font-medium text-fg-2">What this desktop needs</h2>
      <ul className="card divide-y divide-line">
        {items.map((p) => {
          const waiting = pending(p);
          return (
            <li key={p.id} className="flex items-start gap-4 px-4 py-3">
              <span
                aria-hidden="true"
                className={"mt-[7px] h-2 w-2 shrink-0 rounded-full " + (p.granted ? "bg-success" : waiting ? "bg-accent" : p.required ? "bg-danger" : "bg-fg-3")}
              />
              <div className="min-w-0 flex-1">
                <div className="text-[14px] font-medium">
                  {p.label}
                  {!p.required && <span className="ml-2 text-[12px] font-normal text-fg-3">optional</span>}
                </div>
                <p className="selectable mt-0.5 text-[12.5px] text-fg-2">{p.help}</p>
                {waiting && <p className="mt-1 text-[12.5px] font-medium">Installed. Log out and back in to finish.</p>}
              </div>
              {p.granted ? (
                <span className="shrink-0 pt-1 text-[12.5px] text-success">Granted</span>
              ) : (
                !waiting && (
                  <Button busy={busy === p.id} onClick={() => void grant(p.id)}>
                    {actionLabel(p.id)}
                  </Button>
                )
              )}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
