import { api } from "../../shared/api";
import { detach, useAsync } from "../../shared/hooks";
import { useApiKey, useProviderModels } from "../../shared/prefs";
import { ChoiceRow, Combobox, Group, Row, Select, TextField, type Choice } from "../../shared/ui";
import { useSettings } from "../context";

const STYLES: readonly Choice[] = [
  { value: "light", label: "Light", description: "Punctuation only, wording untouched" },
  { value: "balanced", label: "Balanced", description: "Also fixes grammar and removes fillers" },
  { value: "tidy", label: "Tidy", description: "Also tightens loose phrasing" },
];

// The languages the recogniser knows; the same list as LANGUAGES in
// crates/rustle-core/src/cleanup.rs.
const SPOKEN: readonly (readonly [string, string])[] = [
  ["bg", "Bulgarian"],
  ["hr", "Croatian"],
  ["cs", "Czech"],
  ["da", "Danish"],
  ["nl", "Dutch"],
  ["en", "English"],
  ["et", "Estonian"],
  ["fi", "Finnish"],
  ["fr", "French"],
  ["de", "German"],
  ["el", "Greek"],
  ["hu", "Hungarian"],
  ["it", "Italian"],
  ["lv", "Latvian"],
  ["lt", "Lithuanian"],
  ["mt", "Maltese"],
  ["pl", "Polish"],
  ["pt", "Portuguese"],
  ["ro", "Romanian"],
  ["ru", "Russian"],
  ["sk", "Slovak"],
  ["sl", "Slovenian"],
  ["es", "Spanish"],
  ["sv", "Swedish"],
  ["uk", "Ukrainian"],
];

const LANGUAGES: readonly Choice[] = [
  { value: "same", label: "Same as spoken", description: "Whatever language you speak stays that language" },
  ...SPOKEN.map(([code, name]) => ({ value: code, label: `Always ${name}`, description: `Translates everything into ${name}` })),
];

export function CleanupSection() {
  const { config, save, latest } = useSettings();
  const providers = useAsync(() => api.listProviders());
  const provider = config.cleanup.backend;
  const spec = providers.data?.find((p) => p.key === provider) ?? null;
  // Ollama and "none" need no key, and asking the keyring can pop up its
  // unlock prompt.
  const apiKey = useApiKey(spec?.needs_api_key ? provider : null);

  const models = useProviderModels(provider, spec?.suggested_models ?? [], config.cleanup.model);
  const { fetched, fetching } = models;

  const showKey = Boolean(spec?.needs_api_key) && !apiKey.fromEnv;

  const modelSubtitle = fetched
    ? `${fetched.length} available ${provider === "ollama" ? "locally" : "from this provider"}`
    : fetching
      ? "Asking the provider what it offers…"
      : spec?.needs_api_key && provider !== "custom" && apiKey.source === "not set"
        ? "Set a key to load the full list"
        : "";

  // Both writes are asked for in the same tick and decided from the latest
  // choice, so arrowing through the list can't leave one provider's model
  // with another provider.
  const chooseProvider = async (key: string) => {
    const next = providers.data?.find((p) => p.key === key);
    const model = latest().cleanup.model;
    const writes = [save("cleanup", "backend", key)];
    if (next?.default_model && !next.suggested_models.includes(model)) {
      writes.push(save("cleanup", "model", next.default_model, false));
    }
    await Promise.all(writes);
  };

  const providerOptions = (providers.data ?? []).map((p) => ({ value: p.key, label: p.label }));
  if (providers.data && !providers.data.some((p) => p.key === provider)) providerOptions.unshift({ value: provider, label: provider });

  return (
    <Group title="Cleanup" description="The model that turns the transcript into what you meant">
      <Row
        title="Provider"
        htmlFor="provider"
        subtitle={
          providers.error ? (
            <span className="text-danger">Could not list the providers: {providers.error}</span>
          ) : apiKey.fromEnv ? (
            `Using the key from ${apiKey.source}`
          ) : (
            (spec?.note ?? "")
          )
        }
      >
        <Select id="provider" value={provider} options={providerOptions} onChange={(v) => detach(chooseProvider(v))} />
      </Row>

      {spec?.has_base_url && (
        <Row title="API address" htmlFor="base-url" subtitle="The OpenAI-compatible base URL, ending in /v1" stacked>
          <TextField
            id="base-url"
            value={config.cleanup.base_url}
            placeholder="https://api.groq.com/openai/v1"
            onApply={async (v) => {
              await save("cleanup", "base_url", v.trim());
              void models.refresh();
            }}
          />
        </Row>
      )}

      {provider !== "none" && (
        <Row title="Model" subtitle={modelSubtitle}>
          <Combobox
            label="Model"
            className="w-[280px]"
            value={config.cleanup.model}
            options={models.choices}
            loading={fetching}
            placeholder="Type to search"
            onChange={(v) => {
              if (v && v !== config.cleanup.model) detach(save("cleanup", "model", v));
            }}
          />
        </Row>
      )}

      {showKey && (
        <Row
          title={`${spec?.label ?? "API"} API key`}
          htmlFor="api-key"
          subtitle={apiKey.source === "keyring" ? "Stored in your keyring. Enter a new one to replace it, or clear it to remove." : "Kept in your keyring, never in config.toml"}
          stacked
          below={apiKey.error && <p className="selectable mt-2 text-[12.5px] text-danger">{apiKey.error}</p>}
        >
          <TextField
            id="api-key"
            key={provider}
            value=""
            secret
            clearOnApply
            placeholder={apiKey.source === "keyring" ? "••••••••••••" : "Paste the key"}
            onApply={async (key) => {
              if (await apiKey.apply(key)) void models.refresh();
            }}
          />
        </Row>
      )}

      <ChoiceRow id="style" title="Editing" choices={STYLES} value={config.cleanup.style} onChange={(v) => detach(save("cleanup", "style", v))} />
      <ChoiceRow
        id="lang"
        title="Output language"
        choices={LANGUAGES}
        value={config.cleanup.output_language}
        onChange={(v) => detach(save("cleanup", "output_language", v))}
      />
    </Group>
  );
}
