import { useState, type InputHTMLAttributes } from "react";
import { Button } from "./Button";

interface TextFieldProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "value" | "onChange"> {
  value: string;
  /** Called when the user presses Enter or clicks Save. */
  onApply: (value: string) => void | Promise<void>;
  /** Mask the value; a small eye toggle reveals it. */
  secret?: boolean;
  /** Empty the field once saved - for secrets that are never read back. */
  clearOnApply?: boolean;
}

/** An entry with an explicit Save step, like an Adw.EntryRow with an apply button. */
export function TextField({ value, onApply, secret, clearOnApply, className, ...rest }: TextFieldProps) {
  const [draft, setDraft] = useState(value);
  const [shown, setShown] = useState(false);
  const [busy, setBusy] = useState(false);

  // A new saved value replaces whatever was being typed.
  const [seen, setSeen] = useState(value);
  if (seen !== value) {
    setSeen(value);
    setDraft(value);
  }
  const dirty = draft !== value;

  const apply = async () => {
    if (!dirty) return;
    setBusy(true);
    try {
      await onApply(draft);
      if (clearOnApply) setDraft(value);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={"flex items-center gap-2 " + (className ?? "")}>
      <div className="control flex h-8 min-w-0 flex-1 items-center gap-1 pr-1">
        <input
          {...rest}
          type={secret && !shown ? "password" : "text"}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void apply();
            if (e.key === "Escape") setDraft(value);
          }}
          className="h-full min-w-0 flex-1 bg-transparent text-[13.5px] outline-none placeholder:text-fg-3"
          autoComplete="off"
          spellCheck={false}
        />
        {secret && (
          <button
            type="button"
            onClick={() => setShown((s) => !s)}
            aria-label={shown ? "Hide" : "Show"}
            className="flex h-6 w-6 items-center justify-center rounded text-fg-2 hover:bg-surface-3"
          >
            <EyeIcon open={shown} />
          </button>
        )}
      </div>
      {dirty && (
        <Button variant="suggested" busy={busy} onClick={() => void apply()}>
          Save
        </Button>
      )}
    </div>
  );
}

function EyeIcon({ open }: { open: boolean }) {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M1.5 8s2.5-4.5 6.5-4.5S14.5 8 14.5 8s-2.5 4.5-6.5 4.5S1.5 8 1.5 8Z" />
      <circle cx="8" cy="8" r="2" />
      {!open && <path d="M3 13 13 3" />}
    </svg>
  );
}
