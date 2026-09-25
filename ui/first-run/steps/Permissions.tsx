import { useState } from "react";
import { api } from "../../shared/api";
import { describe } from "../../shared/hooks";
import { Button, useToast } from "../../shared/ui";
import { useWizard } from "../context";

/**
 * The button for each permission Rustle cannot simply ask the OS for. The GNOME
 * extension is installed by Rustle and only runs after the next login; the
 * paste helper is installed by the user, so the button just looks again.
 * Anything else asks Rust to request it.
 */
const ACTIONS: Record<string, string> = {
  "hotkey-gnome-extension": "Install",
  "paste-tool": "Check again",
};

/** Granted once the user logs out and back in. */
const NEEDS_LOGIN = new Set(["hotkey-gnome-extension"]);

/** The permissions one step depends on, each with a Grant button. */
export function PermissionRows({ scope }: { scope: "hotkey" | "paste" }) {
  const { permissions, refreshPermissions } = useWizard();
  const toast = useToast();
  const [busy, setBusy] = useState<string | null>(null);
  const [awaitingLogin, setAwaitingLogin] = useState<ReadonlySet<string>>(new Set());

  const items = permissions.filter((p) => p.id.startsWith("hotkey") === (scope === "hotkey"));
  if (items.length === 0) return null;

  const grant = async (id: string) => {
    setBusy(id);
    try {
      await api.requestPermission(id);
      if (NEEDS_LOGIN.has(id)) {
        setAwaitingLogin((s) => new Set(s).add(id));
        toast("Installed. Log out and back in to finish.");
      }
      await refreshPermissions();
    } catch (err) {
      toast(`Could not ${NEEDS_LOGIN.has(id) ? "install" : "request"} it: ${describe(err)}`);
    } finally {
      setBusy(null);
    }
  };

  return (
    <section className="mt-6">
      <h2 className="mb-2 px-1 text-[13px] font-medium text-fg-2">What this desktop needs</h2>
      <ul className="card divide-y divide-line">
        {items.map((p) => {
          const pending = !p.granted && awaitingLogin.has(p.id);
          return (
            <li key={p.id} className="flex items-start gap-4 px-4 py-3">
              <span
                aria-hidden="true"
                className={"mt-[7px] h-2 w-2 shrink-0 rounded-full " + (p.granted ? "bg-success" : pending ? "bg-accent" : p.required ? "bg-danger" : "bg-fg-3")}
              />
              <div className="min-w-0 flex-1">
                <div className="text-[14px] font-medium">
                  {p.label}
                  {!p.required && <span className="ml-2 text-[12px] font-normal text-fg-3">optional</span>}
                </div>
                <p className="selectable mt-0.5 text-[12.5px] text-fg-2">{p.help}</p>
                {pending && <p className="mt-1 text-[12.5px] font-medium">Installed. Log out and back in to finish.</p>}
              </div>
              {p.granted ? (
                <span className="shrink-0 pt-1 text-[12.5px] text-success">Granted</span>
              ) : (
                !pending && (
                  <Button busy={busy === p.id} onClick={() => void grant(p.id)}>
                    {ACTIONS[p.id] ?? "Grant"}
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
