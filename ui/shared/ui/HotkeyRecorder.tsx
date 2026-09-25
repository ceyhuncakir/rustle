import { useEffect, useState } from "react";
import { IS_MAC, comboOf, isModifierOnly, isUsableCombo, keyNameOf, modifiersOf } from "../hotkey";
import { Button } from "./Button";
import { Keys } from "./Keys";

interface HotkeyRecorderProps {
  value: string;
  /** Reject (after telling the user why) to keep the new combination on screen for another try. */
  onSave: (combo: string) => void | Promise<unknown>;
  /** Live state of the shortcut, from `rustle:hotkey`. */
  down?: boolean;
}

const NEEDS_MODIFIER = IS_MAC ? "Add ⌃, ⌥ or ⌘" : "Add Ctrl, Alt or Super";

/** Click, press a combination, see it, save. Escape cancels. */
export function HotkeyRecorder({ value, onSave, down }: HotkeyRecorderProps) {
  const [recording, setRecording] = useState(false);
  const [pending, setPending] = useState<string[]>([]);
  /** Why the last key pressed while recording was not taken. */
  const [hint, setHint] = useState<string | null>(null);
  const [candidate, setCandidate] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);

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
        setFailed(false);
        setRecording(false);
        setPending([]);
      } else {
        setPending(mods);
        if (!isModifierOnly(e)) setHint(key ? NEEDS_MODIFIER : `${e.key === " " ? "Space" : e.key} can't be a shortcut`);
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
      setFailed(false);
    } catch {
      // The caller has said why; the candidate stays for another try.
      setFailed(true);
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
          setHint(null);
        }}
        aria-pressed={recording}
        aria-invalid={failed || undefined}
        title={recording ? "Press the new shortcut, Escape to cancel" : "Click to change"}
        className={
          "control flex h-8 min-w-[150px] items-center justify-center gap-1 px-3 text-[13px] " +
          (recording
            ? "[box-shadow:inset_0_0_0_1px_var(--accent),0_0_0_3px_var(--accent-soft)]"
            : failed
              ? "[box-shadow:inset_0_0_0_1px_var(--danger)]"
              : "")
        }
      >
        {recording ? (
          pending.length ? (
            <span className="flex items-center gap-1">
              <Keys combo={pending.join("+")} />
              <span className="text-fg-3">+ …</span>
            </span>
          ) : (
            <span className="text-fg-2">{hint ?? "Press a combination…"}</span>
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
          <Button
            variant="flat"
            onClick={() => {
              setCandidate(null);
              setFailed(false);
            }}
          >
            Cancel
          </Button>
        </>
      )}
    </div>
  );
}
