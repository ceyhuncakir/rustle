//! The model providers behind the cleanup pass.
//!
//! Three interchangeable backends behind one interface, so the choice between
//! a local model and a hosted one is a dropdown rather than a rewrite:
//!
//!   ollama    - local, offline, free, nothing leaves the machine (the default)
//!   anthropic - Claude, through the Messages API
//!   openai    - GPT, and anything else speaking the OpenAI protocol
//!
//! Cleanup is a short, latency-sensitive rewrite, so the hosted backends are
//! configured for speed rather than depth: low effort, no streaming, small
//! output cap. The whole dictation waits on this call.
//!
//! All HTTP here is blocking: the engine calls backends from plain worker
//! threads, never from an async runtime.

use std::fmt;
use std::time::{Duration, Instant};

use log::{info, warn};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

use crate::config::CleanupConfig;
use crate::secrets;

/// Cleanup output is bounded by what the speaker said; this is generous.
const MAX_TOKENS: u32 = 4096;

/// Where the OpenAI protocol lives when a provider names no other address.
const OPENAI_API_URL: &str = "https://api.openai.com/v1";
const ANTHROPIC_API_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BackendError(pub String);

/// Anything that can rewrite a transcript.
pub trait Backend: Send + Sync {
    fn complete(
        &self,
        system: &str,
        prompt: &str,
        timeout_secs: f32,
        thinking: bool,
    ) -> Result<String, BackendError>;
    fn available(&self) -> (bool, String);
    /// The provider's name as it appears in error messages.
    fn label(&self) -> String;
    /// What the provider can serve; empty when it cannot be asked.
    fn installed_models(&self) -> Vec<String> {
        Vec::new()
    }
    /// Seconds spent. Hosted models are always warm, so by default there is
    /// nothing to do.
    fn warm_up(&self) -> f64 {
        0.0
    }
    /// Hand back whatever the model holds; nothing, for a hosted backend.
    fn unload(&self) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    pub key: &'static str,
    pub label: &'static str,
    pub needs_api_key: bool,
    pub default_model: &'static str,
    /// Offered in the GUI before a key is set. Once there is one, the live
    /// /models endpoint replaces this, so it only has to be a starting point.
    pub suggested_models: &'static [&'static str],
    /// Set for anything speaking the OpenAI protocol at a non-OpenAI address.
    pub base_url: Option<&'static str>,
    /// Env var checked before the keyring.
    pub env_var: Option<&'static str>,
    pub note: &'static str,
    /// Catalogue readable without authentication, when the provider has one.
    pub public_models_url: Option<&'static str>,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        key: "ollama",
        label: "Ollama (local)",
        needs_api_key: false,
        default_model: "qwen3:14b",
        suggested_models: &[
            "qwen3:14b",
            "qwen3:8b",
            "qwen3:4b",
            "llama3.1:8b",
            "mistral-nemo:12b",
            "gemma3:12b",
            "phi4:14b",
        ],
        base_url: None,
        env_var: None,
        note: "Runs on this machine. Nothing leaves it.",
        public_models_url: None,
    },
    Provider {
        key: "anthropic",
        label: "Anthropic Claude",
        needs_api_key: true,
        default_model: "claude-opus-5",
        suggested_models: &["claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"],
        base_url: None,
        env_var: Some("ANTHROPIC_API_KEY"),
        note: "",
        public_models_url: None,
    },
    Provider {
        key: "openai",
        label: "OpenAI",
        needs_api_key: true,
        default_model: "gpt-5",
        suggested_models: &["gpt-5", "gpt-5-mini", "gpt-4.1", "gpt-4.1-mini"],
        base_url: None,
        env_var: Some("OPENAI_API_KEY"),
        note: "",
        public_models_url: None,
    },
    // Model IDs here go stale fast - OpenRouter publishes its catalogue
    // without authentication, so the real list is always fetched and this
    // is only what shows if the network is down.
    Provider {
        key: "openrouter",
        label: "OpenRouter",
        needs_api_key: true,
        default_model: "deepseek/deepseek-v4.1-flash",
        suggested_models: &["deepseek/deepseek-v4.1-flash"],
        base_url: Some("https://openrouter.ai/api/v1"),
        env_var: Some("OPENROUTER_API_KEY"),
        note: "Hundreds of models from one key. The full list loads below.",
        public_models_url: Some("https://openrouter.ai/api/v1/models"),
    },
    Provider {
        key: "deepseek",
        label: "DeepSeek",
        needs_api_key: true,
        default_model: "deepseek-chat",
        suggested_models: &["deepseek-chat", "deepseek-reasoner"],
        base_url: Some("https://api.deepseek.com"),
        env_var: Some("DEEPSEEK_API_KEY"),
        note: "",
        public_models_url: None,
    },
    Provider {
        key: "custom",
        label: "Other (OpenAI-compatible)",
        needs_api_key: true,
        default_model: "",
        suggested_models: &[],
        base_url: None,
        env_var: Some("FLOW_API_KEY"),
        note: "Any OpenAI-compatible endpoint: Groq, Together, Fireworks, vLLM, llama.cpp, LM Studio. A local server usually needs no key.",
        public_models_url: None,
    },
    Provider {
        key: "none",
        label: "No cleanup (raw transcript)",
        needs_api_key: false,
        default_model: "",
        suggested_models: &[],
        base_url: None,
        env_var: None,
        note: "Paste the transcript exactly as recognised.",
        public_models_url: None,
    },
];

pub fn provider(key: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.key == key)
}

/// Substrings that mark a model as unable to answer a chat completion.
/// OpenAI's /v1/models returns its whole catalogue - embeddings, speech,
/// images and moderation included - and offering those as a cleanup model is
/// a guaranteed runtime error. Excluding known families beats allow-listing,
/// because new chat models appear constantly and an allow-list would hide
/// them.
const NON_CHAT_MARKERS: &[&str] = &[
    "embedding",
    "tts",
    "whisper",
    "transcribe",
    "dall-e",
    "moderation",
    "image",
    "audio",
    "realtime",
    "sora",
    "babbage",
    "davinci",
    "search",
    "computer-use",
    "codex-mini",
];

