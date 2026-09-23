import type { ReactNode } from "react";

export function StepHeader({ title, lead }: { title: string; lead?: string }) {
  return (
    <header className="mb-6">
      <h1 className="text-[22px] font-semibold leading-tight tracking-[-0.01em]">{title}</h1>
      {lead && <p className="mt-2 max-w-[52ch] text-[14px] text-fg-2">{lead}</p>}
    </header>
  );
}

/** A small result line under a test button. */
export function Outcome({ ok, children }: { ok: boolean; children: ReactNode }) {
  return (
    <p className={"flex items-start gap-2 text-[13px] " + (ok ? "text-success" : "text-danger")}>
      <span aria-hidden="true" className={"mt-[6px] h-2 w-2 shrink-0 rounded-full " + (ok ? "bg-success" : "bg-danger")} />
      <span className="selectable text-fg">{children}</span>
    </p>
  );
}
