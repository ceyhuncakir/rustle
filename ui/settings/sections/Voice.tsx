import { api, type ComputeReport, type GpuReport } from "../../shared/api";
import { useAsync } from "../../shared/hooks";
import { DownloadProgress, computePlan, deviceOptions, providerChoice, useModelDownload } from "../../shared/prefs";
import { Button, ChoiceRow, Group, Row, Select, descriptionOf, useToast, type Choice, type Option } from "../../shared/ui";
import { useSettings } from "../context";

const COMPUTE: readonly Choice[] = [
  { value: "auto", label: "Automatic", description: "Use the graphics card when it is faster than the CPU" },
  { value: "gpu", label: "GPU only", description: "Use the graphics card even when it is slower, and fail without one" },
  { value: "cpu", label: "CPU only", description: "Slower, but leaves the graphics card free" },
];

/** What runs now once the model is loaded; before that, what will. */
function computeSubtitle(report: ComputeReport | null, gpu: GpuReport | null, requested: string): string {
  if (report?.actual === "cuda" || report?.actual === "webgpu") return `Running on the ${gpu?.gpu ?? "GPU"}`;
  if (report?.actual === "cpu") return report.reason ? `Running on the CPU - ${report.reason}` : "Running on the CPU";
  if (gpu) return computePlan(gpu, requested).headline;
  return descriptionOf(COMPUTE, providerChoice(requested));
}

export function VoiceSection() {
  const { config, save } = useSettings();
  const toast = useToast();
  // Which files count as downloaded depends on where recognition runs.
  const models = useAsync(() => api.listSttModels(), [config.stt.provider]);
  const devices = useAsync(() => api.listInputDevices());
  const compute = useAsync(() => api.getComputeReport(), [config.stt.provider]);
  const gpu = useAsync(() => api.detectGpu());
  const fix = gpu.data && config.stt.provider !== "cpu" ? computePlan(gpu.data, config.stt.provider).fix : null;

  const selected = models.data?.find((m) => m.id === config.stt.model) ?? null;
  const download = useModelDownload(config.stt.model, {
    onDone: () => {
      toast("Model downloaded");
      void models.reload();
    },
    onError: (message) => toast(`Download failed: ${message}`),
  });

  const modelOptions: Option[] = (models.data ?? []).map((m) => ({ value: m.id, label: m.downloaded ? m.label : `${m.label}  (downloads on first use)` }));
  if (config.stt.model && !models.data?.some((m) => m.id === config.stt.model)) modelOptions.unshift({ value: config.stt.model, label: config.stt.model });

  const chooseModel = async (id: string) => {
    await save("stt", "model", id, false);
    const m = models.data?.find((x) => x.id === id);
    toast(m && !m.downloaded ? "Not downloaded yet - it downloads when Flow starts" : "Saved");
  };

  return (
    <Group title="Voice" description="How your speech becomes text">
      <Row
        title="Recognition model"
        htmlFor="stt-model"
        subtitle={selected?.description ?? (models.loading ? "Loading…" : models.error ?? "")}
        below={download.progress && <DownloadProgress event={download.progress} className="mt-3" />}
      >
        {selected && !selected.downloaded && (
          <Button busy={download.progress !== null} onClick={() => void download.start()}>
            {download.progress ? "Downloading" : "Download"}
          </Button>
        )}
        <Select id="stt-model" value={config.stt.model} options={modelOptions} onChange={(v) => void chooseModel(v)} />
      </Row>

      <ChoiceRow
        id="compute"
        title="Runs on"
        choices={COMPUTE}
        subtitle={computeSubtitle(compute.data, gpu.data, config.stt.provider)}
        below={fix && <p className="selectable mt-2 text-[12.5px] text-fg-2">To use the graphics card: {fix}</p>}
        value={providerChoice(config.stt.provider)}
        onChange={(v) => void save("stt", "provider", v)}
      />

      <Row title="Microphone" htmlFor="mic" subtitle={devices.error ? `Could not list devices: ${devices.error}` : "Which input to record from"}>
        <Select id="mic" value={config.audio.device} options={deviceOptions(devices.data, config.audio.device)} onChange={(v) => void save("audio", "device", v)} />
      </Row>
    </Group>
  );
}
