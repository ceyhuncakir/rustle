import { StepHeader } from "./StepHeader";

export function WelcomeStep() {
  return (
    <div>
      <StepHeader title="Welcome to Rustle" />
      <div className="flex flex-col gap-5 text-[14.5px] leading-relaxed">
        <p className="max-w-[52ch]">
          Rustle is dictation for your whole desktop. Hold a key, say what you mean, and the cleaned-up text lands in whatever app
          has focus: punctuation added, fillers dropped, and the sentence you changed your mind about halfway through written the
          way you meant it.
        </p>
        <dl className="card grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 px-4 py-3 text-[13.5px]">
          <dt className="font-medium">Audio</dt>
          <dd className="text-fg-2">Never leaves this machine. Speech recognition runs locally, on your GPU when you have one.</dd>
          <dt className="font-medium">Text</dt>
          <dd className="text-fg-2">
            Goes only to the cleanup provider you choose in a moment. The default, Ollama, also runs on this machine.
          </dd>
          <dt className="font-medium">Licence</dt>
          <dd className="text-fg-2">Free software under the MIT licence. The source is yours to read and change.</dd>
        </dl>
        <p className="text-[13px] text-fg-3">Setup takes about two minutes. Every choice can be changed later in Settings.</p>
      </div>
    </div>
  );
}