/// Drop anything that cannot serve a normal chat completion.
pub fn usable_chat_models(ids: &[String], provider: &str) -> Vec<String> {
    let mut kept: Vec<String> = ids
        .iter()
        // ":batch" variants exist only for a provider's batch endpoint and
        // reject a synchronous request.
        .filter(|model| !model.ends_with(":batch"))
        .filter(|model| {
            let lower = model.to_lowercase();
            provider != "openai" || !NON_CHAT_MARKERS.iter().any(|marker| lower.contains(marker))
        })
        .cloned()
        .collect();
    kept.sort();
    kept
}

// -- shared plumbing --------------------------------------------------------

fn http_client() -> Client {
    Client::builder().build().expect("build HTTP client")
}

/// A hosted backend's API key. Production reads the environment and the
/// keyring through [`secrets`]; tests pin a value so they neither touch the
/// keyring nor depend on whatever the developer has exported.
fn resolve_key(pinned: Option<&str>, provider: &str) -> String {
    pinned.map(str::to_string).unwrap_or_else(|| secrets::get_key(provider))
}

/// The error for a missing key, naming both places one can be set.
fn missing_key(label: &str, env_var: Option<&str>) -> BackendError {
    let mut message = format!("no {label} API key - set one in the settings window");
    if let Some(env) = env_var {
        message.push_str(&format!(", or export {env}"));
    }
    BackendError(message)
}

/// Why a request to a hosted provider failed, in the two shapes the Python
/// SDKs distinguished: the server answered with an error status, or it could
/// not be reached at all (which includes a timeout).
#[derive(Debug)]
enum Failure {
    Status { code: u16, body: String },
    Transport(reqwest::Error),
}

impl Failure {
    /// The availability check shows `"{ExceptionName}: {detail}"` truncated
    /// to 60 characters, as the SDK exceptions rendered; the names here are
    /// the ones both SDKs use for those statuses so the GUI text is unchanged.
    fn sdk_style(&self) -> String {
        let (name, detail) = match self {
            Failure::Status { code, body } => {
                let name = match *code {
                    400 => "BadRequestError",
                    401 => "AuthenticationError",
                    403 => "PermissionDeniedError",
                    404 => "NotFoundError",
                    409 => "ConflictError",
                    422 => "UnprocessableEntityError",
                    429 => "RateLimitError",
                    500.. => "InternalServerError",
                    _ => "APIStatusError",
                };
                (name, format!("Error code: {code} - {body}"))
            }
            Failure::Transport(err) if err.is_timeout() => ("APITimeoutError", err.to_string()),
            Failure::Transport(err) => ("APIConnectionError", err.to_string()),
        };
        format!("{name}: {}", truncate_chars(&detail, 60))
    }

    /// What a failed completion tells the user. It names the provider, so a
    /// rejected key on OpenRouter does not send them to OpenAI's dashboard.
    fn for_user(&self, label: &str, model: &str) -> String {
        match self {
            Failure::Status { code: 401, .. } => format!("{label} rejected the API key"),
            Failure::Status { code: 429, .. } => format!("{label} rate limit reached"),
            Failure::Status { code: 404, .. } => format!("{label} has no model {model:?}"),
            Failure::Status { code, .. } => format!("{label} error {code}"),
            Failure::Transport(err) if err.is_decode() => {
                format!("{label} returned an unreadable reply: {err}")
            }
            Failure::Transport(_) => format!("could not reach {label}"),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Status { code, body } => write!(f, "HTTP {code}: {}", truncate_chars(body, 200)),
            Failure::Transport(err) => write!(f, "{err}"),
        }
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Bounds on a request timeout. The cleanup timeout comes from
/// config.toml, where it can be anything TOML can spell - `inf` and `nan`
/// included, and thinking multiplies it by four - and `Duration` panics on
/// anything infinite, NaN or too large.
const MIN_TIMEOUT_SECS: f32 = 1.0;
const MAX_TIMEOUT_SECS: f32 = 600.0;

fn timeout(secs: f32) -> Duration {
    // NaN says nothing about how long to wait, so it gets the patient end.
    let secs = if secs.is_nan() { MAX_TIMEOUT_SECS } else { secs.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS) };
    Duration::from_secs_f32(secs)
}

/// Send with a timeout and insist on a 2xx.
fn send(request: RequestBuilder, timeout_secs: f32) -> Result<Response, Failure> {
    let response = request.timeout(timeout(timeout_secs)).send().map_err(Failure::Transport)?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    Err(Failure::Status { code: status.as_u16(), body: response.text().unwrap_or_default() })
}

/// [`send`], then read the JSON body.
fn send_json<T: DeserializeOwned>(request: RequestBuilder, timeout_secs: f32) -> Result<T, Failure> {
    send(request, timeout_secs)?.json().map_err(Failure::Transport)
}

/// The `/models` reply shape OpenAI and Anthropic share.
#[derive(Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    #[serde(default)]
    id: String,
}

/// The ids a catalogue request returns; empty when it fails, because a
/// listing must never surface an error into the UI thread.
fn list_models(request: RequestBuilder) -> Vec<String> {
    send_json::<ModelList>(request, 10.0)
        .map(|list| list.data.into_iter().map(|m| m.id).filter(|id| !id.is_empty()).collect())
        .unwrap_or_default()
}

/// The availability verdict for a probe request.
fn probed(result: Result<Response, Failure>) -> (bool, String) {
    match result {
        Ok(_) => (true, "ok".into()),
        Err(err) => (false, err.sdk_style()),
    }
}

// -- Ollama -----------------------------------------------------------------

/// Local models through Ollama's HTTP API.
pub struct OllamaBackend {
    model: String,
    endpoint: String,
    keep_alive: String,
    client: Client,
}

#[derive(Deserialize)]
struct GenerateReply {
    response: Option<String>,
}

#[derive(Deserialize)]
struct TagsReply {
    #[serde(default)]
    models: Vec<TagEntry>,
}

#[derive(Deserialize)]
struct TagEntry {
    name: String,
}

impl OllamaBackend {
    pub fn new(model: impl Into<String>, endpoint: &str, keep_alive: impl Into<String>) -> OllamaBackend {
        OllamaBackend {
            model: model.into(),
            endpoint: endpoint.trim_end_matches('/').to_string(),
            keep_alive: keep_alive.into(),
            client: http_client(),
        }
    }

    fn generate(&self, body: &serde_json::Value) -> RequestBuilder {
        self.client.post(format!("{}/api/generate", self.endpoint)).json(body)
    }

