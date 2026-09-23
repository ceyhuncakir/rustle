import { useState } from "react";
import { Keys } from "../../shared/ui";
import { useWizard } from "../context";
import { StepHeader } from "./StepHeader";

export function DoneStep() {
  const { hotkey, config } = useWizard();
  const [text, setText] = useState("");

  return (
    <div>
      <StepHeader title="Ready" lead="Everything is in place. Try it here before you go." />
      <div className="card p-4">
        <p className="mb-3 flex flex-wrap items-center gap-1.5 text-[14px]">
          Click the box, hold <Keys combo={hotkey} /> and talk here.
        </p>
        <textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          rows={5}
          placeholder={`Hold ${hotkey} and talk here`}
          className="control w-full resize-none rounded-card px-4 py-3 text-[15px] leading-relaxed placeholder:text-fg-3"
        />
      </div>
      <dl className="mt-6 grid grid-cols-[auto_1fr] gap-x-5 gap-y-1.5 text-[13px]">
        <dt className="text-fg-2">Recognition</dt>
        <dd className="font-mono text-[12.5px]">{config.stt.model}</dd>
        <dt className="text-fg-2">Cleanup</dt>
        <dd className="font-mono text-[12.5px]">
          {config.cleanup.backend === "none" ? "none" : `${config.cleanup.backend} / ${config.cleanup.model}`}
        </dd>
        <dt className="text-fg-2">Learning</dt>
        <dd>{config.learning.enabled ? "On" : "Off"}</dd>
      </dl>
      <p className="mt-5 text-[13px] text-fg-3">The settings window has everything on these pages, and a few things more.</p>
    </div>
  );
}
