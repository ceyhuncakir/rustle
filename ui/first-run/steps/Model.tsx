import { useState } from "react";
import { api, type GpuReport } from "../../shared/api";
import { detach, useAction, useAsync } from "../../shared/hooks";
import { DownloadProgress, computePlan, formatMegabytes, useModelDownload } from "../../shared/prefs";
import { Button } from "../../shared/ui";
import { useWizard } from "../context";
import { Outcome, StepHeader } from "./StepHeader";

/** Which graphics card was found and whether recognition will use it. */
function ComputeCard({ gpu, provider, error }: { gpu: GpuReport | null; provider: string; error: string | null }) {
  if (!gpu) {
    return <div className="card mb-4 px-4 py-3 text-[13px] text-fg-2">{error ? `Could not check the graphics card: ${error}` : "Checking the graphics card…"}</div>;
  }
  const plan = computePlan(gpu, provider);
  const dot = plan.onGpu ? "bg-success" : plan.fix ? "bg-accent" : "bg-fg-3";
  return (
    <section aria-label="Graphics card" className="card mb-4 px-4 py-3">
      <p className="flex items-start gap-2.5 text-[14px]">
        <span aria-hidden="true" className={"mt-[7px] h-2 w-2 shrink-0 rounded-full " + dot} />
        <span className="selectable">{plan.headline}</span>
      </p>
      {plan.fix && <p className="selectable mt-1.5 pl-[18px] text-[13px] text-fg-2">To use it: {plan.fix}</p>}
      {gpu.devices.length > 0 && (
        <p className="selectable mt-1.5 pl-[18px] text-[12.5px] text-fg-3">Found: {gpu.devices.map((d) => d.name).join(", ")}</p>
      )}
    </section>
  );
}

export function ModelStep() {
  const { config, save } = useWizard();
  const models = useAsync(() => api.listSttModels());
  const gpu = useAsync(() => api.detectGpu());
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const download = useModelDownload({ onDone: () => void models.reload(), onError: setDownloadError });
  const test = useAction(() => api.wizardTestTranscribe());

  const selected = models.data?.find((m) => m.id === config.stt.model) ?? null;
  const downloading = download.progress !== null;

  return (
    <div>
      <StepHeader title="Recognition model" lead="Speech is recognised on this machine by a model downloaded once. Pick one, fetch it, then try a sentence." />
      <ComputeCard gpu={gpu.data} provider={config.stt.provider} error={gpu.error} />
      <ul role="radiogroup" aria-label="Recognition model" className="card divide-y divide-line">
        {(models.data ?? []).map((m) => {
          const active = m.id === config.stt.model;
          return (
            <li key={m.id}>
              <label className="flex cursor-default items-center gap-3 px-4 py-3">
                <input
                  type="radio"
                  name="stt-model"
                  className="accent-accent"
                  checked={active}
                  disabled={downloading}
                  onChange={() => detach(save("stt", "model", m.id))}
                />
                <span className="min-w-0 flex-1">
                  <span className="block text-[14px] font-medium">{m.label}</span>
                  <span className="block text-[12.5px] text-fg-2">{m.description}</span>
                </span>
                <span className={"shrink-0 text-[12.5px] " + (m.downloaded ? "text-success" : "text-fg-3")}>
                  {m.downloaded ? "Downloaded" : "Not downloaded"}
                </span>
              </label>
            </li>
          );
        })}
        {models.loading && <li className="px-4 py-3 text-[13px] text-fg-2">Loading…</li>}
      </ul>

      {(downloading || (selected && !selected.downloaded) || downloadError) && (
        <div className="mt-4 flex flex-col gap-2">
          {download.progress ? (
            <div className="flex items-center gap-3">
              <DownloadProgress event={download.progress} className="min-w-0 flex-1" />
              <Button variant="flat" busy={download.cancelling} onClick={() => void download.cancel()}>
                Cancel
              </Button>
            </div>
          ) : (
            selected &&
            !selected.downloaded && (
              <div className="flex items-center gap-3">
                <Button
                  variant="suggested"
                  onClick={() => {
                    setDownloadError(null);
                    void download.start(selected.id);
                  }}
                >
                  Download {selected.label}
                </Button>
                <span className="text-[13px] text-fg-2">
                  About {formatMegabytes(selected.download_mb)}
                  {selected.precision === "fp32" ? ", the full-precision version the graphics card runs." : "."}
                </span>
              </div>
            )
          )}
          {downloadError && <Outcome ok={false}>Download failed: {downloadError}</Outcome>}
        </div>
      )}

      {selected?.downloaded && (
        <div className="mt-5">
          <div className="flex items-center gap-3">
            <Button busy={test.busy} onClick={() => void test.start()}>
              {test.busy ? "Listening…" : "Test"}
            </Button>
            <span className="text-[13px] text-fg-2">{test.busy ? "Say a sentence. Recording stops on its own." : "Records a few seconds and shows what it heard."}</span>
          </div>
          {test.error && (
            <div className="mt-3">
              <Outcome ok={false}>{test.error}</Outcome>
            </div>
          )}
          {test.result && (
            <figure className="card mt-3 px-4 py-3">
              <blockquote className="selectable text-[15px] leading-relaxed">{test.result.text || <span className="text-fg-3">Nothing recognised</span>}</blockquote>
              <figcaption className="mt-1 text-[12.5px] text-fg-3">Recognised in {test.result.seconds.toFixed(1)} s</figcaption>
            </figure>
          )}
        </div>
      )}
    </div>
  );
}
