import type { ReactNode } from "react";
import { Button } from "./Button";

interface BannerProps {
  title: ReactNode;
  action?: string;
  onAction?: () => void;
  busy?: boolean;
}

/** A full-width notice at the top of the window, like an Adw.Banner. */
export function Banner({ title, action, onAction, busy }: BannerProps) {
  return (
    <div role="status" className="flex items-center justify-between gap-4 bg-accent-soft px-6 py-2.5 text-[13.5px] font-medium">
      <span>{title}</span>
      {action && (
        <Button variant="suggested" busy={busy} onClick={onAction}>
          {action}
        </Button>
      )}
    </div>
  );
}