    fn tags(&self) -> Result<Vec<String>, Failure> {
        let reply: TagsReply = send_json(self.client.get(format!("{}/api/tags", self.endpoint)), 3.0)?;
        Ok(reply.models.into_iter().map(|m| m.name).collect())
    }
}

impl Backend for OllamaBackend {
    fn complete(
        &self,
        system: &str,
        prompt: &str,
        timeout_secs: f32,
        thinking: bool,
    ) -> Result<String, BackendError> {
        let body = json!({
            "model": self.model,
            "system": system,
            "prompt": prompt,
            "stream": false,
            "think": thinking,
            "keep_alive": self.keep_alive,
            "options": {"temperature": 0.1},
        });
        let reply: GenerateReply =
            send_json(self.generate(&body), timeout_secs).map_err(|err| BackendError(err.to_string()))?;
        Ok(reply.response.unwrap_or_default())
    }

    fn available(&self) -> (bool, String) {
        let names = match self.tags() {
            Ok(names) => names,
            Err(err) => return (false, format!("Ollama unreachable at {}: {err}", self.endpoint)),
        };

        let wanted = self.model.as_str();
        if !names.iter().any(|n| n == wanted || n.split(':').next() == Some(wanted)) {
            let have = if names.is_empty() { "none".to_string() } else { names.join(", ") };
            return (false, format!("model {wanted:?} not pulled (have: {have})"));
        }
        (true, "ok".into())
    }

    fn label(&self) -> String {
        "Ollama".into()
    }

    fn installed_models(&self) -> Vec<String> {
        self.tags()
            .map(|mut names| {
                names.sort();
                names
            })
            .unwrap_or_default()
    }

    fn warm_up(&self) -> f64 {
        let started = Instant::now();
        let body = json!({
            "model": self.model,
            "prompt": "hi",
            "stream": false,
            "think": false,
            "keep_alive": self.keep_alive,
            "options": {"num_predict": 1},
        });
        if let Err(err) = send(self.generate(&body), 180.0) {
            warn!("could not warm up {}: {err}", self.model);
            return 0.0;
        }
        started.elapsed().as_secs_f64()
    }

    fn unload(&self) {
        // Short: this runs on the way out, and Windows gives an app a few
        // seconds to end when the user logs off. Ollama answers at once.
        match send(self.generate(&json!({"model": self.model, "keep_alive": 0})), 3.0) {
            Ok(_) => info!("unloaded {} from Ollama", self.model),
            Err(err) => warn!("could not unload {} from Ollama: {err}", self.model),
        }
    }
}

// -- Anthropic --------------------------------------------------------------

/// Claude through the Messages API.
pub struct AnthropicBackend {
    model: String,
    base_url: String,
    /// See [`resolve_key`].
    key: Option<String>,
    client: Client,
}

#[derive(Deserialize)]
struct MessagesReply {
    #[serde(default)]
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

impl AnthropicBackend {
    const KEY: &'static str = "anthropic";

    pub fn new(model: impl Into<String>) -> AnthropicBackend {
        AnthropicBackend {
            model: model.into(),
            base_url: ANTHROPIC_API_URL.into(),
            key: None,
            client: http_client(),
        }
    }

    /// Point at another server - a proxy, or a mock in tests.
    pub fn with_base_url(mut self, base_url: &str) -> AnthropicBackend {
        self.base_url = base_url.trim_end_matches('/').to_string();
        self
    }

    /// Use this key instead of consulting the environment and the keyring.
    #[cfg(test)]
    pub fn with_api_key(mut self, key: impl Into<String>) -> AnthropicBackend {
        self.key = Some(key.into());
        self
    }

    fn api_key(&self) -> Result<String, BackendError> {
        let key = resolve_key(self.key.as_deref(), Self::KEY);
        if key.is_empty() {
            return Err(missing_key("Anthropic", provider(Self::KEY).and_then(|p| p.env_var)));
        }
        Ok(key)
    }

    fn request(&self, method: Method, path: &str, key: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base_url))
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION)
    }
}

impl Backend for AnthropicBackend {
    fn complete(
        &self,
        system: &str,
        prompt: &str,
        timeout_secs: f32,
        thinking: bool,
    ) -> Result<String, BackendError> {
        let key = self.api_key()?;
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "messages": [{"role": "user", "content": prompt}],
            // Cleanup is a short rewrite and the user is waiting on it, so
            // keep thinking shallow rather than switching it off - disabling
            // it on Opus 5 can leak reasoning into the visible answer.
            "output_config": {"effort": if thinking { "high" } else { "low" }},
        });
        let reply: MessagesReply =
            send_json(self.request(Method::POST, "/v1/messages", &key).json(&body), timeout_secs)
                .map_err(|err| BackendError(err.for_user("Anthropic", &self.model)))?;
        if reply.stop_reason.as_deref() == Some("refusal") {
            return Err(BackendError("Anthropic declined to process this transcript".into()));
        }
        Ok(reply.content.iter().filter(|b| b.kind == "text").map(|b| b.text.as_str()).collect())
    }

    fn available(&self) -> (bool, String) {
        let key = resolve_key(self.key.as_deref(), Self::KEY);
        if key.is_empty() {
            return (false, "no API key set".into());
        }
        probed(send(self.request(Method::GET, &format!("/v1/models/{}", self.model), &key), 10.0))
    }

    fn label(&self) -> String {
        "Anthropic".into()
    }

    fn installed_models(&self) -> Vec<String> {
        let Ok(key) = self.api_key() else { return Vec::new() };
        // The API pages at 20 by default; asking for a larger page just means
        // fewer round trips.
        let mut ids = list_models(self.request(Method::GET, "/v1/models?limit=1000", &key));
        ids.sort();
        ids
    }
}

// -- OpenAI and everything that speaks its protocol -------------------------

/// Anything speaking the OpenAI protocol.
///
/// OpenAI itself, but also OpenRouter, DeepSeek, Groq, Together, vLLM,
/// llama.cpp and LM Studio - they differ only by base URL and which key opens
/// them. One implementation covers the lot, so adding a provider is a row in
/// [`PROVIDERS`] rather than a new type.
pub struct OpenAICompatibleBackend {
    model: String,
    provider: String,
    base_url: Option<String>,
    public_models_url: Option<String>,
    /// See [`resolve_key`].
    key: Option<String>,
    client: Client,
}

