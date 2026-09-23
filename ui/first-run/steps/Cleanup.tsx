import { useState, type ReactNode } from "react";
import { api } from "../../shared/api";
import { detach, plural, useAction, useAsync } from "../../shared/hooks";
import { useApiKey } from "../../shared/prefs";
import { Button, Select, TextField } from "../../shared/ui";
import { useWizard } from "../context";
import { Outcome, StepHeader } from "./StepHeader";

type Choice = "none" | "ollama" | "cloud";

function choiceOf(backend: string): Choice {
  if (backend === "none") return "none";
  if (backend === "ollama") return "ollama";
  return "cloud";
}

export function CleanupStep() {
  const { config, save } = useWizard();
  const providers = useAsync(() => api.listProviders());
  const ollama = useAsync(() => api.listProviderModels("ollama").catch(() => [] as string[]));
  const [choice, setChoice] = useState<Choice>(() => choiceOf(config.cleanup.backend));
  const [cloudKey, setCloudKey] = useState<string>(() => (choiceOf(config.cleanup.backend) === "cloud" ? config.cleanup.backend : "anthropic"));
  // The keyring is only consulted once a cloud provider is in play.
  const apiKey = useApiKey(choice === "cloud" ? cloudKey : null);
  const test = useAction(() => api.wizardTestCleanup());

  const cloud = (providers.data ?? []).filter((p) => p.needs_api_key);
  const spec = providers.data?.find((p) => p.key === cloudKey) ?? null;

  const pick = async (next: Choice, provider?: string) => {
    setChoice(next);
    test.reset();
    const backend = next === "cloud" ? (provider ?? cloudKey) : next;
    const p = providers.data?.find((x) => x.key === backend);
    await save("cleanup", "backend", backend);
    if (p?.default_model && !p.suggested_models.includes(config.cleanup.model)) {
      await save("cleanup", "model", p.default_model);
    }
    if (next === "ollama") {
      const first = ollama.data?.[0];
      if (first && !ollama.data?.includes(config.cleanup.model)) await save("cleanup", "model", first);
    }
  };

  const installed = ollama.data?.length ?? 0;

  const option = (value: Choice, title: ReactNode, subtitle: ReactNode, body?: ReactNode) => (
    <li>
      <label className="flex cursor-default items-start gap-3 px-4 py-3">
        <input type="radio" name="cleanup" className="mt-[3px] accent-accent" checked={choice === value} onChange={() => detach(pick(value))} />
        <span className="min-w-0 flex-1">
          <span className="block text-[14px] font-medium">{title}</span>
          <span className="block text-[12.5px] text-fg-2">{subtitle}</span>
        </span>
      </label>
      {choice === value && body && <div className="px-4 pb-4 pl-[42px]">{body}</div>}
    </li>
  );

  return (
    <div>
      <StepHeader
        title="Cleanup"
        lead="A language model turns the raw transcript into what you meant: punctuation, dropped fillers, resolved changes of mind. Choose where it runs."
      />
      <ul role="radiogroup" aria-label="Cleanup provider" className="card divide-y divide-line">
        {option("none", "None", "Paste the transcript exactly as recognised.")}
        {option(
          "ollama",
          <>
            Ollama
            {installed > 0 && <span className="ml-2 rounded-full bg-success-soft px-2 py-0.5 text-[11.5px] font-medium text-success">detected</span>}
          </>,
          installed > 0
            ? `Runs on this machine, nothing leaves it. ${plural(installed, "model")} installed.`
            : ollama.loading
              ? "Checking for a local Ollama…"
              : "Not running. Install it from ollama.com and pull a model such as qwen3:14b, then come back.",
        )}
        {option(
          "cloud",
          "Cloud provider",
          "Claude, GPT, DeepSeek, OpenRouter or any OpenAI-compatible endpoint. Transcripts are sent to them.",
          <div className="flex flex-col gap-3">
            <label className="flex items-center justify-between gap-4">
              <span className="text-[13.5px]">Provider</span>
              <Select
                label="Cloud provider"
                value={cloudKey}
                options={cloud.map((p) => ({ value: p.key, label: p.label }))}
                onChange={(v) => {
                  setCloudKey(v);
                  detach(pick("cloud", v));
                }}
              />
            </label>
            {spec?.has_base_url && (
              <div>
                <div className="mb-1 text-[13.5px]">API address</div>
                <TextField value={config.cleanup.base_url} placeholder="https://api.groq.com/openai/v1" onApply={(v) => save("cleanup", "base_url", v.trim())} />
              </div>
            )}
            <div>
              <div className="mb-1 text-[13.5px]">
                {spec?.label ?? "API"} key
                {apiKey.fromEnv && <span className="ml-2 text-[12.5px] text-fg-2">using {apiKey.source}</span>}
                {apiKey.source === "keyring" && <span className="ml-2 text-[12.5px] text-success">saved</span>}
              </div>
              {!apiKey.fromEnv && <TextField key={cloudKey} value="" secret clearOnApply placeholder="Paste the key" onApply={apiKey.apply} />}
              {apiKey.error && <p className="selectable mt-1.5 text-[12.5px] text-danger">{apiKey.error}</p>}
            </div>
            {spec?.note && <p className="text-[12.5px] text-fg-2">{spec.note}</p>}
          </div>,
        )}
      </ul>

      {choice !== "none" && (
        <div className="mt-5">
          <div className="flex items-center gap-3">
            <Button busy={test.busy} onClick={() => void test.start()}>
              Test
            </Button>
            <span className="text-[13px] text-fg-2">Sends one sample sentence through the cleanup model.</span>
          </div>
          {test.error && (
            <div className="mt-3">
              <Outcome ok={false}>{test.error}</Outcome>
            </div>
          )}
          {test.result && (
            <div className="card mt-3 px-4 py-3">
              <Outcome ok={test.result.ok}>{test.result.ok ? "The model answered." : "The model did not answer."}</Outcome>
              {test.result.sample && <pre className="selectable mt-2 font-sans text-[13.5px] leading-relaxed whitespace-pre-wrap">{test.result.sample}</pre>}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
