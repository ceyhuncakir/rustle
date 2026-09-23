import type { ReactNode } from "react";

interface RowProps {
  title: ReactNode;
  subtitle?: ReactNode;
  children?: ReactNode;
  /** Put the control under the title instead of beside it. */
  stacked?: boolean;
  htmlFor?: string;
  /** Extra content under the row, e.g. an expanded result list. */
  below?: ReactNode;
}

/** One line of a Group: title and subtitle at the left, the control at the right. */
export function Row({ title, subtitle, children, stacked, htmlFor, below }: RowProps) {
  const Label = htmlFor ? "label" : "div";
  return (
    <div className="px-4 py-3">
      <div className={stacked ? "flex flex-col gap-2.5" : "flex min-h-[32px] items-center gap-4"}>
        <Label htmlFor={htmlFor} className="min-w-0 flex-1">
          <div className="text-[14px] font-medium leading-snug">{title}</div>
          {subtitle && <div className="mt-0.5 text-[12.5px] leading-snug text-fg-2">{subtitle}</div>}
        </Label>
        {children && <div className={stacked ? "" : "flex shrink-0 items-center gap-2"}>{children}</div>}
      </div>
      {below}
    </div>
  );
}
