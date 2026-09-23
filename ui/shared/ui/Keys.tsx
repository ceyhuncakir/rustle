import { IS_MAC, keyLabel, partsOf, spokenCombo } from "../hotkey";

/** "Super+D" rendered as keycaps, in the platform's own names (⌘ on a Mac). */
export function Keys({ combo }: { combo: string }) {
  const parts = partsOf(combo);
  if (parts.length === 0) return <span className="text-fg-2">not set</span>;
  return (
    <span className="inline-flex items-center gap-1" aria-label={spokenCombo(combo)}>
      {parts.map((p, i) => (
        <span key={i} className="inline-flex items-center gap-1">
          {i > 0 && !IS_MAC && <span className="text-fg-3">+</span>}
          <kbd className="keycap">{keyLabel(p)}</kbd>
        </span>
      ))}
    </span>
  );
}
