import { partsOf } from "../hotkey";

/** "Super+D" rendered as keycaps. */
export function Keys({ combo }: { combo: string }) {
  const parts = partsOf(combo);
  if (parts.length === 0) return <span className="text-fg-2">not set</span>;
  return (
    <span className="inline-flex items-center gap-1" aria-label={combo}>
      {parts.map((p, i) => (
        <span key={i} className="inline-flex items-center gap-1">
          {i > 0 && <span className="text-fg-3">+</span>}
          <kbd className="keycap">{p}</kbd>
        </span>
      ))}
    </span>
  );
}
