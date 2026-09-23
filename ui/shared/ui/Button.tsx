import type { ButtonHTMLAttributes, ReactNode } from "react";

interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> {
  variant?: "default" | "suggested" | "destructive" | "flat";
  busy?: boolean;
  children: ReactNode;
}

const VARIANTS: Record<NonNullable<ButtonProps["variant"]>, string> = {
  default: "bg-surface-2 hover:bg-surface-3 [box-shadow:inset_0_0_0_1px_var(--line)] active:bg-surface-3",
  suggested: "bg-accent text-accent-fg hover:brightness-110 active:brightness-95",
  destructive: "bg-danger-soft text-danger hover:brightness-110 [box-shadow:inset_0_0_0_1px_var(--danger-soft)]",
  flat: "hover:bg-surface-2 active:bg-surface-3",
};

export function Button({ variant = "default", busy, className, disabled, children, type = "button", ...rest }: ButtonProps) {
  return (
    <button
      type={type}
      disabled={disabled || busy}
      aria-busy={busy || undefined}
      className={
        "inline-flex h-8 shrink-0 items-center justify-center gap-1.5 rounded-control px-3 text-[13.5px] font-medium transition-[background-color,filter] duration-100 disabled:opacity-50 disabled:pointer-events-none " +
        VARIANTS[variant] +
        " " +
        (className ?? "")
      }
      {...rest}
    >
      {busy && <Spinner />}
      {children}
    </button>
  );
}

function Spinner({ className }: { className?: string }) {
  return (
    <svg className={"h-3.5 w-3.5 animate-spin " + (className ?? "")} viewBox="0 0 16 16" fill="none" aria-hidden="true">
      <circle cx="8" cy="8" r="6" stroke="currentColor" strokeOpacity="0.25" strokeWidth="2" />
      <path d="M14 8a6 6 0 0 0-6-6" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
    </svg>
  );
}