#[derive(Deserialize)]
struct ChatReply {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

impl OpenAICompatibleBackend {
    /// `base_url` overrides the provider's own address; empty or `None`
    /// means the provider's, and for OpenAI itself the official endpoint.
    pub fn new(model: impl Into<String>, provider: &str, base_url: Option<&str>) -> OpenAICompatibleBackend {
        let spec = self::provider(provider);
        OpenAICompatibleBackend {
            model: model.into(),
            provider: provider.to_string(),
            base_url: base_url
                .filter(|url| !url.is_empty())
                .or_else(|| spec.and_then(|s| s.base_url))
                .map(String::from),
            public_models_url: spec.and_then(|s| s.public_models_url).map(String::from),
            key: None,
            client: http_client(),
        }
    }

    /// Use this key instead of consulting the environment and the keyring.
    #[cfg(test)]
    pub fn with_api_key(mut self, key: impl Into<String>) -> OpenAICompatibleBackend {
        self.key = Some(key.into());
        self
    }

    /// Read the public catalogue from here instead of the provider's address.
    #[cfg(test)]
    pub fn with_public_models_url(mut self, url: &str) -> OpenAICompatibleBackend {
        self.public_models_url = Some(url.to_string());
        self
    }

    /// `None` means the SDK default address, i.e. OpenAI itself.
    #[cfg(test)]
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    fn provider_label(&self) -> String {
        provider(&self.provider).map(|p| p.label.to_string()).unwrap_or_else(|| self.provider.clone())
    }

    /// A local server (llama.cpp, LM Studio, vLLM) usually runs without a
    /// key; only the named providers always want one.
    fn key_required(&self) -> bool {
        self.provider != "custom"
    }

    /// The key and address needed before any request can be made, with the
    /// same messages the Python client construction raised. The key may be
    /// empty for a custom endpoint.
    fn credentials(&self) -> Result<String, BackendError> {
        let key = resolve_key(self.key.as_deref(), &self.provider);
        if key.is_empty() && self.key_required() {
            let env = provider(&self.provider).and_then(|p| p.env_var);
            return Err(missing_key(&self.provider_label(), env));
        }
        if self.provider == "custom" && self.base_url.is_none() {
            return Err(BackendError(
                "no base URL set - a custom provider needs the address of its OpenAI-compatible endpoint"
                    .into(),
            ));
        }
        Ok(key)
    }

    fn request(&self, method: Method, path: &str, key: &str) -> RequestBuilder {
        let base = self.base_url.as_deref().unwrap_or(OPENAI_API_URL).trim_end_matches('/');
        let request = self.client.request(method, format!("{base}{path}"));
        if key.is_empty() {
            request
        } else {
            request.bearer_auth(key)
        }
    }
}

impl Backend for OpenAICompatibleBackend {
    fn complete(
        &self,
        system: &str,
        prompt: &str,
        timeout_secs: f32,
        _thinking: bool,
    ) -> Result<String, BackendError> {
        let key = self.credentials()?;
        let label = self.provider_label();
        let body = json!({
            "model": self.model,
            "max_completion_tokens": MAX_TOKENS,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": prompt},
            ],
        });
        let reply: ChatReply =
            send_json(self.request(Method::POST, "/chat/completions", &key).json(&body), timeout_secs)
                .map_err(|err| BackendError(err.for_user(&label, &self.model)))?;
        let choice = reply
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| BackendError(format!("{label} returned no choices")))?;
        // Reasoning models on some routers return the answer alongside a
        // separate reasoning field; only the answer should be pasted.
        Ok(choice.message.content.unwrap_or_default().trim().to_string())
    }

    fn available(&self) -> (bool, String) {
        let key = resolve_key(self.key.as_deref(), &self.provider);
        if key.is_empty() && self.key_required() {
            return (false, "no API key set".into());
        }
        if self.provider == "custom" && self.base_url.is_none() {
            return (false, "no base URL set".into());
        }
        probed(send(self.request(Method::GET, &format!("/models/{}", self.model), &key), 10.0))
    }

    fn label(&self) -> String {
        self.provider_label()
    }

    fn installed_models(&self) -> Vec<String> {
        // A public catalogue means the dropdown can be filled before the user
        // has pasted a key, which is when they most want to browse it.
        let mut found = match &self.public_models_url {
            Some(url) => list_models(self.client.get(url)),
            None => Vec::new(),
        };
        if found.is_empty() {
            let Ok(key) = self.credentials() else { return Vec::new() };
            found = list_models(self.request(Method::GET, "/models", &key));
        }
        usable_chat_models(&found, &self.provider)
    }
}

// -- selection ----------------------------------------------------------------

/// The backend named in the config: Anthropic by name, any other provider
/// that needs a key through the OpenAI protocol, and everything else -
/// including "none" - through Ollama.
pub fn build_backend(config: &CleanupConfig) -> Box<dyn Backend> {
    let backend = config.backend.as_str();
    if backend == "anthropic" {
        return Box::new(AnthropicBackend::new(&config.model));
    }
    if provider(backend).is_some_and(|p| p.needs_api_key) {
        return Box::new(openai_compatible(config));
    }
    Box::new(OllamaBackend::new(&config.model, &config.endpoint, &config.keep_alive))
}

/// Only "custom" is handed the configured base URL. It stays in the file
/// after switching to a named provider, and passing it on would send that
/// provider's key to whatever host the custom one pointed at.
fn openai_compatible(config: &CleanupConfig) -> OpenAICompatibleBackend {
    let base_url = (config.backend == "custom").then_some(config.base_url.as_str());
    OpenAICompatibleBackend::new(&config.model, &config.backend, base_url)
}

#[cfg(test)]
mod tests {
    //! Backend selection and the provider catalogue.
    //!
    //! The risk here is quiet misconfiguration: a provider switch that leaves
    //! the previous provider's model behind, or an error that names the wrong
    //! provider and sends the user to the wrong dashboard.

    use super::*;
    use crate::test_util::strings;
    use httpmock::prelude::*;
    use serde_json::json;

    fn cfg(backend: &str, model: &str) -> CleanupConfig {
        CleanupConfig { backend: backend.into(), model: model.into(), ..Default::default() }
    }

    // -- the catalogue --------------------------------------------------------

