import { useEffect, useState } from "react";
import { comboOf, isUsableCombo, keyNameOf, modifiersOf } from "../hotkey";
import { Button } from "./Button";
import { Keys } from "./Keys";

interface HotkeyRecorderProps {
  value: string;
  onSave: (combo: string) => void | Promise<void>;
  /** Live state of the shortcut, from `flow:hotkey`. */
  down?: boolean;
}

/** Click, press a combination, see it, save. Escape cancels. */
export function HotkeyRecorder({ value, onSave, down }: HotkeyRecorderProps) {
  const [recording, setRecording] = useState(false);
  const [pending, setPending] = useState<string[]>([]);
  const [candidate, setCandidate] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!recording) return;
    const onKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        setRecording(false);
        setPending([]);
        return;
      }
      const mods = modifiersOf(e);
      const key = keyNameOf(e);
      if (isUsableCombo(mods, key)) {
        setCandidate(comboOf(mods, key));
        setRecording(false);
        setPending([]);
      } else {
        setPending(mods);
      }
    };
    const onKeyUp = (e: KeyboardEvent) => {
      e.preventDefault();
      setPending(modifiersOf(e));
    };
    const stop = () => setRecording(false);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("keyup", onKeyUp, true);
    window.addEventListener("blur", stop);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("keyup", onKeyUp, true);
      window.removeEventListener("blur", stop);
    };
  }, [recording]);

  const shown = candidate ?? value;
  const changed = candidate !== null && candidate !== value;

  const save = async () => {
    if (!candidate) return;
    setBusy(true);
    try {
      await onSave(candidate);
      setCandidate(null);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex items-center gap-2">
      <button
        type="button"
        onClick={() => {
          setRecording(true);
          setPending([]);
        }}
        aria-pressed={recording}
        title={recording ? "Press the new shortcut, Escape to cancel" : "Click to change"}
        className={
          "control flex h-8 min-w-[150px] items-center justify-center gap-1 px-3 text-[13px] " +
          (recording ? "[box-shadow:inset_0_0_0_1px_var(--accent),0_0_0_3px_var(--accent-soft)]" : "")
        }
      >
        {recording ? (
          pending.length ? (
            <span className="flex items-center gap-1">
              <Keys combo={pending.join("+")} />
              <span className="text-fg-3">+ …</span>
            </span>
          ) : (
            <span className="text-fg-2">Press a combination…</span>
          )
        ) : (
          <span className="flex items-center gap-2">
            <Keys combo={shown} />
            {down !== undefined && (
              <span
                aria-label={down ? "pressed" : "released"}
                className={"h-2 w-2 rounded-full transition-colors " + (down ? "bg-accent" : "bg-surface-3 [box-shadow:inset_0_0_0_1px_var(--line-strong)]")}
              />
            )}
          </span>
        )}
      </button>
      {changed && (
        <>
          <Button variant="suggested" busy={busy} onClick={() => void save()}>
            Save
          </Button>
          <Button variant="flat" onClick={() => setCandidate(null)}>
            Cancel
          </Button>
        </>
      )}
    </div>
  );
}
