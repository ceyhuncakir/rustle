import { useRef, useState } from "react";
import { api } from "../../shared/api";
import { describe } from "../../shared/hooks";
import { Button } from "../../shared/ui";
import { PermissionRows } from "./Permissions";
import { Outcome, StepHeader } from "./StepHeader";

export function PasteStep() {
  const field = useRef<HTMLTextAreaElement>(null);
  const [text, setText] = useState("");
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; message: string } | null>(null);

  const test = async () => {
    const el = field.current;
    if (!el) return;
    setResult(null);
    setTesting(true);
    el.focus();
    const before = el.value;
    try {
      // Give focus a moment to settle before the native side types into it.
      await new Promise((r) => setTimeout(r, 150));
      const r = await api.wizardTestPaste();
      // Trust what actually happened in the field over what the native side reports.
      await new Promise((r) => setTimeout(r, 200));
      const after = field.current?.value ?? "";
      const landed = after !== before && after.length > before.length;
      setText(after);
      if (landed && r.ok) {
        setResult({ ok: true, message: `Text landed in the field. ${r.restored ? "Your clipboard was put back." : "Your clipboard now holds the pasted text."}` });
      } else if (landed) {
        setResult({ ok: true, message: `Text landed, although the app reported: ${r.detail}` });
      } else {
        setResult({ ok: false, message: r.ok ? "Nothing arrived in the field. Keep it focused and try again." : r.detail || "Paste failed" });
      }
    } catch (err) {
      setResult({ ok: false, message: describe(err) });
    } finally {
      setTesting(false);
    }
  };

  return (
    <div>
      <StepHeader title="Paste-back" lead="After a dictation Rustle types the text into whatever has focus. Check that it can reach this field." />
      <textarea
        ref={field}
        value={text}
        onChange={(e) => setText(e.target.value)}
        rows={4}
        placeholder="Click here, then press Test paste"
        className="control w-full resize-none rounded-card px-4 py-3 text-[14px] leading-relaxed placeholder:text-fg-3"
      />
      <div className="mt-3 flex items-center gap-3">
        <Button busy={testing} onClick={() => void test()}>
          Test paste
        </Button>
        {result && <Outcome ok={result.ok}>{result.message}</Outcome>}
      </div>
      <PermissionRows scope="paste" />
    </div>
  );
}