    #[test]
    fn every_provider_is_described() {
        for spec in PROVIDERS {
            assert_eq!(provider(spec.key), Some(spec));
            assert!(!spec.label.is_empty());
            // "none" has no model, and "custom" cannot have one - the user
            // brings their own endpoint. Everything else must offer its
            // default.
            if !spec.default_model.is_empty() {
                assert!(spec.suggested_models.contains(&spec.default_model), "{}", spec.key);
            }
        }
    }

    #[test]
    fn custom_provider_has_no_preset_model_or_url() {
        // It is the escape hatch: the user supplies both.
        let spec = provider("custom").unwrap();
        assert!(spec.needs_api_key);
        assert!(spec.default_model.is_empty());
        assert_eq!(spec.base_url, None);
    }

    #[test]
    fn routers_carry_a_base_url() {
        for key in ["openrouter", "deepseek"] {
            let spec = provider(key).unwrap();
            assert!(spec.base_url.unwrap().starts_with("https://"), "{key}");
            assert!(spec.needs_api_key);
        }
    }

    #[test]
    fn openai_uses_the_sdk_default_address() {
        assert_eq!(provider("openai").unwrap().base_url, None);
    }

    #[test]
    fn env_vars_match_the_provider_catalogue() {
        // secrets duplicates this mapping to stay free of a dependency on
        // this module, so drift is caught here rather than by a key silently
        // not being found.
        for spec in PROVIDERS {
            if spec.needs_api_key {
                assert_eq!(secrets::env_var(spec.key), spec.env_var, "{}", spec.key);
            } else {
                assert_eq!(secrets::env_var(spec.key), None, "{}", spec.key);
            }
        }
        assert_eq!(secrets::ENV_VARS.len(), PROVIDERS.iter().filter(|p| p.needs_api_key).count());
    }

    #[test]
    fn hosted_providers_need_a_key_and_local_ones_do_not() {
        assert!(provider("anthropic").unwrap().needs_api_key);
        assert!(provider("openai").unwrap().needs_api_key);
        assert!(!provider("ollama").unwrap().needs_api_key);
        assert!(!provider("none").unwrap().needs_api_key);
    }

    // -- selection ------------------------------------------------------------

    #[test]
    fn build_backend_picks_the_right_class() {
        // The label is the class: the OpenAI-compatible backend answers with
        // its provider's name, the other two with their own.
        for (backend, expected) in [
            ("ollama", "Ollama"),
            ("anthropic", "Anthropic"),
            ("openai", "OpenAI"),
            ("openrouter", "OpenRouter"),
            ("deepseek", "DeepSeek"),
            ("custom", "Other (OpenAI-compatible)"),
            ("none", "Ollama"),
        ] {
            assert_eq!(build_backend(&cfg(backend, "x")).label(), expected, "{backend}");
        }
    }

    #[test]
    fn an_empty_address_does_not_shadow_the_providers_own() {
        for (backend, url) in [
            ("openrouter", Some("https://openrouter.ai/api/v1")),
            ("deepseek", Some("https://api.deepseek.com")),
            ("openai", None),
        ] {
            assert_eq!(OpenAICompatibleBackend::new("x", backend, Some("")).base_url(), url, "{backend}");
        }
    }

    #[test]
    fn switching_away_from_custom_leaves_its_address_behind() {
        // base_url survives in the file after the switch. Honouring it would
        // send the OpenAI, DeepSeek or OpenRouter key to the old custom host.
        let stale = "https://old-custom.example/v1";
        for (backend, url) in [
            ("openrouter", Some("https://openrouter.ai/api/v1")),
            ("deepseek", Some("https://api.deepseek.com")),
            ("openai", None),
            ("custom", Some(stale)),
        ] {
            let config = CleanupConfig { base_url: stale.into(), ..cfg(backend, "x") };
            assert_eq!(openai_compatible(&config).base_url(), url, "{backend}");
        }
    }

    #[test]
    fn timeouts_from_the_config_cannot_panic() {
        // TOML can spell inf and nan, and thinking multiplies by four.
        assert_eq!(timeout(f32::INFINITY), Duration::from_secs(600));
        assert_eq!(timeout(f32::NAN), Duration::from_secs(600));
        assert_eq!(timeout(f32::MAX * 4.0), Duration::from_secs(600));
        assert_eq!(timeout(f32::NEG_INFINITY), Duration::from_secs(1));
        assert_eq!(timeout(-5.0), Duration::from_secs(1));
        assert_eq!(timeout(0.0), Duration::from_secs(1));
        assert_eq!(timeout(20.0), Duration::from_secs(20));
        // And through a real backend: fails cleanly, does not panic.
        let backend = OllamaBackend::new("m", "http://127.0.0.1:1", "1h");
        assert!(backend.complete("s", "p", f32::INFINITY, true).is_err());
    }

    #[test]
    fn custom_provider_uses_the_configured_address() {
        let backend = OpenAICompatibleBackend::new("m", "custom", Some("http://127.0.0.1:8000/v1"));
        assert_eq!(backend.base_url(), Some("http://127.0.0.1:8000/v1"));
    }

