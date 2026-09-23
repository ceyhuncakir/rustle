import type { ReactNode } from "react";

interface GroupProps {
  title?: string;
  description?: string;
  children: ReactNode;
}

/** A titled boxed list of rows, like an Adw.PreferencesGroup. */
export function Group({ title, description, children }: GroupProps) {
  return (
    <section className="flex flex-col gap-2.5">
      {(title || description) && (
        <header className="px-1">
          {title && <h2 className="text-[15px] font-semibold leading-tight">{title}</h2>}
          {description && <p className="mt-0.5 text-[13px] text-fg-2">{description}</p>}
        </header>
      )}
      <div className="card divide-y divide-line">{children}</div>
    </section>
  );
}
