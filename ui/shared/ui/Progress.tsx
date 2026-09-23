interface ProgressProps {
  /** 0..1, or null for indeterminate. */
  value: number | null;
  label?: string;
}

export function Progress({ value, label }: ProgressProps) {
  const pct = value === null ? null : Math.max(0, Math.min(1, value)) * 100;
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={pct === null ? undefined : Math.round(pct)}
      className="h-1.5 w-full overflow-hidden rounded-full bg-surface-3"
    >
      <div
        className={"h-full rounded-full bg-accent transition-[width] duration-200 " + (pct === null ? "w-1/3 animate-pulse" : "")}
        style={pct === null ? undefined : { width: `${pct}%` }}
      />
    </div>
  );
}

export function formatBytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(n >= 1e10 ? 0 : 1)} GB`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(0)} MB`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(0)} kB`;
  return `${n} B`;
}