    #[test]
    fn a_custom_endpoint_works_without_a_key() {
        // llama.cpp, LM Studio and vLLM run without one unless told otherwise.
        let server = MockServer::start();
        let no_auth = |req: &HttpMockRequest| {
            req.headers
                .as_ref()
                .is_none_or(|h| !h.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization")))
        };
        let chat = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions").matches(no_auth);
            then.status(200).json_body(json!({"choices": [{"message": {"content": "Hi there."}}]}));
        });
        let probe = server.mock(|when, then| {
            when.method(GET).path("/v1/models/local").matches(no_auth);
            then.status(200).json_body(json!({"id": "local"}));
        });
        let backend =
            OpenAICompatibleBackend::new("local", "custom", Some(&server.url("/v1"))).with_api_key("");
        assert_eq!(backend.available(), (true, "ok".to_string()));
        assert_eq!(backend.complete("s", "hi there", 5.0, false).unwrap(), "Hi there.");
        chat.assert();
        probe.assert();
    }

    #[test]
    fn custom_provider_without_an_address_says_so() {
        let backend = OpenAICompatibleBackend::new("m", "custom", None).with_api_key("sk-test");
        let (ok, why) = backend.available();
        assert!(!ok && why.contains("base URL"), "{why}");
        let err = backend.complete("s", "p", 5.0, false).unwrap_err();
        assert!(err.to_string().contains("base URL"), "{err}");
    }

    #[test]
    fn hosted_backends_fail_clearly_without_a_key() {
        // A missing key must say so, not surface as a timeout or a stack trace.
        let backends: Vec<Box<dyn Backend>> = vec![
            Box::new(AnthropicBackend::new("claude-opus-5").with_api_key("")),
            Box::new(OpenAICompatibleBackend::new("gpt-5", "openai", None).with_api_key("")),
            Box::new(
                OpenAICompatibleBackend::new("deepseek/deepseek-chat", "openrouter", None).with_api_key(""),
            ),
            Box::new(OpenAICompatibleBackend::new("deepseek-chat", "deepseek", None).with_api_key("")),
        ];
        for backend in backends {
            let (ok, why) = backend.available();
            assert!(!ok && why.to_lowercase().contains("key"), "{why}");
            let err = backend.complete("sys", "prompt", 5.0, false).unwrap_err();
            assert!(err.to_string().contains("API key"), "{err}");
        }
    }

    #[test]
    fn unreachable_ollama_reports_unavailable() {
        // Port 1 is reserved and nothing listens there.
        let (ok, why) = OllamaBackend::new("m", "http://127.0.0.1:1", "1h").available();
        assert!(!ok && why.contains("unreachable"), "{why}");
    }

    #[test]
    fn hosted_backends_are_always_warm() {
        assert_eq!(AnthropicBackend::new("claude-opus-5").warm_up(), 0.0);
        assert_eq!(OpenAICompatibleBackend::new("gpt-5", "openai", None).warm_up(), 0.0);
        assert_eq!(OpenAICompatibleBackend::new("x", "openrouter", None).warm_up(), 0.0);
    }

    #[test]
    fn error_messages_name_the_provider() {
        // "OpenAI rejected the key" when you are on OpenRouter sends you to
        // the wrong dashboard.
        let err = OpenAICompatibleBackend::new("x", "openrouter", None)
            .with_api_key("")
            .complete("s", "p", 5.0, false)
            .unwrap_err();
        assert!(err.to_string().contains("OpenRouter"), "{err}");
        let err = OpenAICompatibleBackend::new("x", "deepseek", None)
            .with_api_key("")
            .complete("s", "p", 5.0, false)
            .unwrap_err();
        assert!(err.to_string().contains("DeepSeek"), "{err}");
    }

    #[test]
    fn cleaner_failure_falls_back_to_the_raw_transcript() {
        // Switching to a cloud provider must not risk losing a dictation when
        // the network is down.
        use crate::cleanup::{Cleaner, ModelCleaner};
        struct Broken;
        impl Backend for Broken {
            fn complete(&self, _: &str, _: &str, _: f32, _: bool) -> Result<String, BackendError> {
                Err(BackendError("no network".into()))
            }
            fn available(&self) -> (bool, String) {
                (false, "no network".into())
            }
            fn label(&self) -> String {
                "Broken".into()
            }
        }
        let cleaner = ModelCleaner::new(&CleanupConfig::default(), Box::new(Broken));
        assert_eq!(cleaner.clean("um hello there", &Default::default()), "um hello there");
    }

    // -- model listing --------------------------------------------------------
    // A provider's catalogue is not a list of things that can clean up a
    // transcript. Offering an embedding model as the cleanup model is a runtime
    // error the user cannot diagnose from the dropdown.

    #[test]
    fn openai_non_chat_models_are_excluded() {
        let catalogue = strings(&[
            "gpt-5",
            "gpt-5-mini",
            "gpt-4.1",
            "o3",
            "text-embedding-3-large",
            "tts-1-hd",
            "whisper-1",
            "dall-e-3",
            "omni-moderation-latest",
            "gpt-4o-audio-preview",
            "gpt-4o-realtime-preview",
            "gpt-image-1",
            "babbage-002",
            "davinci-002",
            "computer-use-preview",
            "codex-mini-latest",
            "gpt-4o-transcribe",
            "sora-2",
        ]);
        assert_eq!(
            usable_chat_models(&catalogue, "openai"),
            strings(&["gpt-4.1", "gpt-5", "gpt-5-mini", "o3"])
        );
    }

    #[test]
    fn batch_variants_are_excluded_for_every_provider() {
        let ids = strings(&["deepseek/deepseek-v4-pro", "deepseek/deepseek-v4-pro:batch"]);
        assert_eq!(usable_chat_models(&ids, "openrouter"), strings(&["deepseek/deepseek-v4-pro"]));
        assert_eq!(usable_chat_models(&ids, "custom"), strings(&["deepseek/deepseek-v4-pro"]));
    }

    #[test]
    fn free_variants_are_kept() {
        let ids = strings(&["qwen/qwen3-14b:free", "qwen/qwen3-14b"]);
        let mut sorted = ids.clone();
        sorted.reverse();
        assert_eq!(usable_chat_models(&ids, "openrouter"), sorted);
    }

    #[test]
    fn routers_keep_models_whose_names_look_like_openai_extras() {
        // The OpenAI exclusions are name-based, so they must not be applied
        // to a router where "openai/gpt-4o-audio" is simply another routed
        // model.
        let ids = strings(&["openai/gpt-4o-audio-preview", "openai/gpt-5"]);
        assert_eq!(usable_chat_models(&ids, "openrouter"), ids);
    }

    #[test]
    fn listing_is_sorted_so_vendors_group_together() {
        let ids = strings(&["z-ai/glm", "anthropic/claude-opus-5", "deepseek/x"]);
        assert_eq!(
            usable_chat_models(&ids, "openrouter"),
            strings(&["anthropic/claude-opus-5", "deepseek/x", "z-ai/glm"])
        );
    }

    #[test]
    fn openrouter_catalogue_is_declared_public() {
        // It is fetchable without a key, which is what lets the dropdown fill
        // before the user has pasted one.
        assert!(provider("openrouter").unwrap().public_models_url.is_some());
        assert_eq!(provider("openai").unwrap().public_models_url, None);
        assert_eq!(provider("anthropic").unwrap().public_models_url, None);
    }

    #[test]
    fn public_listing_needs_no_api_key() {
        let server = MockServer::start();
        let catalogue = server.mock(|when, then| {
            when.method(GET).path("/public/models").matches(|req| req.headers.as_ref().is_none_or(|h| !h.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization"))));
            then.status(200).json_body(json!({"data": [{"id": "b/model"}, {"id": "a/model"}, {"id": "a/model:batch"}, {"name": "no id"}]}));
        });
        let backend = OpenAICompatibleBackend::new("x", "openrouter", None)
            .with_api_key("")
            .with_public_models_url(&server.url("/public/models"));
        assert_eq!(backend.installed_models(), strings(&["a/model", "b/model"]));
        catalogue.assert();
    }

    #[test]
    fn listing_never_raises_when_a_provider_is_unreachable() {
        // No key, no network: an empty list, not an exception into the UI
        // thread.
        assert_eq!(
            OpenAICompatibleBackend::new("x", "openai", None).with_api_key("").installed_models(),
            Vec::<String>::new()
        );
        assert_eq!(AnthropicBackend::new("x").with_api_key("").installed_models(), Vec::<String>::new());
        // A key but a dead server: still an empty list.
        let openai =
            OpenAICompatibleBackend::new("x", "custom", Some("http://127.0.0.1:1/v1")).with_api_key("k");
        assert_eq!(openai.installed_models(), Vec::<String>::new());
        let anthropic = AnthropicBackend::new("x").with_api_key("k").with_base_url("http://127.0.0.1:1");
        assert_eq!(anthropic.installed_models(), Vec::<String>::new());
    }

    // -- the wire format, against mocked endpoints ----------------------------

    #[test]
    fn ollama_complete_posts_the_documented_payload() {
        let server = MockServer::start();
        let generate = server.mock(|when, then| {
            when.method(POST).path("/api/generate").json_body_partial(
                r#"{"model": "qwen3:14b", "system": "sys", "prompt": "hi there", "stream": false, "think": true, "keep_alive": "1h", "options": {"temperature": 0.1}}"#,
            );
            then.status(200).json_body(json!({"response": "Hi there."}));
        });
        let backend = OllamaBackend::new("qwen3:14b", &format!("{}/", server.base_url()), "1h");
        assert_eq!(backend.complete("sys", "hi there", 5.0, true).unwrap(), "Hi there.");
        generate.assert();
    }

    #[test]
    fn ollama_complete_surfaces_a_server_error() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/api/generate").body_contains("\"prompt\":\"p\"");
            then.status(500).body("boom");
        });
        let backend = OllamaBackend::new("m", &server.base_url(), "1h");
        let err = backend.complete("s", "p", 5.0, false).unwrap_err();
        assert!(err.to_string().contains("500"), "{err}");
        // A reply with no "response" field is an empty completion, not a crash.
        server.mock(|when, then| {
            when.method(POST).path("/api/generate").body_contains("\"prompt\":\"empty\"");
            then.status(200).json_body(json!({"done": true}));
        });
        assert_eq!(backend.complete("s", "empty", 5.0, false).unwrap(), "");
    }

    #[test]
    fn ollama_availability_matches_bare_and_tagged_names() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/api/tags");
            then.status(200).json_body(json!({"models": [{"name": "qwen3:14b"}, {"name": "llama3.1:8b"}]}));
        });
        let url = server.base_url();
        assert_eq!(OllamaBackend::new("qwen3:14b", &url, "1h").available(), (true, "ok".into()));
        assert_eq!(OllamaBackend::new("qwen3", &url, "1h").available(), (true, "ok".into()));
        let (ok, why) = OllamaBackend::new("phi4", &url, "1h").available();
        assert!(!ok);
        assert_eq!(why, "model \"phi4\" not pulled (have: qwen3:14b, llama3.1:8b)");
        assert_eq!(
            OllamaBackend::new("qwen3", &url, "1h").installed_models(),
            strings(&["llama3.1:8b", "qwen3:14b"])
        );
    }

    #[test]
    fn ollama_reports_an_empty_catalogue_as_none() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/api/tags");
            then.status(200).json_body(json!({"models": []}));
        });
        let (ok, why) = OllamaBackend::new("qwen3", &server.base_url(), "1h").available();
        assert!(!ok);
        assert_eq!(why, "model \"qwen3\" not pulled (have: none)");
    }

    #[test]
    fn ollama_warm_up_and_unload_post_the_documented_payloads() {
        let server = MockServer::start();
        let warm = server.mock(|when, then| {
            when.method(POST).path("/api/generate").json_body_partial(
                r#"{"model": "m", "prompt": "hi", "stream": false, "think": false, "keep_alive": "30m", "options": {"num_predict": 1}}"#,
            );
            then.status(200).json_body(json!({"response": ""}));
        });
        let unload = server.mock(|when, then| {
            when.method(POST).path("/api/generate").json_body_partial(r#"{"model": "m", "keep_alive": 0}"#);
            then.status(200).json_body(json!({"done": true}));
        });
        let backend = OllamaBackend::new("m", &server.base_url(), "30m");
        assert!(backend.warm_up() > 0.0);
        backend.unload();
        warm.assert();
        unload.assert();
        // A dead server warms nothing and says so with 0.0.
        assert_eq!(OllamaBackend::new("m", "http://127.0.0.1:1", "1h").warm_up(), 0.0);
    }

    #[test]
    fn anthropic_complete_joins_text_blocks_and_sets_effort() {
        let server = MockServer::start();
        let low = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/messages")
                .header("x-api-key", "sk-ant")
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json_body_partial(
                    r#"{"model": "claude-opus-5", "max_tokens": 4096, "system": "sys", "messages": [{"role": "user", "content": "hello"}], "output_config": {"effort": "low"}}"#,
                );
            then.status(200).json_body(json!({
                "stop_reason": "end_turn",
                "content": [
                    {"type": "thinking", "thinking": "hmm"},
                    {"type": "text", "text": "Hello"},
                    {"type": "text", "text": " there."}
                ]
            }));
        });
        let high = server.mock(|when, then| {
            when.method(POST).path("/v1/messages").json_body_partial(r#"{"output_config": {"effort": "high"}}"#);
            then.status(200).json_body(json!({"stop_reason": "end_turn", "content": [{"type": "text", "text": "Thought about it."}]}));
        });
        let backend =
            AnthropicBackend::new("claude-opus-5").with_api_key("sk-ant").with_base_url(&server.base_url());
        assert_eq!(backend.complete("sys", "hello", 5.0, false).unwrap(), "Hello there.");
        assert_eq!(backend.complete("sys", "hello", 5.0, true).unwrap(), "Thought about it.");
        low.assert();
        high.assert();
    }

    #[test]
    fn anthropic_refusal_and_status_errors_are_labelled() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages").body_contains("refuse");
            then.status(200).json_body(json!({"stop_reason": "refusal", "content": []}));
        });
        for (marker, status) in [("unauthorised", 401), ("throttled", 429), ("broken", 503)] {
            server.mock(move |when, then| {
                when.method(POST).path("/v1/messages").body_contains(marker);
                then.status(status).json_body(json!({"error": {"message": marker}}));
            });
        }
        let backend =
            AnthropicBackend::new("claude-opus-5").with_api_key("sk-ant").with_base_url(&server.base_url());
        let message = |prompt: &str| backend.complete("s", prompt, 5.0, false).unwrap_err().to_string();
        assert_eq!(message("refuse"), "Anthropic declined to process this transcript");
        assert_eq!(message("unauthorised"), "Anthropic rejected the API key");
        assert_eq!(message("throttled"), "Anthropic rate limit reached");
        assert_eq!(message("broken"), "Anthropic error 503");

        let dead =
            AnthropicBackend::new("claude-opus-5").with_api_key("sk-ant").with_base_url("http://127.0.0.1:1");
        assert_eq!(dead.complete("s", "p", 5.0, false).unwrap_err().to_string(), "could not reach Anthropic");
    }

    #[test]
    fn anthropic_available_and_installed_models() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v1/models/claude-opus-5").header("x-api-key", "sk-ant");
            then.status(200).json_body(json!({"id": "claude-opus-5", "type": "model"}));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v1/models/nope");
            then.status(404)
                .json_body(json!({"error": {"type": "not_found_error", "message": "model: nope"}}));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v1/models").query_param("limit", "1000");
            then.status(200).json_body(json!({"data": [{"id": "claude-sonnet-5"}, {"id": "claude-haiku-4-5"}, {"id": "claude-opus-5"}]}));
        });
        let backend =
            AnthropicBackend::new("claude-opus-5").with_api_key("sk-ant").with_base_url(&server.base_url());
        assert_eq!(backend.available(), (true, "ok".into()));
        assert_eq!(
            backend.installed_models(),
            strings(&["claude-haiku-4-5", "claude-opus-5", "claude-sonnet-5"])
        );

        let missing = AnthropicBackend::new("nope").with_api_key("sk-ant").with_base_url(&server.base_url());
        let (ok, why) = missing.available();
        assert!(!ok);
        assert!(why.starts_with("NotFoundError: Error code: 404"), "{why}");
        assert!(why.len() <= "NotFoundError: ".len() + 60, "{why}");
    }

    #[test]
    fn openai_complete_reads_the_first_choice_and_trims() {
        let server = MockServer::start();
        let chat = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .header("authorization", "Bearer sk-oai")
                .json_body_partial(
                    r#"{"model": "gpt-5", "max_completion_tokens": 4096, "messages": [{"role": "system", "content": "sys"}, {"role": "user", "content": "hello"}]}"#,
                );
            then.status(200).json_body(json!({"choices": [{"message": {"role": "assistant", "content": "  Hello there.\n"}}]}));
        });
        let backend =
            OpenAICompatibleBackend::new("gpt-5", "custom", Some(&server.url("/v1"))).with_api_key("sk-oai");
        assert_eq!(backend.complete("sys", "hello", 5.0, false).unwrap(), "Hello there.");
        chat.assert();

        // A null content (reasoning-only reply) is an empty answer, not a crash.
        server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions").body_contains("nothing");
            then.status(200)
                .json_body(json!({"choices": [{"message": {"role": "assistant", "content": null}}]}));
        });
        assert_eq!(backend.complete("sys", "nothing", 5.0, false).unwrap(), "");
    }

    #[test]
    fn openai_status_errors_are_labelled_with_the_provider() {
        let server = MockServer::start();
        for (marker, status) in [("unauthorised", 401), ("throttled", 429), ("missing", 404), ("broken", 502)]
        {
            server.mock(move |when, then| {
                when.method(POST).path("/v1/chat/completions").body_contains(marker);
                then.status(status).json_body(json!({"error": {"message": marker}}));
            });
        }
        let backend =
            OpenAICompatibleBackend::new("gpt-5", "openai", Some(&server.url("/v1"))).with_api_key("sk");
        let message = |prompt: &str| backend.complete("s", prompt, 5.0, false).unwrap_err().to_string();
        assert_eq!(message("unauthorised"), "OpenAI rejected the API key");
        assert_eq!(message("throttled"), "OpenAI rate limit reached");
        assert_eq!(message("missing"), "OpenAI has no model \"gpt-5\"");
        assert_eq!(message("broken"), "OpenAI error 502");

        let dead =
            OpenAICompatibleBackend::new("x", "deepseek", Some("http://127.0.0.1:1")).with_api_key("sk");
        assert_eq!(dead.complete("s", "p", 5.0, false).unwrap_err().to_string(), "could not reach DeepSeek");
    }

    #[test]
    fn openai_available_and_keyed_listing() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/v1/models/gpt-5").header("authorization", "Bearer sk");
            then.status(200).json_body(json!({"id": "gpt-5", "object": "model"}));
        });
        server.mock(|when, then| {
            when.method(GET).path("/v1/models").header("authorization", "Bearer sk");
            then.status(200)
                .json_body(json!({"data": [{"id": "gpt-5"}, {"id": "whisper-1"}, {"id": "gpt-4.1"}]}));
        });
        let backend =
            OpenAICompatibleBackend::new("gpt-5", "openai", Some(&server.url("/v1"))).with_api_key("sk");
        assert_eq!(backend.available(), (true, "ok".into()));
        // No public catalogue for OpenAI, so the keyed listing is used and
        // filtered.
        assert_eq!(backend.installed_models(), strings(&["gpt-4.1", "gpt-5"]));

        let wrong_key =
            OpenAICompatibleBackend::new("gpt-5", "openai", Some(&server.url("/v1"))).with_api_key("bad");
        let (ok, why) = wrong_key.available();
        assert!(!ok);
        assert!(why.starts_with("NotFoundError: ") || why.starts_with("AuthenticationError: "), "{why}");
    }
}
